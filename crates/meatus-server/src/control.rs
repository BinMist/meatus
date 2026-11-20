#![forbid(unsafe_code)]
//! 控制面会话：HMAC 挑战应答、代理注册、心跳与 TCP/UDP 数据流桥接。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use meatus_core::{
    ClientPolicy, ConnectionPool, MultiplexedStream, PortReleasePolicy, QuotaAction, RateLimiter,
    TargetPolicy, UsageRecord, WireError, copy_bidirectional_limited, read_frame, write_frame,
};
use meatus_proto::{
    CAP_PROBE, ErrorCode, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, negotiate,
    v1::{
        AdminAddProxy, AdminRemoveProxy, Challenge, ControlMsg, Hello, NewProxy, Ping, Pong, Probe,
        ProbeResult, ProxyOk, ProxyType, StreamHeader, StreamKind, UdpPacket, Welcome, control_msg,
    },
};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval, interval_at, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::ServerState;
use crate::admin_bus::AdminProxyCommand;
use crate::handshake::HANDSHAKE_FAIL_DELAY;
use crate::metrics::ProxyMetrics;
use crate::quota::ProxyQuota;
use crate::routes::{DomainRoute, RouteKind};
use crate::socks5::Socks5Route;
use crate::status::ProxyEntry;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_PING: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(25);
const IDLE_CHECK: Duration = Duration::from_secs(5);
const READ_SLICE: Duration = Duration::from_secs(60);
const UDP_MAX_PACKET: usize = 1500;
const UDP_DEFAULT_IDLE: Duration = Duration::from_secs(60);
const UDP_MAX_SESSIONS: usize = 2048;
const UDP_CLEANUP: Duration = Duration::from_secs(30);
const ADMIN_ACK_TIMEOUT: Duration = Duration::from_secs(5);

type Reader = ReadHalf<MultiplexedStream>;
type Writer = WriteHalf<MultiplexedStream>;

pub struct SessionContext {
    pub state: Arc<ServerState>,
    pub open: meatus_core::OpenHandle,
    pub cancel: CancellationToken,
    pub closed: CancellationToken,
    pub peer: std::net::IpAddr,
}

struct ProxyRuntime {
    name: String,
    port: Option<u16>,
    task: Option<JoinHandle<()>>,
    allocator: Option<Arc<dyn meatus_core::NameAllocator>>,
    tenant_id: String,
    client_id: String,
    run_id: String,
    domains: Vec<String>,
    spec: NewProxy,
    state: Arc<ServerState>,
}

impl Drop for ProxyRuntime {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let hold = self.state.config.ports.release_policy == PortReleasePolicy::Hold;
        if let (Some(port), Some(allocator)) = (self.port, self.allocator.as_ref()) {
            if hold {
                let allocator = Arc::clone(allocator);
                let tenant_id = self.tenant_id.clone();
                let proxy_name = self.name.clone();
                let status = self.state.status.clone();
                let hold = Duration::from_secs(self.state.config.ports.hold_secs);
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        tokio::time::sleep(hold).await;
                        // 原 client 在保留期内已重连并重新占用同名 proxy 时，
                        // 不能由旧租约的延迟任务释放新租约。
                        if status
                            .proxy_entries(&proxy_name, None)
                            .iter()
                            .any(|entry| entry.tenant_id == tenant_id)
                        {
                            return;
                        }
                        allocator.release_lease(&tenant_id, &proxy_name, port);
                    });
                } else {
                    allocator.release_lease(&self.tenant_id, &self.name, port);
                }
            } else {
                allocator.release_lease(&self.tenant_id, &self.name, port);
            }
        }
        self.state
            .socks5
            .unregister(&self.name, &self.tenant_id, &self.client_id, &self.run_id);
        self.state.audit.record(
            crate::audit::AuditEvent::new("proxy.unregister", &self.name)
                .actor(format!("client:{}", self.client_id))
                .client(&self.client_id),
        );
        self.state.status.unregister_proxy(
            &self.tenant_id,
            &self.client_id,
            &self.run_id,
            &self.name,
        );
        let released =
            self.state
                .domains
                .unregister_proxy(&self.tenant_id, &self.client_id, &self.name);
        // 域名离线标记独立于端口释放策略：即使端口已立即释放，保留期内
        // 仍对 HTTP 请求返回 503，避免被误判为“域名从未存在”的 404。
        if !released.is_empty() {
            self.state.domains.mark_offline(
                &released,
                Duration::from_secs(self.state.config.ports.hold_secs),
            );
        }
        if let Some(acme) = &self.state.acme {
            for domain in released {
                acme.remove_domain(&domain);
            }
        }
        debug!(proxy = %self.name, port = ?self.port, "proxy released");
    }
}

#[derive(Clone)]
struct ProxyContext {
    state: Arc<ServerState>,
    pool: Arc<ConnectionPool>,
    tenant_id: String,
    client_id: String,
    proxy_name: String,
    limiter: Arc<RateLimiter>,
    conn_limiter: Arc<RateLimiter>,
    quota: Arc<ProxyQuota>,
    metrics: Arc<ProxyMetrics>,
    next_trace: Arc<AtomicU64>,
}

impl ProxyContext {
    fn allow_connection(&self) -> bool {
        self.conn_limiter.try_consume(1) && self.quota.allow_connection()
    }

    fn record_usage(&self, bytes_in: u64, bytes_out: u64) {
        self.quota.record(bytes_in, bytes_out);
    }
}

async fn serve_data_connection(
    hello: &Hello,
    writer: &mut Writer,
    ctx: &SessionContext,
    negotiated: &meatus_proto::Negotiated,
) -> anyhow::Result<()> {
    if hello.session_id.trim().is_empty() {
        let _ = write_frame(
            writer,
            &ErrorCode::AuthFailed.to_wire("data connection requires session_id"),
        )
        .await;
        bail!("data connection missing session_id");
    }
    let (pool, id, session_cancel) = ctx
        .state
        .pools
        .add_data(
            &hello.session_id,
            &hello.tenant_id,
            &hello.client_id,
            ctx.open.clone(),
        )
        .map_err(|err| anyhow!("register data connection: {err}"))?;
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Welcome(Welcome {
                run_id: hello.session_id.clone(),
                heartbeat_secs: SERVER_PING.as_secs() as u32,
                server_time_ms: now_ms(),
                protocol_version: negotiated.version,
                caps: negotiated.caps.clone(),
                trace_id: hello.trace_id.clone(),
            })),
        },
    )
    .await?;
    info!(
        trace_id = %hello.trace_id,
        client_id = %hello.client_id,
        session_id = %hello.session_id,
        pool_size = pool.len(),
        "data connection registered"
    );
    tokio::select! {
        _ = ctx.closed.cancelled() => {}
        _ = session_cancel.cancelled() => {}
        _ = ctx.cancel.cancelled() => {}
    }
    pool.remove(id);
    Ok(())
}

