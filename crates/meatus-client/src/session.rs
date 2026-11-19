#![forbid(unsafe_code)]
//! 客户端会话：握手、注册、心跳、统计、TCP/UDP 转发与热重载。

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use meatus_core::{
    ClientConfig, MultiplexedStream, ProxySpec, ProxyType as ConfigProxyType, TargetPolicy,
    WireError, multiplex, read_frame, write_frame,
};
use meatus_proto::{
    PROTOCOL_VERSION,
    v1::{
        AdminProxyAck, Auth, Challenge, ControlMsg, Hello, NewProxy, Ping, Pong, ProxyOk,
        ProxyOptions, ProxyType as WireProxyType, StreamHeader, StreamKind, UdpPacket, Welcome,
        control_msg,
    },
};
use tokio::io::{AsyncWrite, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval, interval_at, timeout};
use tokio_util::compat::TokioAsyncReadCompatExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::stats::{ClientStats, ProxyCounter};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_PING: Duration = Duration::from_secs(10);
const PONG_TIMEOUT: Duration = Duration::from_secs(30);
const READ_SLICE: Duration = Duration::from_secs(35);
const DATA_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const STATS_INTERVAL: Duration = Duration::from_secs(10);
const UDP_MAX_PACKET: usize = 1500;
const UDP_MAX_SESSIONS: usize = 2048;
const UDP_IDLE: Duration = Duration::from_secs(60);
const UDP_CLEANUP: Duration = Duration::from_secs(30);

type Reader = ReadHalf<MultiplexedStream>;
type Writer = WriteHalf<MultiplexedStream>;

pub async fn run_session(
    config: &ClientConfig,
    cancel: &CancellationToken,
    reload_rx: &mut mpsc::Receiver<Vec<ProxySpec>>,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let tls_stream = crate::tls::connect(config).await?;
    let (open, mut inbound) = multiplex::spawn(tls_stream.compat(), yamux::Mode::Client);

    let control = timeout(HANDSHAKE_TIMEOUT, open.open())
        .await
        .context("timed out opening control stream")?
        .context("open control stream")?;
    let (mut reader, mut writer) = tokio::io::split(control);

    handshake(config, &mut reader, &mut writer).await?;
    let stats = Arc::new(ClientStats::new(&config.proxies));
    register_proxies(config, &stats, &mut reader, &mut writer).await?;

    let proxies = Arc::new(RwLock::new(config.proxy_map()));
    let dispatcher = {
        let proxies = Arc::clone(&proxies);
        let stats = Arc::clone(&stats);
        tokio::spawn(async move {
            while let Some(stream) = inbound.recv().await {
                let proxies = Arc::clone(&proxies);
                let stats = Arc::clone(&stats);
                tokio::spawn(async move {
                    if let Err(err) = handle_data(stream, proxies, stats).await {
                        debug!(error = %err, "data stream ended");
                    }
                });
            }
        })
    };

    let result = control_loop(
        &mut reader,
        &mut writer,
        cancel,
        reload_rx,
        proxies,
        stats,
        config_path,
    )
    .await;
    dispatcher.abort();
    result
}