pub async fn serve(control: MultiplexedStream, ctx: SessionContext) -> anyhow::Result<()> {
    let handshake_complete = AtomicBool::new(false);
    let result = run_session(control, &ctx, &handshake_complete).await;
    if !handshake_complete.load(Ordering::Relaxed) {
        // 握手失败统一延迟 3s 再断开，降低在线枚举与撞库速度。
        tokio::time::sleep(HANDSHAKE_FAIL_DELAY).await;
    }
    // 控制面结束即停止该会话的所有监听与转发任务。
    ctx.cancel.cancel();
    result
}

async fn run_session(
    control: MultiplexedStream,
    ctx: &SessionContext,
    handshake_complete: &AtomicBool,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(control);

    let hello = match read_hello(&mut reader).await {
        Ok(hello) => hello,
        Err(err) => {
            ctx.state.metrics.handshake_failed("hello");
            return Err(err);
        }
    };
    let negotiated = match negotiate(hello.protocol_version, &hello.caps) {
        Ok(negotiated) => negotiated,
        Err(err) => {
            ctx.state.metrics.handshake_failed("protocol");
            let _ = write_frame(
                &mut writer,
                &ErrorCode::UnsupportedVersion.to_wire(format!(
                    "server supports protocol {MIN_PROTOCOL_VERSION}-{PROTOCOL_VERSION}: {err}"
                )),
            )
            .await;
            bail!("unsupported protocol version {}", hello.protocol_version);
        }
    };
    info!(
        client_protocol = hello.protocol_version,
        protocol = negotiated.version,
        client_id = %hello.client_id,
        caps = ?negotiated.caps,
        ignored_caps = ?negotiated.ignored_caps,
        "protocol negotiated"
    );
    if hello.client_id.trim().is_empty() || hello.tenant_id.trim().is_empty() {
        ctx.state.metrics.handshake_failed("identity");
        let _ = write_frame(
            &mut writer,
            &ErrorCode::AuthFailed.to_wire("client_id and tenant_id are required"),
        )
        .await;
        bail!("hello missing identity");
    }

    let nonce = random_bytes(32)?;
    write_frame(
        &mut writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Challenge(Challenge {
                nonce: nonce.clone(),
            })),
        },
    )
    .await?;

    let auth = match timeout(HANDSHAKE_TIMEOUT, read_frame::<_, ControlMsg>(&mut reader)).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(err)) => {
            ctx.state.metrics.handshake_failed("auth_io");
            return Err(err).context("read auth frame");
        }
        Err(_) => {
            ctx.state.metrics.handshake_failed("auth_timeout");
            bail!("timed out waiting for Auth");
        }
    };
    let Some(control_msg::Kind::Auth(auth)) = auth.kind else {
        ctx.state.metrics.handshake_failed("auth_protocol");
        let _ = write_frame(&mut writer, &ErrorCode::AuthFailed.to_wire("expected Auth")).await;
        bail!("expected Auth frame");
    };

    let auth_provider = ctx.state.runtime.auth();
    match auth_provider
        .verify_hmac(&hello.tenant_id, &hello.client_id, &nonce, &auth.hmac)
        .await
    {
        Ok(identity) => {
            info!(
                trace_id = %hello.trace_id,
                tenant = %identity.tenant_id,
                client = %identity.client_id,
                "client authenticated"
            );
        }
        Err(err) => {
            ctx.state.metrics.handshake_failed("auth");
            let _ = write_frame(
                &mut writer,
                &ErrorCode::AuthFailed.to_wire("authentication failed"),
            )
            .await;
            bail!("authentication failed: {err}");
        }
    }

    if hello.data_connection {
        return serve_data_connection(&hello, &mut writer, ctx, &negotiated).await;
    }

    let client_policy = auth_provider.client_policy(&hello.tenant_id, &hello.client_id);

    let run_id = hex::encode(random_bytes(16)?);
    write_frame(
        &mut writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Welcome(Welcome {
                run_id: run_id.clone(),
                heartbeat_secs: SERVER_PING.as_secs() as u32,
                server_time_ms: now_ms(),
                protocol_version: negotiated.version,
                caps: negotiated.caps.clone(),
                trace_id: hello.trace_id.clone(),
            })),
        },
    )
    .await?;
    handshake_complete.store(true, Ordering::Relaxed);
    ctx.state
        .pools
        .create(
            &run_id,
            &hello.tenant_id,
            &hello.client_id,
            ctx.open.clone(),
            hello.pool_size,
            hello.max_streams_per_conn,
        )
        .map_err(|err| anyhow!("create session pool: {err}"))?;
    ctx.state.status.client_connected(
        &hello.tenant_id,
        &hello.client_id,
        &run_id,
        &negotiated.caps,
    );
    ctx.state.audit.record(
        crate::audit::AuditEvent::new("client.online", &hello.client_id)
            .actor(format!("client:{}", hello.client_id))
            .client(&hello.client_id)
            .source_ip(ctx.peer)
            .trace_id(hello.trace_id.clone()),
    );
    struct ClientStatusGuard {
        state: Arc<ServerState>,
        tenant_id: String,
        client_id: String,
        run_id: String,
    }
    impl Drop for ClientStatusGuard {
        fn drop(&mut self) {
            self.state
                .status
                .client_disconnected(&self.tenant_id, &self.client_id, &self.run_id);
            self.state.pools.remove(&self.run_id);
            self.state.audit.record(
                crate::audit::AuditEvent::new("client.offline", &self.client_id)
                    .actor(format!("client:{}", self.client_id))
                    .client(&self.client_id),
            );
        }
    }
    let _client_status = ClientStatusGuard {
        state: Arc::clone(&ctx.state),
        tenant_id: hello.tenant_id.clone(),
        client_id: hello.client_id.clone(),
        run_id: run_id.clone(),
    };

    let (admin_tx, mut admin_rx) = mpsc::channel::<AdminProxyCommand>(8);
    ctx.state.admin_bus.register(&run_id, admin_tx);
    struct AdminBusGuard {
        state: Arc<ServerState>,
        run_id: String,
    }
    impl Drop for AdminBusGuard {
        fn drop(&mut self) {
            self.state.admin_bus.unregister(&self.run_id);
        }
    }
    let _admin_bus_guard = AdminBusGuard {
        state: Arc::clone(&ctx.state),
        run_id: run_id.clone(),
    };

    let mut proxies: HashMap<String, ProxyRuntime> = HashMap::new();
    let mut last_rx = Instant::now();
    let mut next_admin_id = 1u64;
    let mut ping_seq = 0u64;
    let mut ping_timer = interval_at(TokioInstant::now() + SERVER_PING, SERVER_PING);
    ping_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut idle_timer = interval(IDLE_CHECK);
    idle_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let next_trace = Arc::new(AtomicU64::new(1));

    loop {
        tokio::select! {
            _ = ctx.cancel.cancelled() => {
                debug!("control session cancelled");
                break;
            }
            _ = idle_timer.tick() => {
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    bail!("control session idle for {:?}", last_rx.elapsed());
                }
            }
            _ = ping_timer.tick() => {
                ping_seq += 1;
                write_frame(
                    &mut writer,
                    &ControlMsg { kind: Some(control_msg::Kind::Ping(Ping { seq: ping_seq, ts_ms: now_ms() })) },
                ).await?;
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    bail!("control session idle for {:?}", last_rx.elapsed());
                }
            }
            Some(command) = admin_rx.recv() => {
                handle_admin_command(
                    ctx,
                    &mut reader,
                    &mut writer,
                    &mut proxies,
                    command,
                    &hello.tenant_id,
                    &hello.client_id,
                    &run_id,
                    &negotiated.caps,
                    &client_policy,
                    next_trace.clone(),
                    &mut next_admin_id,
                ).await?;
            }
            frame = timeout(READ_SLICE, read_frame::<_, ControlMsg>(&mut reader)) => {
                let msg = match frame {
                    Ok(Ok(msg)) => msg,
                    Ok(Err(WireError::Closed)) => break,
                    Ok(Err(err)) => return Err(err).context("read control frame"),
                    Err(_) => {
                        if last_rx.elapsed() > IDLE_TIMEOUT {
                            bail!("control session idle for {:?}", last_rx.elapsed());
                        }
                        continue;
                    }
                };
                last_rx = Instant::now();
                ctx.state.status.client_heartbeat(
                    &hello.tenant_id,
                    &hello.client_id,
                    &run_id,
                );

                match msg.kind {
                    Some(control_msg::Kind::Ping(ping)) => {
                        write_frame(
                            &mut writer,
                            &ControlMsg { kind: Some(control_msg::Kind::Pong(Pong { seq: ping.seq, ts_ms: now_ms() })) },
                        ).await?;
                    }
                    Some(control_msg::Kind::Pong(_)) => {}
                    Some(control_msg::Kind::NewProxy(request)) => {
                        let name = request.name.clone();
                        if proxies.contains_key(&name) {
                            let _ = write_frame(
                                &mut writer,
                                &ErrorCode::ProxyConflict.to_wire(format!("proxy {name} already registered in this session")),
                            ).await;
                            continue;
                        }
                        match start_proxy(
                            ctx,
                            &hello.tenant_id,
                            &hello.client_id,
                            &run_id,
                            request,
                            &client_policy,
                            false,
                            next_trace.clone(),
                        )
                        .await {
                            Ok(runtime) => {
                                let port = runtime.port.unwrap_or(0);
                                let domains = runtime.domains.clone();
                                proxies.insert(name.clone(), runtime);
                                info!(proxy = %name, port, domains = ?domains, "proxy registered");
                                write_frame(
                                    &mut writer,
                                    &ControlMsg { kind: Some(control_msg::Kind::ProxyOk(ProxyOk {
                                        name,
                                        remote_port: u32::from(port),
                                        domains,
                                    })) },
                                ).await?;
                            }
                            Err((code, message)) => {
                                warn!(proxy = %name, error = %message, "proxy registration failed");
                                let _ = write_frame(&mut writer, &code.to_wire(message)).await;
                            }
                        }
                    }
                    Some(control_msg::Kind::RemoveProxy(remove)) => {
                        if proxies.remove(&remove.name).is_some() {
                            info!(proxy = %remove.name, "proxy removed");
                        }
                    }
                    Some(control_msg::Kind::Stats(stats)) => {
                        ctx.state
                            .metrics
                            .proxy(&stats.proxy_name)
                            .set_local_fail(stats.local_connect_fail);
                    }
                    Some(control_msg::Kind::Error(err)) => {
                        warn!(code = err.code, message = %err.message, "client reported error");
                        if err.fatal {
                            break;
                        }
                    }
                    Some(control_msg::Kind::Hello(_))
                    | Some(control_msg::Kind::Challenge(_))
                    | Some(control_msg::Kind::Auth(_))
                    | Some(control_msg::Kind::Welcome(_))
                    | Some(control_msg::Kind::ProxyOk(_))
                    | Some(control_msg::Kind::AdminAddProxy(_))
                    | Some(control_msg::Kind::AdminRemoveProxy(_))
                    | Some(control_msg::Kind::AdminProxyAck(_))
                    | Some(control_msg::Kind::Probe(_))
                    | Some(control_msg::Kind::ProbeResult(_)) => {
                        debug!("ignoring unexpected control message");
                    }
                    None => {}
                }
            }
        }
    }

    info!(run_id = %run_id, proxies = proxies.len(), "control session ended");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_admin_command(
    ctx: &SessionContext,
    reader: &mut Reader,
    writer: &mut Writer,
    proxies: &mut HashMap<String, ProxyRuntime>,
    command: AdminProxyCommand,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    caps: &[String],
    policy: &ClientPolicy,
    next_trace: Arc<AtomicU64>,
    next_admin_id: &mut u64,
) -> anyhow::Result<()> {
    match command {
        AdminProxyCommand::Set {
            request,
            replace,
            reply,
        } => {
            let result = admin_set_proxy(
                ctx,
                reader,
                writer,
                proxies,
                request,
                replace,
                tenant_id,
                client_id,
                run_id,
                policy,
                next_trace,
                next_admin_id,
            )
            .await;
            let _ = reply.send(result);
        }
        AdminProxyCommand::Remove { name, reply } => {
            let result = admin_remove_proxy(
                ctx,
                reader,
                writer,
                proxies,
                &name,
                tenant_id,
                client_id,
                run_id,
                policy,
                next_trace.clone(),
                next_admin_id,
            )
            .await;
            let _ = reply.send(result);
        }
        AdminProxyCommand::Probe {
            proxy_name,
            timeout_ms,
            reply,
        } => {
            let request_id = *next_admin_id;
            *next_admin_id += 1;
            let result = probe_via_client(
                reader,
                writer,
                proxies,
                &proxy_name,
                timeout_ms,
                caps,
                request_id,
            )
            .await;
            let _ = reply.send(result);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn probe_via_client(
    reader: &mut Reader,
    writer: &mut Writer,
    proxies: &HashMap<String, ProxyRuntime>,
    proxy_name: &str,
    timeout_ms: u32,
    caps: &[String],
    request_id: u64,
) -> Result<ProbeResult, String> {
    if !caps.iter().any(|cap| cap == CAP_PROBE) {
        return Err("当前客户端不支持诊断，请升级客户端到 1.2.1 或更高版本".to_string());
    }
    if !proxies.contains_key(proxy_name) {
        return Err(format!("proxy {proxy_name} not registered"));
    }
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Probe(Probe {
                request_id,
                proxy_name: proxy_name.to_string(),
                timeout_ms,
            })),
        },
    )
    .await
    .map_err(|error| format!("send probe: {error}"))?;

    let wait = Duration::from_millis(u64::from(timeout_ms.clamp(500, 3_000) + 1_000));
    let deadline = TokioInstant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(TokioInstant::now());
        if remaining.is_zero() {
            return Err("probe timeout".to_string());
        }
        let frame = timeout(remaining, read_frame::<_, ControlMsg>(reader)).await;
        match frame {
            Ok(Ok(msg)) => match msg.kind {
                Some(control_msg::Kind::ProbeResult(result)) if result.request_id == request_id => {
                    return Ok(result);
                }
                Some(control_msg::Kind::Ping(ping)) => {
                    write_frame(
                        writer,
                        &ControlMsg {
                            kind: Some(control_msg::Kind::Pong(Pong {
                                seq: ping.seq,
                                ts_ms: now_ms(),
                            })),
                        },
                    )
                    .await
                    .map_err(|error| format!("send pong: {error}"))?;
                }
                Some(control_msg::Kind::Error(err)) if err.fatal => {
                    return Err(format!("client error {}: {}", err.code, err.message));
                }
                Some(_) | None => continue,
            },
            Ok(Err(WireError::Closed)) => {
                return Err("client disconnected during probe".to_string());
            }
            Ok(Err(error)) => return Err(format!("read probe result: {error}")),
            Err(_) => return Err("probe timeout".to_string()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn admin_set_proxy(
    ctx: &SessionContext,
    reader: &mut Reader,
    writer: &mut Writer,
    proxies: &mut HashMap<String, ProxyRuntime>,
    request: NewProxy,
    replace: bool,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    policy: &ClientPolicy,
    next_trace: Arc<AtomicU64>,
    next_admin_id: &mut u64,
) -> Result<ProxyOk, String> {
    let name = request.name.trim().to_string();
    if name.is_empty() {
        return Err("proxy name is required".to_string());
    }
    if proxies.contains_key(&name) && !replace {
        return Err(format!("proxy {name} already registered"));
    }
    if replace && !proxies.contains_key(&name) {
        return Err(format!("proxy {name} is not registered"));
    }

    let mut request = request;
    request.name = name.clone();

    if replace {
        let old = proxies
            .remove(&name)
            .ok_or_else(|| format!("proxy {name} is not registered"))?;
        let old_spec = old.spec.clone();
        let old_port = old.port;
        drop(old);

        if let Err(err) = send_admin_remove(reader, writer, &name, next_admin_id).await {
            let restored = restore_proxy(
                ctx,
                reader,
                writer,
                proxies,
                old_spec,
                old_port,
                tenant_id,
                client_id,
                run_id,
                policy,
                next_trace.clone(),
                next_admin_id,
            )
            .await;
            return Err(format!(
                "client did not remove old proxy: {err}; rollback={restored}"
            ));
        }

        match start_proxy(
            ctx,
            tenant_id,
            client_id,
            run_id,
            request.clone(),
            policy,
            true,
            next_trace.clone(),
        )
        .await
        {
            Ok(runtime) => {
                let port = runtime.port.unwrap_or(0);
                let domains = runtime.domains.clone();
                proxies.insert(name.clone(), runtime);
                if let Err(err) =
                    send_admin_add(reader, writer, &name, &request, next_admin_id).await
                {
                    drop(proxies.remove(&name));
                    let restored = restore_proxy(
                        ctx,
                        reader,
                        writer,
                        proxies,
                        old_spec,
                        old_port,
                        tenant_id,
                        client_id,
                        run_id,
                        policy,
                        next_trace,
                        next_admin_id,
                    )
                    .await;
                    return Err(format!(
                        "client did not add new proxy: {err}; rollback={restored}"
                    ));
                }
                info!(proxy = %name, port, domains = ?domains, "proxy replaced");
                Ok(ProxyOk {
                    name,
                    remote_port: u32::from(port),
                    domains,
                })
            }
            Err((code, message)) => {
                let restored = restore_proxy(
                    ctx,
                    reader,
                    writer,
                    proxies,
                    old_spec,
                    old_port,
                    tenant_id,
                    client_id,
                    run_id,
                    policy,
                    next_trace,
                    next_admin_id,
                )
                .await;
                Err(format!(
                    "replace {name} failed: {code:?}: {message}; rollback={restored}"
                ))
            }
        }
    } else {
        if let Err(err) = send_admin_add(reader, writer, &name, &request, next_admin_id).await {
            return Err(format!("client did not accept proxy: {err}"));
        }
        match start_proxy(
            ctx, tenant_id, client_id, run_id, request, policy, false, next_trace,
        )
        .await
        {
            Ok(runtime) => {
                let port = runtime.port.unwrap_or(0);
                let domains = runtime.domains.clone();
                proxies.insert(name.clone(), runtime);
                info!(proxy = %name, port, domains = ?domains, "proxy added by admin");
                Ok(ProxyOk {
                    name,
                    remote_port: u32::from(port),
                    domains,
                })
            }
            Err((code, message)) => {
                let _ = send_admin_remove(reader, writer, &name, next_admin_id).await;
                Err(format!("add {name} failed: {code:?}: {message}"))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn admin_remove_proxy(
    ctx: &SessionContext,
    reader: &mut Reader,
    writer: &mut Writer,
    proxies: &mut HashMap<String, ProxyRuntime>,
    name: &str,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    policy: &ClientPolicy,
    next_trace: Arc<AtomicU64>,
    next_admin_id: &mut u64,
) -> Result<(), String> {
    let Some(old) = proxies.remove(name) else {
        return Err(format!("proxy {name} is not registered"));
    };
    let old_spec = old.spec.clone();
    let old_port = old.port;
    drop(old);

    let ack = send_admin_remove(reader, writer, name, next_admin_id).await;
    match ack {
        Ok(()) => {
            info!(proxy = %name, "proxy removed by admin");
            Ok(())
        }
        Err(err) => {
            let restored = restore_proxy(
                ctx,
                reader,
                writer,
                proxies,
                old_spec,
                old_port,
                tenant_id,
                client_id,
                run_id,
                policy,
                next_trace,
                next_admin_id,
            )
            .await;
            Err(format!(
                "client did not remove proxy: {err}; rollback={restored}"
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn restore_proxy(
    ctx: &SessionContext,
    reader: &mut Reader,
    writer: &mut Writer,
    proxies: &mut HashMap<String, ProxyRuntime>,
    mut old_spec: NewProxy,
    old_port: Option<u16>,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    policy: &ClientPolicy,
    next_trace: Arc<AtomicU64>,
    next_admin_id: &mut u64,
) -> bool {
    if let Some(port) = old_port {
        old_spec.remote_port = u32::from(port);
    }
    let name = old_spec.name.clone();
    match start_proxy(
        ctx,
        tenant_id,
        client_id,
        run_id,
        old_spec.clone(),
        policy,
        true,
        next_trace,
    )
    .await
    {
        Ok(runtime) => {
            proxies.insert(name.clone(), runtime);
            send_admin_add(reader, writer, &name, &old_spec, next_admin_id)
                .await
                .is_ok()
        }
        Err((code, message)) => {
            warn!(proxy = %name, ?code, error = %message, "proxy rollback failed");
            false
        }
    }
}

async fn send_admin_add(
    reader: &mut Reader,
    writer: &mut Writer,
    name: &str,
    request: &NewProxy,
    next_admin_id: &mut u64,
) -> Result<(), String> {
    let request_id = *next_admin_id;
    *next_admin_id = next_admin_id.wrapping_add(1);
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::AdminAddProxy(AdminAddProxy {
                request_id,
                proxy: Some(request.clone()),
            })),
        },
    )
    .await
    .map_err(|err| format!("write AdminAddProxy {name}: {err}"))?;
    wait_admin_ack(reader, writer, request_id).await
}

async fn send_admin_remove(
    reader: &mut Reader,
    writer: &mut Writer,
    name: &str,
    next_admin_id: &mut u64,
) -> Result<(), String> {
    let request_id = *next_admin_id;
    *next_admin_id = next_admin_id.wrapping_add(1);
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::AdminRemoveProxy(AdminRemoveProxy {
                request_id,
                name: name.to_string(),
            })),
        },
    )
    .await
    .map_err(|err| format!("write AdminRemoveProxy {name}: {err}"))?;
    wait_admin_ack(reader, writer, request_id).await
}

async fn wait_admin_ack(
    reader: &mut Reader,
    writer: &mut Writer,
    request_id: u64,
) -> Result<(), String> {
    let deadline = Instant::now() + ADMIN_ACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out waiting for AdminProxyAck {request_id}"));
        }
        let frame = timeout(remaining, read_frame::<_, ControlMsg>(reader))
            .await
            .map_err(|_| format!("timed out waiting for AdminProxyAck {request_id}"))?
            .map_err(|err| format!("read AdminProxyAck {request_id}: {err}"))?;
        match frame.kind {
            Some(control_msg::Kind::AdminProxyAck(ack)) if ack.request_id == request_id => {
                return if ack.ok {
                    Ok(())
                } else {
                    Err(if ack.error.is_empty() {
                        "client rejected admin proxy command".to_string()
                    } else {
                        ack.error
                    })
                };
            }
            Some(control_msg::Kind::Ping(ping)) => {
                write_frame(
                    writer,
                    &ControlMsg {
                        kind: Some(control_msg::Kind::Pong(Pong {
                            seq: ping.seq,
                            ts_ms: now_ms(),
                        })),
                    },
                )
                .await
                .map_err(|err| format!("write Pong: {err}"))?;
            }
            Some(control_msg::Kind::Error(err)) if err.fatal => {
                return Err(format!("client fatal error {}: {}", err.code, err.message));
            }
            _ => {}
        }
    }
}

async fn read_hello(reader: &mut Reader) -> anyhow::Result<Hello> {
    let frame = timeout(HANDSHAKE_TIMEOUT, read_frame::<_, ControlMsg>(reader))
        .await
        .context("timed out waiting for Hello")??;
    match frame.kind {
        Some(control_msg::Kind::Hello(hello)) => Ok(hello),
        _ => bail!("expected Hello as first control frame"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_proxy(
    ctx: &SessionContext,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    request: NewProxy,
    policy: &ClientPolicy,
    replacing: bool,
    next_trace: Arc<AtomicU64>,
) -> Result<ProxyRuntime, (ErrorCode, String)> {
    let name = request.name.clone();
    let result = start_proxy_inner(
        ctx, tenant_id, client_id, run_id, request, policy, replacing, next_trace,
    )
    .await;
    let mut event = crate::audit::AuditEvent::new("proxy.create", name)
        .actor(format!("client:{client_id}"))
        .client(client_id);
    if let Err((_, message)) = &result {
        event = event.result(
            if message.to_ascii_lowercase().contains("not allowed")
                || message.contains("max_proxies")
                || message.contains("policy")
            {
                "denied"
            } else {
                "error"
            },
        );
    }
    ctx.state.audit.record(event);
    result
}

#[allow(clippy::too_many_arguments)]
async fn start_proxy_inner(
    ctx: &SessionContext,
    tenant_id: &str,
    client_id: &str,
    run_id: &str,
    request: NewProxy,
    policy: &ClientPolicy,
    replacing: bool,
    next_trace: Arc<AtomicU64>,
) -> Result<ProxyRuntime, (ErrorCode, String)> {
    if request.name.trim().is_empty() {
        return Err((ErrorCode::Internal, "proxy name is required".to_string()));
    }
    let claim = ctx
        .state
        .status
        .claim_proxy(tenant_id, client_id, run_id, &request.name)
        .map_err(|message| (ErrorCode::ProxyConflict, message))?;
    let kind = match request.proxy_type {
        value if value == ProxyType::Tcp as i32 => ProxyType::Tcp,
        value if value == ProxyType::Udp as i32 => ProxyType::Udp,
        value if value == ProxyType::Http as i32 => ProxyType::Http,
        value if value == ProxyType::Https as i32 => ProxyType::Https,
        value if value == ProxyType::Socks5 as i32 => ProxyType::Socks5,
        value => {
            return Err((
                ErrorCode::Internal,
                format!("unsupported proxy_type={value}"),
            ));
        }
    };

    if !policy.allows_type(policy_proxy_type(kind)) {
        return Err((
            ErrorCode::PolicyDenied,
            format!(
                "client is not allowed to create {} proxies",
                proxy_type_name(kind)
            ),
        ));
    }
    if let Some(max) = policy.max_proxies
        && !replacing
        && ctx.state.status.client_proxy_count(tenant_id, client_id) >= max as usize
    {
        return Err((
            ErrorCode::PolicyDenied,
            format!("client reached max_proxies={max}"),
        ));
    }

    let options = request.options.clone().unwrap_or_default();
    let rate_limit_bps = effective_u64(options.rate_limit_bps, policy.rate_limit_bps());
    let max_conns = effective_u32(options.max_conns, policy.max_conns());
    let conn_rate_per_s = effective_u32(options.conn_rate_per_s, policy.conn_rate_per_s());
    let monthly_bytes = effective_u64(options.monthly_bytes, policy.monthly_bytes());
    let quota_action = match options.quota_action.as_str() {
        "throttle" => QuotaAction::Throttle,
        "reject" | "" => policy.quota_action(),
        other => return Err((ErrorCode::Internal, format!("unknown quota_action={other}"))),
    };
    let idle_timeout_s = options.idle_timeout_s;
    let pool = ctx
        .state
        .pools
        .get(run_id)
        .ok_or((ErrorCode::Internal, "data pool missing".to_string()))?;
    let limiter = Arc::new(RateLimiter::new(rate_limit_bps));
    let conn_limiter = Arc::new(RateLimiter::new(u64::from(conn_rate_per_s)));
    let metrics = ctx.state.metrics.proxy(&request.name);
    let quota = Arc::new(ProxyQuota::new(
        Arc::clone(&ctx.state.quotas),
        tenant_id,
        client_id,
        &request.name,
        monthly_bytes,
        quota_action,
        Arc::clone(&limiter),
        rate_limit_bps,
        Arc::clone(&metrics),
    ));
    let proxy_ctx = ProxyContext {
        state: Arc::clone(&ctx.state),
        pool: Arc::clone(&pool),
        tenant_id: tenant_id.to_string(),
        client_id: client_id.to_string(),
        proxy_name: request.name.clone(),
        limiter,
        conn_limiter,
        quota,
        metrics,
        next_trace,
    };

    match kind {
        ProxyType::Http | ProxyType::Https => {
            if !ctx.state.config.http.enabled {
                return Err((
                    ErrorCode::Internal,
                    "server [http] is disabled; cannot register http/https proxy".to_string(),
                ));
            }
            if request.remote_port != 0 {
                return Err((
                    ErrorCode::Internal,
                    "http/https proxies do not use remote_port".to_string(),
                ));
            }
            if request.custom_domains.is_empty() {
                return Err((
                    ErrorCode::DomainConflict,
                    "http/https proxy requires custom_domains".to_string(),
                ));
            }
            if !policy.allows_domains(&request.custom_domains) {
                return Err((
                    ErrorCode::PolicyDenied,
                    "custom_domains are not allowed for this client".to_string(),
                ));
            }
            let route_kind = if kind == ProxyType::Http {
                RouteKind::Http
            } else {
                RouteKind::Https
            };
            let route = DomainRoute {
                domain: String::new(),
                kind: route_kind,
                proxy_name: proxy_ctx.proxy_name.clone(),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                pool: Arc::clone(&proxy_ctx.pool),
                limiter: Arc::clone(&proxy_ctx.limiter),
                conn_limiter: Arc::clone(&proxy_ctx.conn_limiter),
                quota: Arc::clone(&proxy_ctx.quota),
                metrics: Arc::clone(&proxy_ctx.metrics),
                next_trace: Arc::clone(&proxy_ctx.next_trace),
                connections: Arc::new(Semaphore::new(if max_conns == 0 {
                    1024
                } else {
                    max_conns as usize
                })),
            };
            ctx.state
                .domains
                .register(&route, &request.custom_domains)
                .map_err(|err| (ErrorCode::DomainConflict, err))?;
            if route_kind == RouteKind::Http
                && let Some(acme) = &ctx.state.acme
            {
                for domain in &request.custom_domains {
                    if !acme.ensure_domain(domain) {
                        warn!(
                            proxy = %request.name,
                            domain = %domain,
                            "acme did not enable tls termination for this domain; falls back to sni"
                        );
                    }
                }
            }
            info!(
                proxy = %request.name,
                client = %client_id,
                domains = ?request.custom_domains,
                "domain proxy registered"
            );
            claim.commit(ProxyEntry {
                name: request.name.clone(),
                proxy_type: proxy_type_name(kind).to_string(),
                remote_port: 0,
                domains: request.custom_domains.clone(),
                local_addr: request.local_addr.clone(),
                rate_limit_bps,
                max_conns,
                monthly_bytes,
                quota_action: quota_action_name(quota_action).to_string(),
                idle_timeout_s,
                allow_cidrs: options.allow_cidrs.clone(),
                allow_domains: options.allow_domains.clone(),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
            });
            Ok(ProxyRuntime {
                spec: request.clone(),
                name: request.name,
                port: None,
                task: None,
                allocator: None,
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
                domains: request.custom_domains,
                state: Arc::clone(&ctx.state),
            })
        }
        ProxyType::Tcp | ProxyType::Udp => {
            if request
                .custom_domains
                .iter()
                .any(|domain| !domain.trim().is_empty())
            {
                return Err((
                    ErrorCode::DomainConflict,
                    "tcp/udp proxies must not set custom_domains".to_string(),
                ));
            }
            if request.remote_port > u32::from(u16::MAX) {
                return Err((
                    ErrorCode::Internal,
                    format!("remote_port {} is out of range", request.remote_port),
                ));
            }
            let preferred = u16::try_from(request.remote_port)
                .ok()
                .filter(|port| *port != 0);
            let port = ctx
                .state
                .allocator
                .alloc_port_in_range(tenant_id, &request.name, preferred, policy.allow_ports)
                .map_err(|err| match err {
                    meatus_core::AllocError::Taken(port) => (
                        ErrorCode::PortInUse,
                        format!("port {port} is already in use"),
                    ),
                    meatus_core::AllocError::Exhausted => (
                        ErrorCode::PortPoolExhausted,
                        "port pool exhausted".to_string(),
                    ),
                    meatus_core::AllocError::OutOfRange(port) => (
                        ErrorCode::PolicyDenied,
                        format!("port {port} is outside the client allow_ports range"),
                    ),
                })?;

            let bind = meatus_core::config::host_port(&ctx.state.config.server.bind_addr, port);
            let task = match kind {
                ProxyType::Tcp => {
                    let listener = match TcpListener::bind(&bind).await {
                        Ok(listener) => listener,
                        Err(err) => {
                            ctx.state.allocator.release_port(tenant_id, port);
                            return Err((ErrorCode::PortInUse, format!("bind {bind}: {err}")));
                        }
                    };
                    info!(proxy = %request.name, client = %client_id, %bind, "public tcp listener bound");
                    let max_conns = if max_conns == 0 {
                        1024
                    } else {
                        max_conns as usize
                    };
                    let semaphore = Arc::new(Semaphore::new(max_conns));
                    let cancel = ctx.cancel.child_token();
                    let proxy_ctx = proxy_ctx;
                    tokio::spawn(async move {
                        tcp_accept_loop(listener, proxy_ctx, semaphore, cancel).await;
                    })
                }
                ProxyType::Udp => {
                    let socket = match UdpSocket::bind(&bind).await {
                        Ok(socket) => socket,
                        Err(err) => {
                            ctx.state.allocator.release_port(tenant_id, port);
                            return Err((ErrorCode::PortInUse, format!("bind {bind}: {err}")));
                        }
                    };
                    info!(proxy = %request.name, client = %client_id, %bind, "public udp listener bound");
                    let (mut tunnel, _guard) = timeout(HANDSHAKE_TIMEOUT, proxy_ctx.pool.open())
                        .await
                        .map_err(|_| {
                            (
                                ErrorCode::Internal,
                                "timed out opening udp tunnel stream".to_string(),
                            )
                        })?
                        .map_err(|err| (ErrorCode::Internal, format!("open udp stream: {err}")))?;
                    let header = StreamHeader {
                        proxy_name: proxy_ctx.proxy_name.clone(),
                        kind: StreamKind::UdpSession as i32,
                        src_addr: bind.clone(),
                        dst_hint: String::new(),
                        created_ms: now_ms(),
                        trace_id: format!(
                            "{:016x}",
                            proxy_ctx.next_trace.fetch_add(1, Ordering::Relaxed)
                        ),
                        udp_session_id: 0,
                        connection_id: meatus_core::ids::new_id(),
                    };
                    proxy_ctx
                        .metrics
                        .record_stream(&header.trace_id, &header.connection_id);
                    write_frame(&mut tunnel, &header)
                        .await
                        .map_err(|err| (ErrorCode::Internal, format!("write udp header: {err}")))?;
                    let max_sessions = options
                        .max_conns
                        .try_into()
                        .ok()
                        .filter(|value| *value > 0)
                        .unwrap_or(UDP_MAX_SESSIONS)
                        .min(UDP_MAX_SESSIONS);
                    let idle = if idle_timeout_s == 0 {
                        UDP_DEFAULT_IDLE
                    } else {
                        Duration::from_secs(u64::from(idle_timeout_s).clamp(15, 3600))
                    };
                    let cancel = ctx.cancel.child_token();
                    let proxy_ctx = proxy_ctx;
                    tokio::spawn(async move {
                        udp_proxy_loop(socket, tunnel, proxy_ctx, cancel, max_sessions, idle).await;
                    })
                }
                _ => unreachable!(),
            };

            claim.commit(ProxyEntry {
                name: request.name.clone(),
                proxy_type: proxy_type_name(kind).to_string(),
                remote_port: port,
                domains: Vec::new(),
                local_addr: request.local_addr.clone(),
                rate_limit_bps,
                max_conns,
                monthly_bytes,
                quota_action: quota_action_name(quota_action).to_string(),
                idle_timeout_s,
                allow_cidrs: options.allow_cidrs.clone(),
                allow_domains: options.allow_domains.clone(),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
            });
            Ok(ProxyRuntime {
                spec: request.clone(),
                name: request.name,
                port: Some(port),
                task: Some(task),
                allocator: Some(ctx.state.allocator.clone()),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
                domains: Vec::new(),
                state: Arc::clone(&ctx.state),
            })
        }
        ProxyType::Socks5 => {
            if !ctx.state.config.socks5.enabled {
                return Err((
                    ErrorCode::Internal,
                    "server [socks5] is disabled; cannot register socks5 proxy".to_string(),
                ));
            }
            if request.remote_port != 0 {
                return Err((
                    ErrorCode::Internal,
                    "socks5 proxies do not use remote_port".to_string(),
                ));
            }
            if !request.custom_domains.is_empty() {
                return Err((
                    ErrorCode::DomainConflict,
                    "socks5 proxies must not set custom_domains".to_string(),
                ));
            }
            let policy = TargetPolicy::from_allow_cidrs(&options.allow_cidrs)
                .map_err(|err| (ErrorCode::Internal, err))?;
            let domain_policy =
                meatus_core::DomainPolicy::from_allow_domains(&options.allow_domains)
                    .map_err(|err| (ErrorCode::Internal, err))?;
            let max_conns_usize = if max_conns == 0 {
                1024
            } else {
                max_conns as usize
            };
            let route = Socks5Route {
                proxy_name: request.name.clone(),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
                pool: Arc::clone(&proxy_ctx.pool),
                limiter: Arc::clone(&proxy_ctx.limiter),
                conn_limiter: Arc::clone(&proxy_ctx.conn_limiter),
                quota: Arc::clone(&proxy_ctx.quota),
                metrics: Arc::clone(&proxy_ctx.metrics),
                next_trace: Arc::clone(&proxy_ctx.next_trace),
                policy: Arc::new(policy),
                domain_policy: Arc::new(domain_policy),
                permits: Arc::new(Semaphore::new(max_conns_usize)),
            };
            ctx.state
                .socks5
                .register(route)
                .map_err(|err| (ErrorCode::Internal, err))?;
            info!(proxy = %request.name, client = %client_id, "socks5 proxy registered");
            claim.commit(ProxyEntry {
                name: request.name.clone(),
                proxy_type: "socks5".to_string(),
                remote_port: 0,
                domains: Vec::new(),
                local_addr: request.local_addr.clone(),
                rate_limit_bps,
                max_conns,
                monthly_bytes,
                quota_action: quota_action_name(quota_action).to_string(),
                idle_timeout_s,
                allow_cidrs: options.allow_cidrs.clone(),
                allow_domains: options.allow_domains.clone(),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
            });
            Ok(ProxyRuntime {
                spec: request.clone(),
                name: request.name,
                port: None,
                task: None,
                allocator: None,
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
                domains: Vec::new(),
                state: Arc::clone(&ctx.state),
            })
        }
    }
}

/// 两级限制合并：0 视为未设置，双方都设置时取较小值。
fn policy_proxy_type(kind: ProxyType) -> meatus_core::ProxyType {
    match kind {
        ProxyType::Tcp => meatus_core::ProxyType::Tcp,
        ProxyType::Udp => meatus_core::ProxyType::Udp,
        ProxyType::Http => meatus_core::ProxyType::Http,
        ProxyType::Https => meatus_core::ProxyType::Https,
        ProxyType::Socks5 => meatus_core::ProxyType::Socks5,
    }
}

fn quota_action_name(action: QuotaAction) -> &'static str {
    match action {
        QuotaAction::Reject => "reject",
        QuotaAction::Throttle => "throttle",
    }
}

fn effective_u64(proxy: u64, client: u64) -> u64 {
    match (proxy, client) {
        (0, value) | (value, 0) => value,
        (a, b) => a.min(b),
    }
}

fn effective_u32(proxy: u32, client: u32) -> u32 {
    match (proxy, client) {
        (0, value) | (value, 0) => value,
        (a, b) => a.min(b),
    }
}

fn proxy_type_name(kind: ProxyType) -> &'static str {
    match kind {
        ProxyType::Tcp => "tcp",
        ProxyType::Udp => "udp",
        ProxyType::Http => "http",
        ProxyType::Https => "https",
        ProxyType::Socks5 => "socks5",
    }
}

async fn tcp_accept_loop(
    listener: TcpListener,
    ctx: ProxyContext,
    semaphore: Arc<Semaphore>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ctx.state.drain.stopped_accepting() => break,
            accepted = listener.accept() => {
                let (mut user, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(proxy = %ctx.proxy_name, error = %err, "accept failed");
                        continue;
                    }
                };
                if !ctx.allow_connection() {
                    debug!(proxy = %ctx.proxy_name, "connection denied by quota or rate limit");
                    continue;
                }
                let permit = match Arc::clone(&semaphore).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        debug!(proxy = %ctx.proxy_name, "max_conns reached, dropping connection");
                        continue;
                    }
                };
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(err) = pipe_connection(&mut user, peer, ctx, permit).await {
                        debug!(%peer, error = %err, "user connection ended");
                    }
                });
            }
        }
    }
    debug!(proxy = %ctx.proxy_name, "public tcp listener stopped");
}