async fn handshake(
    config: &ClientConfig,
    reader: &mut Reader,
    writer: &mut Writer,
) -> anyhow::Result<()> {
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                client_id: config.client.client_id.clone(),
                caps: vec![
                    "tcp".to_string(),
                    "udp".to_string(),
                    "http".to_string(),
                    "https".to_string(),
                    "socks5".to_string(),
                    "probe".to_string(),
                ],
                tenant_id: config.client.tenant.clone(),
                token_id: String::new(),
            })),
        },
    )
    .await?;

    let challenge = expect(reader, HANDSHAKE_TIMEOUT, "Challenge").await?;
    let nonce = match challenge.kind {
        Some(control_msg::Kind::Challenge(Challenge { nonce })) => nonce,
        Some(control_msg::Kind::Error(err)) => {
            bail!("server rejected Hello: {} ({})", err.message, err.code)
        }
        _ => bail!("expected Challenge"),
    };
    if nonce.len() < 16 {
        bail!("server sent a weak challenge nonce");
    }

    let hmac = meatus_core::proof(&config.client.token, &nonce);
    write_frame(
        writer,
        &ControlMsg {
            kind: Some(control_msg::Kind::Auth(Auth { hmac })),
        },
    )
    .await?;

    let reply = expect(reader, HANDSHAKE_TIMEOUT, "Welcome").await?;
    match reply.kind {
        Some(control_msg::Kind::Welcome(Welcome {
            run_id,
            heartbeat_secs,
            ..
        })) => {
            info!(
                server = %config.client.server_addr,
                tenant = %config.client.tenant,
                client = %config.client.client_id,
                %run_id,
                heartbeat_secs,
                "tunnel established"
            );
            Ok(())
        }
        Some(control_msg::Kind::Error(err)) => {
            bail!("server rejected handshake: {} ({})", err.message, err.code)
        }
        _ => bail!("expected Welcome"),
    }
}

async fn register_proxies(
    config: &ClientConfig,
    stats: &ClientStats,
    reader: &mut Reader,
    writer: &mut Writer,
) -> anyhow::Result<()> {
    for proxy in &config.proxies {
        if !supports(proxy.kind) {
            warn!(
                proxy = %proxy.name,
                kind = ?proxy.kind,
                "proxy type is not implemented yet; skipping"
            );
            continue;
        }

        write_frame(writer, &new_proxy_msg(proxy)).await?;
        match wait_proxy_reply(reader, writer, &proxy.name).await? {
            Some(ProxyOk {
                name, remote_port, ..
            }) => info!(proxy = %name, remote_port, "proxy registered"),
            None => warn!(proxy = %proxy.name, "proxy registration rejected"),
        }
    }
    let _ = stats;
    Ok(())
}

fn supports(kind: ConfigProxyType) -> bool {
    matches!(
        kind,
        ConfigProxyType::Tcp
            | ConfigProxyType::Udp
            | ConfigProxyType::Http
            | ConfigProxyType::Https
            | ConfigProxyType::Socks5
    )
}

fn spec_from_new_proxy(request: NewProxy) -> anyhow::Result<ProxySpec> {
    let kind = match request.proxy_type {
        value if value == WireProxyType::Tcp as i32 => ConfigProxyType::Tcp,
        value if value == WireProxyType::Udp as i32 => ConfigProxyType::Udp,
        value if value == WireProxyType::Http as i32 => ConfigProxyType::Http,
        value if value == WireProxyType::Https as i32 => ConfigProxyType::Https,
        value if value == WireProxyType::Socks5 as i32 => ConfigProxyType::Socks5,
        value => bail!("unsupported proxy_type={value}"),
    };
    let name = request.name.trim().to_string();
    if name.is_empty() {
        bail!("proxy name is required");
    }
    if request.local_addr.trim().is_empty() {
        bail!("proxy {name} local_addr is required");
    }
    let remote_port = match request.remote_port {
        0 => None,
        value => Some(
            u16::try_from(value)
                .with_context(|| format!("proxy {name} remote_port {value} is out of range"))?,
        ),
    };
    let options = request.options.unwrap_or_default();
    let allow_cidrs = options.allow_cidrs;
    if !allow_cidrs.is_empty() {
        meatus_core::TargetPolicy::from_allow_cidrs(&allow_cidrs)
            .map_err(|err| anyhow::anyhow!("proxy {name} allow_cidrs invalid: {err}"))?;
    }
    let allow_domains = options.allow_domains;
    meatus_core::DomainPolicy::from_allow_domains(&allow_domains)
        .map_err(|err| anyhow::anyhow!("proxy {name} allow_domains invalid: {err}"))?;
    Ok(ProxySpec {
        name,
        kind,
        local_addr: request.local_addr,
        remote_port,
        custom_domains: request.custom_domains,
        options: meatus_core::ProxyOptions {
            rate_limit_bps: (options.rate_limit_bps > 0).then_some(options.rate_limit_bps),
            idle_timeout_s: (options.idle_timeout_s > 0).then_some(options.idle_timeout_s),
            max_conns: (options.max_conns > 0).then_some(options.max_conns),
            allow_cidrs,
            allow_domains,
        },
    })
}