async fn pipe_connection(
    user: &mut TcpStream,
    peer: SocketAddr,
    ctx: ProxyContext,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    ctx.metrics.connection_started();
    let result = pipe_inner(user, peer, &ctx).await;
    match &result {
        Ok((bytes_in, bytes_out)) => {
            ctx.metrics.add_bytes(*bytes_in, *bytes_out);
            ctx.record_usage(*bytes_in, *bytes_out);
            ctx.state.usage.record(&UsageRecord {
                tenant_id: ctx.tenant_id.clone(),
                client_id: ctx.client_id.clone(),
                proxy_name: ctx.proxy_name.clone(),
                bytes_in: *bytes_in,
                bytes_out: *bytes_out,
                connections: 1,
                at: SystemTime::now(),
            });
        }
        Err(err) => debug!(error = %err, "tcp tunnel copy failed"),
    }
    ctx.metrics.connection_finished();
    result.map(|_| ())
}

async fn pipe_inner(
    user: &mut TcpStream,
    peer: SocketAddr,
    ctx: &ProxyContext,
) -> anyhow::Result<(u64, u64)> {
    let (mut tunnel, _guard) = timeout(HANDSHAKE_TIMEOUT, ctx.pool.open())
        .await
        .context("timed out opening data stream")?
        .context("open data stream")?;

    let header = StreamHeader {
        proxy_name: ctx.proxy_name.clone(),
        kind: StreamKind::UserConn as i32,
        src_addr: peer.to_string(),
        dst_hint: String::new(),
        created_ms: now_ms(),
        trace_id: format!("{:016x}", ctx.next_trace.fetch_add(1, Ordering::Relaxed)),
        udp_session_id: 0,
        connection_id: meatus_core::ids::new_id(),
    };
    info!(trace_id = %header.trace_id, connection_id = %header.connection_id, proxy = %ctx.proxy_name, "data tunnel opened");
    ctx.metrics
        .record_stream(&header.trace_id, &header.connection_id);
    write_frame(&mut tunnel, &header).await?;

    copy_bidirectional_limited(user, &mut tunnel, ctx.limiter.as_ref())
        .await
        .context("bidirectional copy")
}

struct UdpSession {
    addr: SocketAddr,
    last_seen: Instant,
    bytes_in: u64,
    bytes_out: u64,
}

async fn udp_proxy_loop(
    socket: UdpSocket,
    tunnel: MultiplexedStream,
    ctx: ProxyContext,
    cancel: CancellationToken,
    max_sessions: usize,
    idle: Duration,
) {
    let (mut reader, writer) = tokio::io::split(tunnel);
    let (packet_tx, packet_rx) = mpsc::channel::<UdpPacket>(512);
    let writer_task = tokio::spawn(write_udp_frames(writer, packet_rx));

    let mut by_addr: HashMap<SocketAddr, u32> = HashMap::new();
    let mut by_id: HashMap<u32, UdpSession> = HashMap::new();
    let mut next_id = 1u32;
    let mut cleanup = interval(UDP_CLEANUP);
    cleanup.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recv_buf = [0u8; UDP_MAX_PACKET + 1];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = cleanup.tick() => {
                let now = Instant::now();
                let expired: Vec<u32> = by_id
                    .iter()
                    .filter(|(_, session)| now.duration_since(session.last_seen) > idle)
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    if let Some(session) = by_id.remove(&id) {
                        by_addr.remove(&session.addr);
                        ctx.metrics.connection_finished();
                        ctx.record_usage(session.bytes_in, session.bytes_out);
                        ctx.state.usage.record(&UsageRecord {
                            tenant_id: ctx.tenant_id.clone(),
                            client_id: ctx.client_id.clone(),
                            proxy_name: ctx.proxy_name.clone(),
                            bytes_in: session.bytes_in,
                            bytes_out: session.bytes_out,
                            connections: 1,
                            at: SystemTime::now(),
                        });
                    }
                }
            }
            received = socket.recv_from(&mut recv_buf) => {
                let (len, src) = match received {
                    Ok(pair) => pair,
                    Err(err) => {
                        debug!(proxy = %ctx.proxy_name, error = %err, "udp recv_from failed");
                        continue;
                    }
                };
                if len > UDP_MAX_PACKET {
                    debug!(proxy = %ctx.proxy_name, "oversized udp datagram dropped");
                    continue;
                }
                ctx.limiter.consume(len as u64).await;

                let session_id = if let Some(id) = by_addr.get(&src).copied() {
                    id
                } else {
                    if by_id.len() >= max_sessions {
                        debug!(proxy = %ctx.proxy_name, "udp session limit reached");
                        continue;
                    }
                    if !ctx.allow_connection() {
                        debug!(proxy = %ctx.proxy_name, "udp session denied by quota");
                        continue;
                    }
                    let id = loop {
                        let candidate = next_id;
                        next_id = next_id.wrapping_add(1).max(1);
                        if candidate != 0 && !by_id.contains_key(&candidate) {
                            break candidate;
                        }
                    };
                    by_addr.insert(src, id);
                    by_id.insert(
                        id,
                        UdpSession {
                            addr: src,
                            last_seen: Instant::now(),
                            bytes_in: 0,
                            bytes_out: 0,
                        },
                    );
                    ctx.metrics.connection_started();
                    id
                };

                let Some(session) = by_id.get_mut(&session_id) else {
                    continue;
                };
                session.last_seen = Instant::now();
                session.bytes_in += len as u64;
                ctx.metrics.add_packet(len as u64, 0);
                let packet = UdpPacket {
                    session_id,
                    payload: recv_buf[..len].to_vec(),
                };
                if packet_tx.send(packet).await.is_err() {
                    break;
                }
            }
            packet = read_frame::<_, UdpPacket>(&mut reader) => {
                let packet = match packet {
                    Ok(packet) => packet,
                    Err(WireError::Closed) => break,
                    Err(err) => {
                        warn!(proxy = %ctx.proxy_name, error = %err, "udp tunnel read failed");
                        break;
                    }
                };
                if packet.payload.len() > UDP_MAX_PACKET {
                    debug!(proxy = %ctx.proxy_name, "oversized udp reply dropped");
                    continue;
                }
                let Some(session) = by_id.get_mut(&packet.session_id) else {
                    debug!(proxy = %ctx.proxy_name, id = packet.session_id, "unknown udp session reply");
                    continue;
                };
                ctx.limiter.consume(packet.payload.len() as u64).await;
                if let Err(err) = socket.send_to(&packet.payload, session.addr).await {
                    debug!(proxy = %ctx.proxy_name, error = %err, "udp send_to failed");
                    continue;
                }
                session.last_seen = Instant::now();
                session.bytes_out += packet.payload.len() as u64;
                ctx.metrics.add_packet(0, packet.payload.len() as u64);
            }
        }
    }

    for (_, session) in by_id {
        ctx.metrics.connection_finished();
        ctx.state.usage.record(&UsageRecord {
            tenant_id: ctx.tenant_id.clone(),
            client_id: ctx.client_id.clone(),
            proxy_name: ctx.proxy_name.clone(),
            bytes_in: session.bytes_in,
            bytes_out: session.bytes_out,
            connections: 1,
            at: SystemTime::now(),
        });
    }
    writer_task.abort();
    debug!(proxy = %ctx.proxy_name, "public udp listener stopped");
}

async fn write_udp_frames(mut writer: Writer, mut packets: mpsc::Receiver<UdpPacket>) {
    while let Some(packet) = packets.recv().await {
        if write_frame(&mut writer, &packet).await.is_err() {
            break;
        }
    }
}

fn random_bytes(len: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("getrandom failed: {err}"))?;
    Ok(bytes)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