fn new_proxy_msg(proxy: &ProxySpec) -> ControlMsg {
    let proxy_type = match proxy.kind {
        ConfigProxyType::Tcp => WireProxyType::Tcp,
        ConfigProxyType::Udp => WireProxyType::Udp,
        ConfigProxyType::Http => WireProxyType::Http,
        ConfigProxyType::Https => WireProxyType::Https,
        ConfigProxyType::Socks5 => WireProxyType::Socks5,
    };
    ControlMsg {
        kind: Some(control_msg::Kind::NewProxy(NewProxy {
            name: proxy.name.clone(),
            proxy_type: proxy_type as i32,
            local_addr: proxy.local_addr.clone(),
            remote_port: proxy.remote_port.map_or(0, u32::from),
            custom_domains: proxy.custom_domains.clone(),
            options: Some(ProxyOptions {
                rate_limit_bps: proxy.options.rate_limit_bps.unwrap_or(0),
                idle_timeout_s: proxy.options.idle_timeout_s.unwrap_or(0),
                max_conns: proxy.options.max_conns.unwrap_or(0),
                enable_compression: false,
                allow_cidrs: proxy.options.allow_cidrs.clone(),
                allow_domains: proxy.options.allow_domains.clone(),
            }),
        })),
    }
}

async fn wait_proxy_reply(
    reader: &mut Reader,
    writer: &mut Writer,
    proxy: &str,
) -> anyhow::Result<Option<ProxyOk>> {
    loop {
        let reply = expect(reader, HANDSHAKE_TIMEOUT, "ProxyOk").await?;
        match reply.kind {
            Some(control_msg::Kind::ProxyOk(ok)) => return Ok(Some(ok)),
            Some(control_msg::Kind::Error(err)) => {
                if err.fatal {
                    bail!("proxy {proxy} rejected: {} ({})", err.message, err.code);
                }
                warn!(
                    proxy,
                    code = err.code,
                    error = %err.message,
                    "proxy registration rejected"
                );
                return Ok(None);
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
                .await?;
            }
            Some(control_msg::Kind::Pong(_)) | Some(control_msg::Kind::Stats(_)) => {}
            Some(_) => {
                warn!(proxy, "unexpected reply to NewProxy");
                return Ok(None);
            }
            None => return Ok(None),
        }
    }
}

async fn control_loop(
    reader: &mut Reader,
    writer: &mut Writer,
    cancel: &CancellationToken,
    reload_rx: &mut mpsc::Receiver<Vec<ProxySpec>>,
    proxies: Arc<RwLock<HashMap<String, ProxySpec>>>,
    stats: Arc<ClientStats>,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let mut ping_timer = interval_at(TokioInstant::now() + CLIENT_PING, CLIENT_PING);
    ping_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stats_timer = interval_at(TokioInstant::now() + STATS_INTERVAL, STATS_INTERVAL);
    stats_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ping_seq = 0u64;
    let mut last_pong = Instant::now();
    let mut pending: HashMap<String, ProxySpec> = HashMap::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("client session cancelled");
                return Ok(());
            }
            _ = ping_timer.tick() => {
                ping_seq += 1;
                write_frame(
                    writer,
                    &ControlMsg { kind: Some(control_msg::Kind::Ping(Ping { seq: ping_seq, ts_ms: now_ms() })) },
                ).await?;
                if last_pong.elapsed() > PONG_TIMEOUT {
                    bail!("no Pong for {:?}", last_pong.elapsed());
                }
            }
            _ = stats_timer.tick() => {
                for snapshot in stats.snapshots() {
                    write_frame(
                        writer,
                        &ControlMsg { kind: Some(control_msg::Kind::Stats(snapshot)) },
                    ).await?;
                }
            }
            Some(desired) = reload_rx.recv() => {
                apply_reload(writer, &proxies, &stats, &mut pending, desired).await?;
            }
            frame = timeout(READ_SLICE, read_frame::<_, ControlMsg>(reader)) => {
                let msg = match frame {
                    Ok(Ok(msg)) => msg,
                    Ok(Err(WireError::Closed)) => return Ok(()),
                    Ok(Err(err)) => return Err(err).context("read control frame"),
                    Err(_) => continue,
                };

                match msg.kind {
                    Some(control_msg::Kind::Ping(ping)) => {
                        write_frame(
                            writer,
                            &ControlMsg { kind: Some(control_msg::Kind::Pong(Pong { seq: ping.seq, ts_ms: now_ms() })) },
                        ).await?;
                    }
                    Some(control_msg::Kind::Pong(_)) => {
                        last_pong = Instant::now();
                    }
                    Some(control_msg::Kind::ProxyOk(ok)) => {
                        if let Some(spec) = pending.remove(&ok.name) {
                            proxies
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(ok.name.clone(), spec);
                            info!(proxy = %ok.name, remote_port = ok.remote_port, "proxy hot-added");
                        }
                    }
                    Some(control_msg::Kind::Error(err)) => {
                        if err.fatal {
                            bail!("server error {}: {}", err.code, err.message);
                        }
                        warn!(code = err.code, error = %err.message, "server reported error");
                        pending.clear();
                    }
                    Some(control_msg::Kind::AdminAddProxy(cmd)) => {
                        let request_id = cmd.request_id;
                        let result = cmd
                            .proxy
                            .ok_or_else(|| "admin add proxy missing spec".to_string())
                            .and_then(|proxy| {
                                spec_from_new_proxy(proxy)
                                    .map_err(|err| format!("admin add proxy rejected: {err}"))
                            });
                        match result {
                            Ok(spec) => {
                                let persisted = config_path.map_or(Ok(()), |path| {
                                    meatus_core::upsert_client_proxy(path, &spec)
                                });
                                if let Err(error) = persisted {
                                    warn!(%error, "admin add proxy config write failed");
                                    write_frame(
                                        writer,
                                        &ControlMsg {
                                            kind: Some(control_msg::Kind::AdminProxyAck(AdminProxyAck {
                                                request_id,
                                                ok: false,
                                                error: format!("write client config: {error}"),
                                            })),
                                        },
                                    )
                                    .await?;
                                    continue;
                                }
                                let name = spec.name.clone();
                                stats.register(&name);
                                proxies
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .insert(name.clone(), spec);
                                info!(proxy = %name, "proxy added by admin");
                                write_frame(
                                    writer,
                                    &ControlMsg {
                                        kind: Some(control_msg::Kind::AdminProxyAck(AdminProxyAck {
                                            request_id,
                                            ok: true,
                                            error: String::new(),
                                        })),
                                    },
                                )
                                .await?;
                            }
                            Err(error) => {
                                warn!(%error, "admin add proxy rejected");
                                write_frame(
                                    writer,
                                    &ControlMsg {
                                        kind: Some(control_msg::Kind::AdminProxyAck(AdminProxyAck {
                                            request_id,
                                            ok: false,
                                            error,
                                        })),
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                    Some(control_msg::Kind::AdminRemoveProxy(cmd)) => {
                        let name = cmd.name;
                        let persisted = config_path.map_or(Ok(()), |path| {
                            meatus_core::remove_client_proxy(path, &name)
                        });
                        if let Err(error) = persisted {
                            warn!(%error, "admin remove proxy config write failed");
                            write_frame(
                                writer,
                                &ControlMsg {
                                    kind: Some(control_msg::Kind::AdminProxyAck(AdminProxyAck {
                                        request_id: cmd.request_id,
                                        ok: false,
                                        error: format!("write client config: {error}"),
                                    })),
                                },
                            )
                            .await?;
                            continue;
                        }
                        proxies
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&name);
                        stats.unregister(&name);
                        info!(proxy = %name, "proxy removed by admin");
                        write_frame(
                            writer,
                            &ControlMsg {
                                kind: Some(control_msg::Kind::AdminProxyAck(AdminProxyAck {
                                    request_id: cmd.request_id,
                                    ok: true,
                                    error: String::new(),
                                })),
                            },
                        )
                        .await?;
                    }
                    Some(control_msg::Kind::Probe(request)) => {
                        let spec = proxies
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get(&request.proxy_name)
                            .cloned();
                        let result = crate::probe::run(
                            request.request_id,
                            &request.proxy_name,
                            spec.as_ref(),
                            request.timeout_ms,
                        )
                        .await;
                        write_frame(
                            writer,
                            &ControlMsg {
                                kind: Some(control_msg::Kind::ProbeResult(result)),
                            },
                        )
                        .await?;
                    }
                    Some(control_msg::Kind::Stats(_)) => {
                        debug!("stats frame received");
                    }
                    Some(_) => {
                        debug!("ignoring unexpected control frame");
                    }
                    None => {}
                }
            }
        }
    }
}

async fn apply_reload<W>(
    writer: &mut W,
    proxies: &Arc<RwLock<HashMap<String, ProxySpec>>>,
    stats: &ClientStats,
    pending: &mut HashMap<String, ProxySpec>,
    desired: Vec<ProxySpec>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let desired: Vec<ProxySpec> = desired
        .into_iter()
        .filter(|proxy| supports(proxy.kind))
        .collect();
    let desired_map: HashMap<String, ProxySpec> = desired
        .iter()
        .map(|proxy| (proxy.name.clone(), proxy.clone()))
        .collect();

    let current: HashMap<String, ProxySpec> = proxies
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();

    let mut removals = HashSet::new();
    for name in current.keys() {
        if !desired_map.contains_key(name) {
            removals.insert(name.clone());
        }
    }
    for (name, spec) in &desired_map {
        if current.get(name).is_some_and(|existing| existing != spec) {
            removals.insert(name.clone());
        }
    }

    for name in removals {
        let was_active = current.contains_key(&name);
        let was_pending = pending.remove(&name).is_some();
        if was_active || was_pending {
            write_frame(
                writer,
                &ControlMsg {
                    kind: Some(control_msg::Kind::RemoveProxy(
                        meatus_proto::v1::RemoveProxy { name: name.clone() },
                    )),
                },
            )
            .await?;
            proxies
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&name);
            info!(proxy = %name, "proxy hot-removed");
        }
    }

    for spec in desired {
        let unchanged = current.get(&spec.name) == Some(&spec);
        if unchanged {
            continue;
        }
        if let Some(pending_spec) = pending.get(&spec.name) {
            if pending_spec == &spec {
                continue;
            }
            write_frame(
                writer,
                &ControlMsg {
                    kind: Some(control_msg::Kind::RemoveProxy(
                        meatus_proto::v1::RemoveProxy {
                            name: spec.name.clone(),
                        },
                    )),
                },
            )
            .await?;
            pending.remove(&spec.name);
        }
        stats.register(&spec.name);
        write_frame(writer, &new_proxy_msg(&spec)).await?;
        info!(proxy = %spec.name, "proxy hot-add requested");
        pending.insert(spec.name.clone(), spec);
    }
    Ok(())
}

async fn handle_data(
    mut stream: MultiplexedStream,
    proxies: Arc<RwLock<HashMap<String, ProxySpec>>>,
    stats: Arc<ClientStats>,
) -> anyhow::Result<()> {
    let header: StreamHeader = timeout(DATA_HEADER_TIMEOUT, read_frame(&mut stream))
        .await
        .context("timed out waiting for StreamHeader")??;

    let proxy = proxies
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&header.proxy_name)
        .cloned();
    let Some(proxy) = proxy else {
        bail!("unknown proxy {}", header.proxy_name);
    };

    match header.kind {
        value if value == StreamKind::UdpSession as i32 => {
            if proxy.kind != ConfigProxyType::Udp {
                bail!("proxy {} is not udp", header.proxy_name);
            }
            let counter = stats.counter(&proxy.name);
            handle_udp_stream(stream, proxy, counter, header).await
        }
        value if value == StreamKind::UserConn as i32 => {
            if !matches!(proxy.kind, ConfigProxyType::Tcp | ConfigProxyType::Socks5) {
                bail!("proxy {} is not tcp/socks5", header.proxy_name);
            }
            let counter = stats.counter(&proxy.name);
            handle_tcp_stream(stream, proxy, counter, header).await
        }
        value if value == StreamKind::HttpReq as i32 => {
            if !matches!(proxy.kind, ConfigProxyType::Http | ConfigProxyType::Https) {
                bail!("proxy {} is not http/https", header.proxy_name);
            }
            let counter = stats.counter(&proxy.name);
            handle_tcp_stream(stream, proxy, counter, header).await
        }
        other => bail!("unsupported stream kind {other}"),
    }
}

async fn handle_tcp_stream(
    mut stream: MultiplexedStream,
    proxy: ProxySpec,
    counter: Option<Arc<ProxyCounter>>,
    header: StreamHeader,
) -> anyhow::Result<()> {
    if let Some(counter) = &counter {
        counter.connection_started();
    }
    let result = handle_tcp_inner(&mut stream, &proxy, &header, counter.as_ref()).await;
    if let Some(counter) = &counter {
        counter.connection_finished();
    }
    result
}

async fn handle_tcp_inner(
    stream: &mut MultiplexedStream,
    proxy: &ProxySpec,
    header: &StreamHeader,
    counter: Option<&Arc<ProxyCounter>>,
) -> anyhow::Result<()> {
    let target = if proxy.kind == ConfigProxyType::Socks5 {
        let raw = header.dst_hint.trim();
        if raw.is_empty() {
            bail!("socks5 stream is missing dst_hint");
        }
        resolve_socks_target(proxy, raw).await?.to_string()
    } else {
        proxy.local_addr.clone()
    };

    let mut local = match timeout(LOCAL_CONNECT_TIMEOUT, TcpStream::connect(&target)).await {
        Ok(Ok(local)) => local,
        Ok(Err(err)) => {
            if let Some(counter) = counter {
                counter.local_fail();
            }
            return Err(err).with_context(|| format!("local connect {target}"));
        }
        Err(err) => {
            if let Some(counter) = counter {
                counter.local_fail();
            }
            return Err(err).with_context(|| format!("local connect {target} timed out"));
        }
    };
    local.set_nodelay(true).context("set TCP_NODELAY")?;

    debug!(
        proxy = %header.proxy_name,
        src = %header.src_addr,
        remote_port = proxy.remote_port.unwrap_or(0),
        "data stream forwarding"
    );

    let (local_to_tunnel, tunnel_to_local) = tokio::io::copy_bidirectional(&mut local, stream)
        .await
        .context("bidirectional copy")?;
    if let Some(counter) = counter {
        counter.add_bytes(tunnel_to_local, local_to_tunnel);
    }
    Ok(())
}

async fn resolve_socks_target(proxy: &ProxySpec, raw: &str) -> anyhow::Result<SocketAddr> {
    let policy = TargetPolicy::from_allow_cidrs(&proxy.options.allow_cidrs)
        .map_err(|err| anyhow::anyhow!("invalid allow_cidrs: {err}"))?;
    let domains = meatus_core::DomainPolicy::from_allow_domains(&proxy.options.allow_domains)
        .map_err(|err| anyhow::anyhow!("invalid allow_domains: {err}"))?;

    if let Ok(target) = raw.parse::<SocketAddr>() {
        if !policy.allows(target.ip()) {
            bail!("socks5 target {target} is outside allow_cidrs");
        }
        return Ok(target);
    }

    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid socks5 target {raw:?}"))?;
    if port.is_empty() {
        bail!("socks5 target {raw:?} is missing a port");
    }
    let port: u16 = port
        .parse()
        .with_context(|| format!("invalid socks5 target port in {raw:?}"))?;
    if port == 0 {
        bail!("socks5 target {raw:?} port must be 1..65535");
    }
    let host = meatus_core::normalize_domain(host)
        .map_err(|err| anyhow::anyhow!("invalid socks5 target {raw:?}: {err}"))?;
    if !domains.allows(&host) {
        bail!("socks5 target {host} is outside allow_domains");
    }

    let resolved = timeout(
        DNS_LOOKUP_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .with_context(|| format!("dns lookup {raw} timed out"))?
    .with_context(|| format!("dns lookup {raw}"))?;
    let mut allowed = resolved.filter(|addr| policy.allows(addr.ip()));
    let target = allowed
        .next()
        .ok_or_else(|| anyhow::anyhow!("socks5 target {host} has no address inside allow_cidrs"))?;
    // 固定已校验的 IP，后续 connect 不再二次解析，避免 DNS rebinding。
    Ok(target)
}

struct UdpClientSession {
    socket: Arc<UdpSocket>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    last_seen: Instant,
}

async fn handle_udp_stream(
    stream: MultiplexedStream,
    proxy: ProxySpec,
    counter: Option<Arc<ProxyCounter>>,
    header: StreamHeader,
) -> anyhow::Result<()> {
    if let Some(counter) = &counter {
        counter.add_bytes(0, 0);
    }
    debug!(
        proxy = %header.proxy_name,
        "udp tunnel stream forwarding"
    );
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (reply_tx, mut reply_rx) = mpsc::channel::<UdpPacket>(256);
    let writer_task = tokio::spawn(async move {
        while let Some(packet) = reply_rx.recv().await {
            if write_frame(&mut writer, &packet).await.is_err() {
                break;
            }
        }
    });

    let mut sessions: HashMap<u32, UdpClientSession> = HashMap::new();
    let mut cleanup = interval(UDP_CLEANUP);
    cleanup.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cleanup.tick() => {
                let now = Instant::now();
                let expired: Vec<u32> = sessions
                    .iter()
                    .filter(|(_, session)| now.duration_since(session.last_seen) > UDP_IDLE)
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    if let Some(session) = sessions.remove(&id) {
                        session.cancel.cancel();
                        session.task.abort();
                        if let Some(counter) = &counter {
                            counter.connection_finished();
                        }
                    }
                }
            }
            packet = read_frame::<_, UdpPacket>(&mut reader) => {
                let packet = match packet {
                    Ok(packet) => packet,
                    Err(WireError::Closed) => break,
                    Err(err) => {
                        warn!(proxy = %proxy.name, error = %err, "udp tunnel read failed");
                        break;
                    }
                };
                if packet.payload.len() > UDP_MAX_PACKET {
                    debug!(proxy = %proxy.name, "oversized udp packet dropped");
                    continue;
                }

                let session_id = packet.session_id;
                if !sessions.contains_key(&session_id) {
                    if sessions.len() >= UDP_MAX_SESSIONS {
                        debug!(proxy = %proxy.name, "udp session limit reached");
                        continue;
                    }
                    let socket = match UdpSocket::bind("0.0.0.0:0").await {
                        Ok(socket) => Arc::new(socket),
                        Err(err) => {
                            if let Some(counter) = &counter {
                                counter.local_fail();
                            }
                            warn!(proxy = %proxy.name, error = %err, "bind udp local socket failed");
                            continue;
                        }
                    };
                    if let Err(err) = socket.connect(&proxy.local_addr).await {
                        if let Some(counter) = &counter {
                            counter.local_fail();
                        }
                        warn!(proxy = %proxy.name, target = %proxy.local_addr, error = %err, "udp local connect failed");
                        continue;
                    }
                    let cancel = CancellationToken::new();
                    let task = tokio::spawn(udp_reply_loop(
                        Arc::clone(&socket),
                        reply_tx.clone(),
                        session_id,
                        cancel.clone(),
                        counter.clone(),
                    ));
                    if let Some(counter) = &counter {
                        counter.connection_started();
                    }
                    sessions.insert(
                        session_id,
                        UdpClientSession {
                            socket,
                            cancel,
                            task,
                            last_seen: Instant::now(),
                        },
                    );
                }

                let Some(session) = sessions.get_mut(&session_id) else {
                    continue;
                };
                session.last_seen = Instant::now();
                if let Err(err) = session.socket.send(&packet.payload).await {
                    debug!(proxy = %proxy.name, error = %err, "udp local send failed");
                } else if let Some(counter) = &counter {
                    counter.add_bytes(packet.payload.len() as u64, 0);
                }
            }
        }
    }

    for (_, session) in sessions {
        session.cancel.cancel();
        session.task.abort();
        if let Some(counter) = &counter {
            counter.connection_finished();
        }
    }
    writer_task.abort();
    Ok(())
}

async fn udp_reply_loop(
    socket: Arc<UdpSocket>,
    replies: mpsc::Sender<UdpPacket>,
    session_id: u32,
    cancel: CancellationToken,
    counter: Option<Arc<ProxyCounter>>,
) {
    let mut buffer = [0u8; UDP_MAX_PACKET];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            received = socket.recv(&mut buffer) => {
                let len = match received {
                    Ok(len) => len,
                    Err(err) => {
                        debug!(error = %err, "udp local recv failed");
                        continue;
                    }
                };
                if len > UDP_MAX_PACKET {
                    continue;
                }
                let packet = UdpPacket {
                    session_id,
                    payload: buffer[..len].to_vec(),
                };
                if replies.send(packet).await.is_err() {
                    break;
                }
                if let Some(counter) = &counter {
                    counter.add_bytes(0, len as u64);
                }
            }
        }
    }
}

async fn expect(reader: &mut Reader, wait: Duration, label: &str) -> anyhow::Result<ControlMsg> {
    let frame = timeout(wait, read_frame::<_, ControlMsg>(reader))
        .await
        .with_context(|| format!("timed out waiting for {label}"))??;
    Ok(frame)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn spec(name: &str, remote_port: u16) -> ProxySpec {
        ProxySpec {
            name: name.to_string(),
            kind: ConfigProxyType::Tcp,
            local_addr: "127.0.0.1:80".to_string(),
            remote_port: Some(remote_port),
            custom_domains: Vec::new(),
            options: meatus_core::ProxyOptions::default(),
        }
    }

    #[test]
    fn spec_helper_is_available() {
        assert_eq!(spec("p0", 20000).name, "p0");
    }

    #[tokio::test]
    async fn reload_diff_adds_and_removes_proxies() {
        let (mut client, mut server) = duplex(8192);
        let p0 = spec("p0", 20000);
        let p1 = spec("p1", 20001);
        let proxies = Arc::new(RwLock::new(HashMap::from([("p0".to_string(), p0.clone())])));
        let stats = ClientStats::new(std::slice::from_ref(&p0));
        let mut pending = HashMap::new();

        apply_reload(
            &mut client,
            &proxies,
            &stats,
            &mut pending,
            vec![p0.clone(), p1.clone()],
        )
        .await
        .expect("add p1");

        let msg: ControlMsg = read_frame(&mut server).await.expect("read new proxy");
        match msg.kind {
            Some(control_msg::Kind::NewProxy(request)) => assert_eq!(request.name, "p1"),
            other => panic!("expected NewProxy, got {other:?}"),
        }
        assert!(pending.contains_key("p1"));

        // 模拟 ProxyOk 后 p1 生效，再做删除差量。
        proxies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("p1".to_string(), p1.clone());
        pending.clear();
        apply_reload(
            &mut client,
            &proxies,
            &stats,
            &mut pending,
            vec![p0.clone()],
        )
        .await
        .expect("remove p1");

        let msg: ControlMsg = read_frame(&mut server).await.expect("read remove proxy");
        match msg.kind {
            Some(control_msg::Kind::RemoveProxy(request)) => assert_eq!(request.name, "p1"),
            other => panic!("expected RemoveProxy, got {other:?}"),
        }
        assert!(
            !proxies
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key("p1")
        );
    }
}
