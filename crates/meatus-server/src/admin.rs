#![forbid(unsafe_code)]
//! 管理面 HTTP：静态面板、登录会话、CSRF 与 JSON API。
//!
//! 管理面默认只绑定 loopback；配置 `admin.admin_token` 后，除 `/healthz`
//! 与登录接口外所有 API 都要求 HttpOnly 会话 Cookie 或 `Authorization: Bearer`。
//! 写接口统一要求 `X-Meatus-CSRF`，避免浏览器跨站请求伪造。

use std::collections::HashMap;
use std::io::Cursor;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use meatus_core::{
    AuthMode, ClientEntry, ConfigFormat, LogFormat, PortReleasePolicy, ServerConfig,
};
use meatus_proto::v1::{NewProxy, ProxyOptions, ProxyType};
use rust_embed::RustEmbed;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use x509_parser::prelude::{GeneralName, parse_x509_certificate};

use crate::admin_bus::{AdminBus, AdminProxyCommand};
use crate::logbuf::LogBuffer;
use crate::metrics::Metrics;
use crate::status::StatusRegistry;
use crate::tls;

const READ_TIMEOUT: Duration = Duration::from_secs(5);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const LOGIN_FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
const LOGIN_MAX_FAILURES: u32 = 5;
const COOKIE_NAME: &str = "meatus_session";

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebAssets;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSummary {
    pub token_configured: bool,
    pub panel: bool,
    pub loopback: bool,
}

#[derive(Clone)]
pub struct AdminState {
    metrics: Arc<Metrics>,
    status: Arc<StatusRegistry>,
    quotas: Arc<crate::quota::MonthlyQuota>,
    config: Arc<ServerConfig>,
    runtime: Arc<crate::RuntimeConfig>,
    reloader: Option<Arc<crate::ConfigReloader>>,
    config_path: Option<PathBuf>,
    logs: Arc<LogBuffer>,
    admin_bus: Arc<AdminBus>,
    acme: Option<Arc<crate::acme::AcmeManager>>,
    admin_token: Option<Arc<str>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    failures: Arc<Mutex<HashMap<IpAddr, LoginFailure>>>,
    probes: Arc<Mutex<HashMap<String, Instant>>>,
}

impl AdminState {
    #[must_use]
    pub fn with_config_path(mut self, path: Option<PathBuf>) -> Self {
        self.config_path = path;
        self
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)] // mirrors the server wiring; grouping would add no clarity
    pub fn new(
        metrics: Arc<Metrics>,
        status: Arc<StatusRegistry>,
        quotas: Arc<crate::quota::MonthlyQuota>,
        config: Arc<ServerConfig>,
        runtime: Arc<crate::RuntimeConfig>,
        logs: Arc<LogBuffer>,
        admin_bus: Arc<AdminBus>,
        acme: Option<Arc<crate::acme::AcmeManager>>,
        reloader: Option<Arc<crate::ConfigReloader>>,
    ) -> Self {
        let admin_token = config
            .admin
            .admin_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .map(Arc::<str>::from);
        Self {
            metrics,
            status,
            quotas,
            config,
            runtime,
            reloader,
            logs,
            admin_bus,
            acme,
            admin_token,
            config_path: None,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            failures: Arc::new(Mutex::new(HashMap::new())),
            probes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn current_config(&self) -> Arc<ServerConfig> {
        self.runtime.config()
    }

    #[must_use]
    pub fn auth_summary(&self) -> AuthSummary {
        AuthSummary {
            token_configured: self.admin_token.is_some(),
            panel: self.config.admin.panel,
            loopback: is_loopback_bind(&self.config.admin.bind),
        }
    }
}

#[derive(Debug, Clone)]
struct Session {
    csrf: String,
    last_seen: Instant,
}

#[derive(Debug, Clone, Copy)]
struct LoginFailure {
    count: u32,
    first_at: Instant,
    locked_until: Option<Instant>,
}

struct HttpRequest {
    method: String,
    path: String,
    query: String,
    headers: String,
    body: Vec<u8>,
    secure: bool,
}

struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    cookie: Option<String>,
    cache_control: &'static str,
}

impl HttpResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: format!("{body}\n").into_bytes(),
            cookie: None,
            cache_control: "no-store",
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: format!("{body}\n").into_bytes(),
            cookie: None,
            cache_control: "no-store",
        }
    }

    fn html(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body,
            cookie: None,
            cache_control: "no-cache",
        }
    }

    fn asset(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
            cookie: None,
            cache_control: "public, max-age=31536000, immutable",
        }
    }

    fn with_cookie(mut self, cookie: String) -> Self {
        self.cookie = Some(cookie);
        self
    }
}

pub async fn serve(
    listener: TcpListener,
    state: AdminState,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let tls_acceptor = if state.config.admin.tls.enabled {
        Some(TlsAcceptor::from(tls::load_admin(&state.config)?))
    } else {
        None
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (mut stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "admin accept failed");
                        continue;
                    }
                };
                let state = state.clone();
                if let Some(acceptor) = tls_acceptor.clone() {
                    tokio::spawn(async move {
                        match timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                            Ok(Ok(mut tls_stream)) => {
                                if let Err(err) = handle(&mut tls_stream, peer.ip(), state.clone(), true).await {
                                    debug!(%peer, error = %err, "admin TLS connection ended");
                                }
                            }
                            Ok(Err(err)) => debug!(%peer, error = %err, "admin TLS handshake failed"),
                            Err(_) => debug!(%peer, "admin TLS handshake timed out"),
                        }
                    });
                } else {
                    tokio::spawn(async move {
                        if let Err(err) = handle(&mut stream, peer.ip(), state, false).await {
                            debug!(%peer, error = %err, "admin connection ended");
                        }
                    });
                }
            }
        }
    }
    Ok(())
}

async fn handle<S>(
    stream: &mut S,
    peer_ip: IpAddr,
    state: AdminState,
    tls: bool,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut request = match read_request(stream).await {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(response) => {
            let bytes = render(response, tls, state.config.admin.tls.hsts);
            stream.write_all(&bytes).await?;
            stream.shutdown().await?;
            return Ok(());
        }
    };
    request.secure = tls || forwarded_https(&request.headers, &state.config);

    let response = route(&request, peer_ip, &state).await;
    let bytes = render(response, request.secure, state.config.admin.tls.hsts);
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}

fn forwarded_https(head: &str, config: &ServerConfig) -> bool {
    config.admin.trust_forwarded_proto
        && header_value(head, "x-forwarded-proto")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"))
}

fn render(response: HttpResponse, secure: bool, hsts: bool) -> Vec<u8> {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: {}\r\nX-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'\r\n",
        response.status,
        response.content_type,
        response.body.len(),
        response.cache_control
    );
    if secure && hsts {
        head.push_str("Strict-Transport-Security: max-age=31536000; includeSubDomains\r\n");
    }
    if let Some(cookie) = response.cookie {
        head.push_str("Set-Cookie: ");
        head.push_str(&cookie);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&response.body);
    bytes
}

/// 读取请求头；body 大小受限。返回 `None` 表示连接被对端提前关闭。
async fn read_request<S>(stream: &mut S) -> Result<Option<HttpRequest>, HttpResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut raw = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    let head_end = loop {
        let read = timeout(READ_TIMEOUT, stream.read(&mut buffer))
            .await
            .map_err(|_| HttpResponse::text(408, "request timeout"))?
            .map_err(|_| HttpResponse::text(400, "read request failed"))?;
        if read == 0 {
            if raw.is_empty() {
                return Ok(None);
            }
            return Err(HttpResponse::text(400, "incomplete request"));
        }
        raw.extend_from_slice(&buffer[..read]);
        if raw.len() > MAX_HEAD {
            return Err(HttpResponse::text(413, "request header too large"));
        }
        if let Some(pos) = find_head_end(&raw) {
            break pos;
        }
    };

    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() {
        return Err(HttpResponse::text(400, "malformed request line"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.to_string();
    let query = query.to_string();
    let content_length = header_value(&head, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY {
        return Err(HttpResponse::text(413, "request body too large"));
    }

    let mut body = raw[head_end..].to_vec();
    while body.len() < content_length {
        let read = timeout(READ_TIMEOUT, stream.read(&mut buffer))
            .await
            .map_err(|_| HttpResponse::text(408, "request body timeout"))?
            .map_err(|_| HttpResponse::text(400, "read request body failed"))?;
        if read == 0 {
            return Err(HttpResponse::text(400, "incomplete request body"));
        }
        body.extend_from_slice(&buffer[..read]);
        if body.len() > MAX_BODY {
            return Err(HttpResponse::text(413, "request body too large"));
        }
    }
    body.truncate(content_length);

    Ok(Some(HttpRequest {
        method,
        path,
        query,
        headers: head,
        body,
        secure: false,
    }))
}

fn find_head_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        candidate
            .trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

fn cookie_value(head: &str, name: &str) -> Option<String> {
    header_value(head, "cookie")?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn bearer_matches(head: &str, token: &str) -> bool {
    header_value(head, "authorization")
        .and_then(|value| value.strip_prefix("Bearer ").map(str::to_string))
        .is_some_and(|candidate| constant_time_eq(candidate.as_bytes(), token.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

fn csrf_matches(head: &str, session: &Session) -> bool {
    header_value(head, "x-meatus-csrf")
        .is_some_and(|value| constant_time_eq(value.as_bytes(), session.csrf.as_bytes()))
}

fn is_api(path: &str) -> bool {
    path.starts_with("/api/")
}

async fn route(request: &HttpRequest, peer_ip: IpAddr, state: &AdminState) -> HttpResponse {
    if let Some(id) = request.path.strip_prefix("/api/v1/clients/") {
        return clients_item(request, state, id).await;
    }
    if request.method == "POST"
        && let Some(name) = request
            .path
            .strip_prefix("/api/v1/proxies/")
            .and_then(|rest| rest.strip_suffix("/probe"))
    {
        return proxy_probe(request, state, name).await;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/healthz") => HttpResponse::json(
            200,
            json!({"status": "ok", "uptime_s": state.metrics.uptime_secs()}),
        ),
        ("GET", "/api/v1/session") => session_status(request, state),
        ("POST", "/api/v1/session") => login(request, peer_ip, state),
        ("DELETE", "/api/v1/session") => logout(request, state),
        ("GET", "/api/v1/status") => {
            if !authorized(request, state) {
                return HttpResponse::json(401, json!({"error": "unauthorized"}));
            }
            HttpResponse::json(
                200,
                state.status.render(
                    &state.metrics,
                    &state.quotas,
                    state.runtime.config().as_ref(),
                ),
            )
        }
        ("GET", "/metrics") => {
            if !authorized(request, state) {
                return HttpResponse::json(401, json!({"error": "unauthorized"}));
            }
            HttpResponse {
                status: 200,
                content_type: "text/plain; version=0.0.4; charset=utf-8",
                body: state.metrics.render().into_bytes(),
                cookie: None,
                cache_control: "no-store",
            }
        }
        ("GET", "/api/v1/clients") => clients_list(request, state),
        ("POST", "/api/v1/clients") => clients_create(request, state).await,
        ("POST", "/api/v1/proxies") => proxy_write(request, state, false).await,
        ("PUT", "/api/v1/proxies") => proxy_write(request, state, true).await,
        ("DELETE", "/api/v1/proxies") => proxy_delete(request, state).await,
        ("PUT", "/api/v1/settings") => settings_write(request, state).await,
        ("POST", "/api/v1/config/reload") => config_reload(request, state),
        ("GET", "/api/v1/settings") => {
            if !authorized(request, state) {
                return HttpResponse::json(401, json!({"error": "unauthorized"}));
            }
            HttpResponse::json(200, settings_json(state))
        }
        ("GET", "/api/v1/certificates") => {
            if !authorized(request, state) {
                return HttpResponse::json(401, json!({"error": "unauthorized"}));
            }
            HttpResponse::json(200, certificates_json(state))
        }
        ("GET", "/api/v1/logs") => {
            if !authorized(request, state) {
                return HttpResponse::json(401, json!({"error": "unauthorized"}));
            }
            let limit = query_value(&request.query, "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(200)
                .min(1_000);
            let lines = state.logs.snapshot(limit);
            HttpResponse::json(200, json!({"lines": lines, "total": state.logs.len()}))
        }
        ("GET", _) if !is_api(&request.path) => {
            if !state.config.admin.panel {
                return HttpResponse::text(404, "panel disabled");
            }
            serve_asset(&request.path)
        }
        _ => HttpResponse::text(404, "not found"),
    }
}

async fn proxy_write(request: &HttpRequest, state: &AdminState, replace: bool) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
    };
    let run_id = body
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if run_id.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "run_id is required"}));
    }
    let proxy = match new_proxy_from_json(&body) {
        Ok(proxy) => proxy,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let name = proxy.name.clone();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let command = AdminProxyCommand::Set {
        request: proxy,
        replace,
        reply: reply_tx,
    };
    if let Err(error) = timeout(
        Duration::from_secs(3),
        state.admin_bus.send(run_id, command),
    )
    .await
    {
        return HttpResponse::json(
            409,
            json!({"error": format!("client command timeout: {error}")}),
        );
    }
    match timeout(Duration::from_secs(15), reply_rx).await {
        Ok(Ok(Ok(ok))) => HttpResponse::json(
            200,
            json!({
                "ok": true,
                "name": ok.name,
                "remote_port": ok.remote_port,
                "domains": ok.domains,
            }),
        ),
        Ok(Ok(Err(error))) => HttpResponse::json(
            proxy_error_status(&error),
            json!({"error": error, "name": name}),
        ),
        Ok(Err(_)) => HttpResponse::json(502, json!({"error": "client command channel closed"})),
        Err(_) => HttpResponse::json(504, json!({"error": "client command timeout"})),
    }
}

async fn proxy_probe(request: &HttpRequest, state: &AdminState, name: &str) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    if !valid_proxy_name(name) {
        return HttpResponse::json(400, json!({"error": "invalid proxy name"}));
    }
    let body: Value = if request.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&request.body) {
            Ok(value) => value,
            Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
        }
    };
    let run_id = body
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let entries = state.status.proxy_entries(name, run_id);
    let entry = match entries.as_slice() {
        [] => return HttpResponse::json(404, json!({"error": format!("proxy {name} not found")})),
        [entry] => entry.clone(),
        _ => {
            return HttpResponse::json(
                409,
                json!({"error": "proxy name is ambiguous; pass run_id"}),
            );
        }
    };
    if !state.status.client_supports(&entry.run_id, "probe") {
        return HttpResponse::json(
            409,
            json!({
                "name": name,
                "supported": false,
                "ok": false,
                "error": "当前客户端不支持诊断，请升级客户端到 1.2.1 或更高版本",
            }),
        );
    }
    if !probe_rate_allowed(state, &entry.run_id, name, &entry.proxy_type) {
        return HttpResponse::json(
            429,
            json!({"error": "probe rate limited: 1 request/second"}),
        );
    }

    let timeout_ms = 3_000u32;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let command = AdminProxyCommand::Probe {
        proxy_name: name.to_string(),
        timeout_ms,
        reply: reply_tx,
    };
    if let Err(error) = state.admin_bus.send(&entry.run_id, command).await {
        return HttpResponse::json(409, json!({"error": error}));
    }
    match timeout(Duration::from_secs(5), reply_rx).await {
        Ok(Ok(Ok(result))) => {
            state.metrics.probe_finished(result.ok);
            HttpResponse::json(
                200,
                json!({
                    "name": name,
                    "supported": true,
                    "ok": result.ok,
                    "latency_ms": result.latency_ms,
                    "error_code": result.error_code,
                    "message": result.message,
                }),
            )
        }
        Ok(Ok(Err(error))) => HttpResponse::json(
            probe_error_status(&error),
            json!({"name": name, "supported": true, "ok": false, "error": error}),
        ),
        Ok(Err(_)) => HttpResponse::json(502, json!({"error": "client command channel closed"})),
        Err(_) => HttpResponse::json(504, json!({"error": "probe timeout"})),
    }
}

fn probe_error_status(error: &str) -> u16 {
    if error.contains("不支持") {
        409
    } else if error.contains("timeout") {
        504
    } else {
        502
    }
}

fn probe_rate_allowed(state: &AdminState, run_id: &str, name: &str, proxy_type: &str) -> bool {
    let key = format!("{run_id}\0{name}\0{proxy_type}");
    let now = Instant::now();
    let mut probes = state
        .probes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if probes
        .get(&key)
        .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(1))
    {
        return false;
    }
    if probes.len() > 512 {
        probes.retain(|_, last| now.duration_since(*last) < Duration::from_secs(60));
    }
    probes.insert(key, now);
    true
}

async fn proxy_delete(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
    };
    let run_id = body
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
    if run_id.trim().is_empty() || name.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "run_id and name are required"}));
    }
    if !valid_proxy_name(name) {
        return HttpResponse::json(400, json!({"error": "invalid proxy name"}));
    }
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let command = AdminProxyCommand::Remove {
        name: name.to_string(),
        reply: reply_tx,
    };
    if let Err(error) = timeout(
        Duration::from_secs(3),
        state.admin_bus.send(run_id, command),
    )
    .await
    {
        return HttpResponse::json(
            409,
            json!({"error": format!("client command timeout: {error}")}),
        );
    }
    match timeout(Duration::from_secs(15), reply_rx).await {
        Ok(Ok(Ok(()))) => HttpResponse::json(200, json!({"ok": true, "name": name})),
        Ok(Ok(Err(error))) => HttpResponse::json(
            proxy_error_status(&error),
            json!({"error": error, "name": name}),
        ),
        Ok(Err(_)) => HttpResponse::json(502, json!({"error": "client command channel closed"})),
        Err(_) => HttpResponse::json(504, json!({"error": "client command timeout"})),
    }
}

fn clients_list(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if !authorized(request, state) {
        return HttpResponse::json(401, json!({"error": "unauthorized"}));
    }
    let config = state.current_config();
    let config = config.as_ref();
    let mut rows = configured_clients(state);
    let snapshot = state.status.render(
        &state.metrics,
        &state.quotas,
        state.runtime.config().as_ref(),
    );
    let online = snapshot
        .get("clients")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in &mut rows {
        let id = row["id"].as_str().unwrap_or_default().to_string();
        let tenant = row["tenant"].as_str().unwrap_or_default().to_string();
        let mut proxy_count = 0u64;
        let mut online_runs = 0u64;
        let mut last_connected = 0i64;
        for entry in &online {
            if entry["client_id"].as_str() == Some(id.as_str())
                && entry["tenant_id"].as_str() == Some(tenant.as_str())
            {
                online_runs += 1;
                proxy_count += entry["proxy_count"].as_u64().unwrap_or(0);
                last_connected = last_connected.max(entry["connected_at_ms"].as_i64().unwrap_or(0));
            }
        }
        row["online"] = json!(online_runs > 0);
        row["run_count"] = json!(online_runs);
        row["proxy_count"] = json!(proxy_count);
        row["last_connected_ms"] = json!(last_connected);
        row["can_delete"] =
            json!(row["source"].as_str() == Some("clients") && config.auth.clients.len() >= 2);
    }
    let (server_addr, server_name) = default_client_endpoint(config);
    HttpResponse::json(
        200,
        json!({
            "clients": rows,
            "server_addr": server_addr,
            "server_name": server_name,
            "config_path": state.config_path.as_ref().map(|path| path.display().to_string()),
            "config_format": state.config_path.as_deref().map(|path| ConfigFormat::from_path(path).label()),
            "restart_required": false,
            "restart_delay_s": 0,
        }),
    )
}

fn configured_clients(state: &AdminState) -> Vec<Value> {
    let config = state.current_config();
    if config.auth.clients.is_empty() {
        return config
            .auth
            .token_sha256
            .as_deref()
            .filter(|hash| !hash.trim().is_empty())
            .map(|hash| {
                vec![json!({
                    "id": "default",
                    "tenant": meatus_core::DEFAULT_TENANT,
                    "token_fingerprint": fingerprint(hash),
                    "allow_ports": Value::Null,
                    "allow_types": [],
                    "allow_domains": [],
                    "max_proxies": Value::Null,
                    "limits": {},
                    "source": "token_sha256",
                    "can_delete": false,
                })]
            })
            .unwrap_or_default();
    }
    config
        .auth
        .clients
        .iter()
        .map(|client| {
            json!({
                "id": client.id,
                "tenant": client.tenant,
                "token_fingerprint": client.token_sha256.as_deref().map(fingerprint).unwrap_or_else(|| "未配置".to_string()),
                "allow_ports": client.allow_ports,
                "allow_types": client.allow_types.iter().map(|kind| proxy_type_label(*kind)).collect::<Vec<_>>(),
                "allow_domains": client.allow_domains,
                "max_proxies": client.max_proxies,
                "limits": client_limits_json(&client.limits),
                "source": "clients",
                "can_delete": config.auth.clients.len() >= 2,
            })
        })
        .collect()
}

async fn clients_create(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    if !local_auth_enabled(state) {
        return HttpResponse::json(
            409,
            json!({"error": "客户端令牌管理要求 auth.mode=hmac 且 auth.provider=local"}),
        );
    }
    let Some(path) = state.config_path.as_deref() else {
        return HttpResponse::json(
            409,
            json!({"error": "server was started without a config path; online edit is disabled"}),
        );
    };
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
    };
    let id = match validate_identifier(body.get("id").and_then(Value::as_str), "id") {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let tenant = match validate_identifier(
        body.get("tenant")
            .and_then(Value::as_str)
            .or(Some(meatus_core::DEFAULT_TENANT)),
        "tenant",
    ) {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let current = state.current_config();
    let current = current.as_ref();
    if current.auth.clients.iter().any(|client| client.id == id) {
        return HttpResponse::json(409, json!({"error": format!("client {id} already exists")}));
    }
    let allow_ports = match parse_allow_ports(body.get("allow_ports")) {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let (allow_types, allow_domains, max_proxies, limits) = match parse_client_policy(&body) {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let endpoint = match client_endpoint(body.get("server_addr").and_then(Value::as_str), current) {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let token = match random_client_token() {
        Ok(token) => token,
        Err(error) => return HttpResponse::json(500, json!({"error": error})),
    };
    let hash = hex::encode(meatus_core::token_key(&token));
    let entry = ClientEntry {
        id: id.clone(),
        tenant: tenant.clone(),
        token_sha256: Some(hash.clone()),
        allow_ports,
        allow_types,
        allow_domains,
        max_proxies,
        limits,
    };
    let backup = match meatus_core::upsert_server_client(path, &entry) {
        Ok(backup) => backup,
        Err(error) => {
            warn!(%error, client = %id, "client write failed");
            return HttpResponse::json(500, json!({"error": error}));
        }
    };
    let reload = match reload_config(state) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    warn!(client = %id, revision = %reload.revision, "client created and auth reloaded");
    let row = client_response_row(state, &entry);
    HttpResponse::json(
        200,
        json!({
            "ok": true,
            "client": row,
            "token": token,
            "config": client_snippets(ConfigFormat::from_path(path), &endpoint.0, &endpoint.1, &id, &tenant, &token),
            "restart_required": false,
            "restart_delay_s": 0,
            "config_version": reload.revision,
            "last_reload_ms": reload.last_reload_ms,
            "backup": backup.as_ref().map(|path| path.display().to_string()),
            "message": "客户端已创建并热加载生效；明文 token 只显示这一次。",
        }),
    )
}

async fn clients_item(request: &HttpRequest, state: &AdminState, id: &str) -> HttpResponse {
    let id = id.trim();
    if id.is_empty() {
        return HttpResponse::json(404, json!({"error": "client not found"}));
    }
    let action = if request.method == "PUT" {
        "rotate"
    } else if request.method == "DELETE" {
        "delete"
    } else {
        return HttpResponse::json(405, json!({"error": "method not allowed"}));
    };
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    if !local_auth_enabled(state) {
        return HttpResponse::json(
            409,
            json!({"error": "客户端令牌管理要求 auth.mode=hmac 且 auth.provider=local"}),
        );
    }
    let Some(path) = state.config_path.as_deref() else {
        return HttpResponse::json(
            409,
            json!({"error": "server was started without a config path; online edit is disabled"}),
        );
    };
    let current = state.current_config();
    let current = current.as_ref();
    if current.auth.clients.is_empty() {
        return HttpResponse::json(
            409,
            json!({"error": "当前为 auth.token_sha256 简写，请先新增一个 auth.clients 客户端再轮换或删除"}),
        );
    }
    let Some(existing) = current
        .auth
        .clients
        .iter()
        .find(|client| client.id == id)
        .cloned()
    else {
        return HttpResponse::json(404, json!({"error": format!("client {id} not found")}));
    };

    if action == "delete" {
        if current.auth.clients.len() <= 1 {
            return HttpResponse::json(409, json!({"error": "不能删除最后一个客户端凭据"}));
        }
        let backup = match meatus_core::remove_server_client(path, id) {
            Ok(backup) => backup,
            Err(error) => {
                warn!(%error, client = %id, "client delete failed");
                return HttpResponse::json(500, json!({"error": error}));
            }
        };
        let reload = match reload_config(state) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        };
        warn!(client = %id, revision = %reload.revision, "client deleted and auth reloaded");
        return HttpResponse::json(
            200,
            json!({
                "ok": true,
                "id": id,
                "restart_required": false,
                "restart_delay_s": 0,
                "config_version": reload.revision,
                "last_reload_ms": reload.last_reload_ms,
                "backup": backup.as_ref().map(|path| path.display().to_string()),
            }),
        );
    }

    let body: Value = if request.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&request.body) {
            Ok(value) => value,
            Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
        }
    };
    let allow_ports = match body.get("allow_ports") {
        Some(value) => match parse_allow_ports(Some(value)) {
            Ok(value) => value,
            Err(error) => return HttpResponse::json(400, json!({"error": error})),
        },
        None => existing.allow_ports,
    };
    let endpoint = match client_endpoint(body.get("server_addr").and_then(Value::as_str), current) {
        Ok(value) => value,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    let token = match random_client_token() {
        Ok(token) => token,
        Err(error) => return HttpResponse::json(500, json!({"error": error})),
    };
    let hash = hex::encode(meatus_core::token_key(&token));
    let entry = ClientEntry {
        id: existing.id.clone(),
        tenant: existing.tenant.clone(),
        token_sha256: Some(hash),
        allow_ports,
        allow_types: existing.allow_types.clone(),
        allow_domains: existing.allow_domains.clone(),
        max_proxies: existing.max_proxies,
        limits: existing.limits.clone(),
    };
    let backup = match meatus_core::upsert_server_client(path, &entry) {
        Ok(backup) => backup,
        Err(error) => {
            warn!(%error, client = %id, "client rotate failed");
            return HttpResponse::json(500, json!({"error": error}));
        }
    };
    let reload = match reload_config(state) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };
    warn!(client = %id, revision = %reload.revision, "client token rotated and auth reloaded");
    HttpResponse::json(
        200,
        json!({
            "ok": true,
            "client": client_response_row(state, &entry),
            "token": token,
            "config": client_snippets(ConfigFormat::from_path(path), &endpoint.0, &endpoint.1, &entry.id, &entry.tenant, &token),
            "restart_required": false,
            "restart_delay_s": 0,
            "config_version": reload.revision,
            "last_reload_ms": reload.last_reload_ms,
            "backup": backup.as_ref().map(|path| path.display().to_string()),
            "message": "token 已轮换并热加载生效；旧 token 立即失效。明文 token 只显示这一次。",
        }),
    )
}

fn proxy_type_label(kind: meatus_core::ProxyType) -> &'static str {
    match kind {
        meatus_core::ProxyType::Tcp => "tcp",
        meatus_core::ProxyType::Udp => "udp",
        meatus_core::ProxyType::Http => "http",
        meatus_core::ProxyType::Https => "https",
        meatus_core::ProxyType::Socks5 => "socks5",
    }
}

fn client_limits_json(limits: &meatus_core::ClientLimits) -> Value {
    json!({
        "rate_limit_bps": limits.rate_limit_bps,
        "monthly_bytes": limits.monthly_bytes,
        "max_conns": limits.max_conns,
        "conn_rate_per_s": limits.conn_rate_per_s,
        "quota_action": limits.quota_action.map(|action| match action {
            meatus_core::QuotaAction::Reject => "reject",
            meatus_core::QuotaAction::Throttle => "throttle",
        }),
    })
}

type ClientPolicyInput = (
    Vec<meatus_core::ProxyType>,
    Vec<String>,
    Option<u32>,
    meatus_core::ClientLimits,
);

fn parse_client_policy(body: &Value) -> Result<ClientPolicyInput, String> {
    let mut allow_types = Vec::new();
    if let Some(array) = body.get("allow_types").and_then(Value::as_array) {
        for item in array {
            let raw = item.as_str().unwrap_or_default().trim();
            let kind = match raw {
                "tcp" => meatus_core::ProxyType::Tcp,
                "udp" => meatus_core::ProxyType::Udp,
                "http" => meatus_core::ProxyType::Http,
                "https" => meatus_core::ProxyType::Https,
                "socks5" => meatus_core::ProxyType::Socks5,
                other => return Err(format!("invalid allow_types entry: {other}")),
            };
            if !allow_types.contains(&kind) {
                allow_types.push(kind);
            }
        }
    }
    if allow_types.len() > 5 {
        return Err("allow_types supports at most 5 entries".to_string());
    }

    let mut allow_domains = Vec::new();
    if let Some(array) = body.get("allow_domains").and_then(Value::as_array) {
        for item in array {
            let domain = item.as_str().unwrap_or_default().trim();
            if domain.is_empty() {
                continue;
            }
            allow_domains.push(domain.to_ascii_lowercase());
        }
    }
    if allow_domains.len() > 16 {
        return Err("allow_domains supports at most 16 entries".to_string());
    }
    meatus_core::DomainPolicy::from_allow_domains(&allow_domains)
        .map_err(|err| format!("allow_domains invalid: {err}"))?;

    let max_proxies = body
        .get("max_proxies")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .map(|value| u32::try_from(value).unwrap_or(0));
    if max_proxies.is_some_and(|value| !(1..=1024).contains(&value)) {
        return Err("max_proxies must be 1..=1024".to_string());
    }

    let limits = body.get("limits").cloned().unwrap_or_else(|| json!({}));
    let opt_u64 = |key: &str| {
        limits
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
    };
    let opt_u32 = |key: &str| {
        limits
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
    };
    let max_conns = opt_u32("max_conns");
    if max_conns.is_some_and(|value| value > 65535) {
        return Err("limits.max_conns must be <=65535".to_string());
    }
    let conn_rate_per_s = opt_u32("conn_rate_per_s");
    if conn_rate_per_s.is_some_and(|value| value > 100_000) {
        return Err("limits.conn_rate_per_s must be <=100000".to_string());
    }
    let quota_action = match limits.get("quota_action").and_then(Value::as_str) {
        Some("reject") => Some(meatus_core::QuotaAction::Reject),
        Some("throttle") => Some(meatus_core::QuotaAction::Throttle),
        Some("") | None => None,
        Some(other) => return Err(format!("invalid quota_action: {other}")),
    };
    Ok((
        allow_types,
        allow_domains,
        max_proxies,
        meatus_core::ClientLimits {
            rate_limit_bps: opt_u64("rate_limit_bps"),
            monthly_bytes: opt_u64("monthly_bytes"),
            max_conns,
            conn_rate_per_s,
            quota_action,
        },
    ))
}

fn client_response_row(state: &AdminState, entry: &ClientEntry) -> Value {
    let snapshot = state.status.render(
        &state.metrics,
        &state.quotas,
        state.runtime.config().as_ref(),
    );
    let online = snapshot
        .get("clients")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let runs: Vec<&Value> = online
        .iter()
        .filter(|value| {
            value["client_id"].as_str() == Some(entry.id.as_str())
                && value["tenant_id"].as_str() == Some(entry.tenant.as_str())
        })
        .collect();
    json!({
        "id": entry.id,
        "tenant": entry.tenant,
        "token_fingerprint": entry.token_sha256.as_deref().map(fingerprint).unwrap_or_else(|| "未配置".to_string()),
        "allow_ports": entry.allow_ports,
        "allow_types": entry.allow_types.iter().map(|kind| proxy_type_label(*kind)).collect::<Vec<_>>(),
        "allow_domains": entry.allow_domains,
        "max_proxies": entry.max_proxies,
        "limits": client_limits_json(&entry.limits),
        "online": !runs.is_empty(),
        "proxy_count": runs.iter().map(|value| value["proxy_count"].as_u64().unwrap_or(0)).sum::<u64>(),
    })
}

fn local_auth_enabled(state: &AdminState) -> bool {
    let config = state.current_config();
    config.auth.mode == AuthMode::Hmac
        && matches!(
            config.auth.provider,
            meatus_core::config::AuthProviderKind::Local
        )
}

fn validate_identifier(value: Option<&str>, field: &str) -> Result<String, String> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty() {
        return Err(format!("{field} is required"));
    }
    if value.len() > 64 {
        return Err(format!("{field} must be <=64 characters"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(format!(
            "{field} only supports letters, digits, '-', '_' and '.'"
        ));
    }
    Ok(value.to_string())
}

fn parse_allow_ports(value: Option<&Value>) -> Result<Option<[u16; 2]>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(array) = value.as_array() else {
        return Err("allow_ports must be [start, end] or null".to_string());
    };
    if array.len() != 2 {
        return Err("allow_ports must contain exactly two ports".to_string());
    };
    let start = array[0]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| "allow_ports[0] is invalid".to_string())?;
    let end = array[1]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| "allow_ports[1] is invalid".to_string())?;
    if start < 1024 || start > end {
        return Err("allow_ports must satisfy 1024 <= start <= end <= 65535".to_string());
    }
    Ok(Some([start, end]))
}

fn default_client_endpoint(config: &ServerConfig) -> (String, String) {
    let domain = config.http.domain.trim();
    let host = if !domain.is_empty()
        && !domain.contains('*')
        && meatus_core::normalize_domain(domain).is_ok()
    {
        meatus_core::normalize_domain(domain).unwrap_or_else(|_| "tunnel.example.com".to_string())
    } else if config.server.bind_addr != "0.0.0.0" && config.server.bind_addr != "::" {
        config.server.bind_addr.clone()
    } else {
        "tunnel.example.com".to_string()
    };
    let server_name = host.clone();
    let addr = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", config.server.control_port)
    } else {
        format!("{host}:{}", config.server.control_port)
    };
    (addr, server_name)
}

fn client_endpoint(value: Option<&str>, config: &ServerConfig) -> Result<(String, String), String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(default_client_endpoint(config));
    };
    if value.chars().any(char::is_whitespace) || value.contains("://") {
        return Err("server_addr must be host:port without scheme or whitespace".to_string());
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| "server_addr IPv6 form must be [addr]:port".to_string())?;
        (format!("[{host}]"), port)
    } else {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| "server_addr must be host:port".to_string())?;
        (host.to_string(), port)
    };
    let port: u16 = port
        .parse()
        .map_err(|_| "server_addr port must be 1..65535".to_string())?;
    if port == 0 {
        return Err("server_addr port must be 1..65535".to_string());
    }
    let bare = host.trim_matches(['[', ']']);
    let name = if let Ok(ip) = bare.parse::<IpAddr>() {
        ip.to_string()
    } else {
        meatus_core::normalize_domain(bare)
            .map_err(|error| format!("server_addr host invalid: {error}"))?
    };
    let addr = if bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        format!("[{name}]:{port}")
    } else {
        format!("{name}:{port}")
    };
    Ok((addr, name))
}

fn random_client_token() -> Result<String, String> {
    use base64::Engine as _;
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|error| format!("generate client token: {error}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

fn fingerprint(hash: &str) -> String {
    if hash.len() <= 16 {
        return hash.to_string();
    }
    format!("{}…{}", &hash[..8], &hash[hash.len() - 8..])
}

fn client_snippets(
    format: ConfigFormat,
    server_addr: &str,
    server_name: &str,
    id: &str,
    tenant: &str,
    token: &str,
) -> Value {
    let toml = format!(
        "[client]\nserver_addr = \"{server_addr}\"\nserver_name = \"{server_name}\"\nclient_id = \"{id}\"\ntenant = \"{tenant}\"\ntoken = \"{token}\"\n# server_ca_cert = \"certs/server.crt\"\n\n[log]\nlevel = \"info\"\nformat = \"text\"\n"
    );
    let yaml = format!(
        "client:\n  server_addr: \"{server_addr}\"\n  server_name: \"{server_name}\"\n  client_id: \"{id}\"\n  tenant: \"{tenant}\"\n  token: \"{token}\"\n  # server_ca_cert: \"certs/server.crt\"\n\nlog:\n  level: info\n  format: text\n"
    );
    json!({
        "toml": toml,
        "yaml": yaml,
        "format": format.label(),
    })
}

async fn settings_write(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    let Some(path) = state.config_path.as_deref() else {
        return HttpResponse::json(
            409,
            json!({"error": "server was started without a config path; online edit is disabled"}),
        );
    };
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
    };
    let current = state.current_config();
    let next = match merge_server_settings(&current, &body) {
        Ok(next) => next,
        Err(error) => return HttpResponse::json(400, json!({"error": error})),
    };
    if let Err(error) = next.validate_runtime() {
        return HttpResponse::json(400, json!({"error": error.to_string()}));
    }
    if let Err(error) = preflight_binds(&state.config, &next).await {
        return HttpResponse::json(400, json!({"error": error}));
    }
    let config_format = ConfigFormat::from_path(path);
    let backup = match meatus_core::write_server_config(path, &next) {
        Ok(backup) => backup,
        Err(error) => {
            warn!(%error, "server settings write failed");
            return HttpResponse::json(500, json!({"error": error}));
        }
    };

    let reload = match reload_config(state) {
        Ok(snapshot) => snapshot,
        Err(response) => return response,
    };

    warn!(
        path = %path.display(),
        format = config_format.label(),
        revision = %reload.revision,
        restart_required = ?reload.restart_required,
        "server settings written and reloaded"
    );
    let restart_needed = !reload.restart_required.is_empty();
    let message = if restart_needed {
        format!(
            "配置已写入并热加载；以下字段需要重启后生效：{}。",
            reload.restart_required.join("、")
        )
    } else {
        "配置已写入并热加载，无需重启。".to_string()
    };

    HttpResponse::json(
        200,
        json!({
            "ok": true,
            "restart_required": restart_needed,
            "restart_delay_s": 0,
            "applied": true,
            "config_version": reload.revision,
            "last_reload_ms": reload.last_reload_ms,
            "restart_fields": reload.restart_required,
            "config_path": path.display().to_string(),
            "config_format": config_format.label(),
            "backup": backup.as_ref().map(|path| path.display().to_string()),
            "message": message,
        }),
    )
}

fn config_reload(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if let Err(response) = write_allowed(request, state) {
        return response;
    }
    match reload_config(state) {
        Ok(snapshot) => {
            let restart_needed = !snapshot.restart_required.is_empty();
            let message = if restart_needed {
                format!(
                    "配置已重载；以下字段需要重启后生效：{}。",
                    snapshot.restart_required.join("、")
                )
            } else {
                "配置已重载，无需重启。".to_string()
            };
            HttpResponse::json(
                200,
                json!({
                    "ok": true,
                    "config_version": snapshot.revision,
                    "last_reload_ms": snapshot.last_reload_ms,
                    "reload_count": snapshot.reload_count,
                    "restart_required": restart_needed,
                    "restart_fields": snapshot.restart_required,
                    "last_error": snapshot.last_error,
                    "message": message,
                }),
            )
        }
        Err(response) => {
            let snapshot = state.runtime.snapshot();
            let mut body = match serde_json::from_slice::<Value>(&response.body) {
                Ok(Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };
            body.insert("ok".to_string(), json!(false));
            body.insert("config_version".to_string(), json!(snapshot.revision));
            body.insert("last_reload_ms".to_string(), json!(snapshot.last_reload_ms));
            body.insert("last_error".to_string(), json!(snapshot.last_error));
            HttpResponse::json(response.status, Value::Object(body))
        }
    }
}

fn reload_config(state: &AdminState) -> Result<crate::ReloadSnapshot, HttpResponse> {
    let Some(reloader) = &state.reloader else {
        return Err(HttpResponse::json(
            409,
            json!({"error": "server was started without a config path; hot reload is disabled"}),
        ));
    };
    reloader.reload().map_err(|error| {
        HttpResponse::json(
            500,
            json!({
                "error": format!("配置已写入但热加载失败；旧配置继续生效：{error}"),
                "kept_previous": true,
                "last_error": error.to_string(),
            }),
        )
    })
}

fn merge_server_settings(current: &ServerConfig, body: &Value) -> Result<ServerConfig, String> {
    let mut next = current.clone();

    if let Some(section) = body.get("server").and_then(Value::as_object) {
        if let Some(value) = section.get("bind").and_then(Value::as_str) {
            next.server.bind_addr = non_empty(value, "server.bind")?;
        }
        if let Some(value) = section.get("control_port").and_then(Value::as_u64) {
            next.server.control_port = u16::try_from(value)
                .map_err(|_| "server.control_port must be 1..65535".to_string())?;
            if next.server.control_port == 0 {
                return Err("server.control_port must be 1..65535".to_string());
            }
        }
    }

    if let Some(section) = body.get("tenant").and_then(Value::as_object)
        && let Some(value) = section.get("default").and_then(Value::as_str)
    {
        next.tenant.default = non_empty(value, "tenant.default")?;
    }

    if let Some(section) = body.get("ports").and_then(Value::as_object) {
        if let Some(range) = section.get("range").and_then(Value::as_array) {
            if range.len() != 2 {
                return Err("ports.range must contain exactly two ports".to_string());
            }
            let start = range[0]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .ok_or_else(|| "ports.range[0] is invalid".to_string())?;
            let end = range[1]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .ok_or_else(|| "ports.range[1] is invalid".to_string())?;
            next.ports.range = [start, end];
        }
        if let Some(value) = section.get("release_policy").and_then(Value::as_str) {
            next.ports.release_policy = match value.trim().to_ascii_lowercase().as_str() {
                "immediate" => PortReleasePolicy::Immediate,
                "hold" => PortReleasePolicy::Hold,
                _ => return Err("ports.release_policy must be immediate or hold".to_string()),
            };
        }
        if let Some(value) = section.get("hold_secs").and_then(Value::as_u64) {
            next.ports.hold_secs = value;
        }
    }

    if let Some(section) = body.get("http").and_then(Value::as_object) {
        set_bool(&mut next.http.enabled, section, "enabled")?;
        set_field(&mut next.http.bind, section, "bind", "http.bind")?;
        set_field(
            &mut next.http.https_bind,
            section,
            "https_bind",
            "http.https_bind",
        )?;
        if let Some(value) = section.get("domain").and_then(Value::as_str) {
            next.http.domain = value.trim().to_string();
        }
    }

    if let Some(section) = body.get("acme").and_then(Value::as_object) {
        set_bool(&mut next.acme.enabled, section, "enabled")?;
        if let Some(value) = section.get("email") {
            if !value.is_null() && !value.is_string() {
                return Err("acme.email must be a string".to_string());
            }
            next.acme.email = value
                .as_str()
                .map(str::trim)
                .filter(|email| !email.is_empty())
                .map(ToString::to_string);
        }
        set_bool(&mut next.acme.staging, section, "staging")?;
        if let Some(value) = section.get("challenge").and_then(Value::as_str) {
            next.acme.challenge = match value.trim() {
                "http-01" => meatus_core::AcmeChallenge::Http01,
                "dns-01" => meatus_core::AcmeChallenge::Dns01,
                "tls-alpn-01" => meatus_core::AcmeChallenge::TlsAlpn01,
                other => {
                    return Err(format!(
                        "acme.challenge must be http-01/dns-01/tls-alpn-01, got {other}"
                    ));
                }
            };
        }
        if let Some(value) = section.get("dns_provider") {
            if !value.is_null() && !value.is_string() {
                return Err("acme.dns_provider must be a string".to_string());
            }
            next.acme.dns_provider = value
                .as_str()
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
                .map(ToString::to_string);
        }
        if let Some(value) = section.get("dns_api_token") {
            if !value.is_null() && !value.is_string() {
                return Err("acme.dns_api_token must be a string".to_string());
            }
            // 留空表示保留现有 token；只有显式传入非空值才覆盖。
            if let Some(token) = value
                .as_str()
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                next.acme.dns_api_token = Some(token.to_string());
            }
        }
    }

    if let Some(section) = body.get("socks5").and_then(Value::as_object) {
        set_bool(&mut next.socks5.enabled, section, "enabled")?;
        set_field(&mut next.socks5.bind, section, "bind", "socks5.bind")?;
    }

    if let Some(section) = body.get("log").and_then(Value::as_object) {
        if let Some(value) = section.get("level").and_then(Value::as_str) {
            let level = value.trim().to_ascii_lowercase();
            if !matches!(
                level.as_str(),
                "trace" | "debug" | "info" | "warn" | "error"
            ) {
                return Err("log.level must be trace/debug/info/warn/error".to_string());
            }
            next.log.level = level;
        }
        if let Some(value) = section.get("format").and_then(Value::as_str) {
            next.log.format = match value.trim().to_ascii_lowercase().as_str() {
                "text" => LogFormat::Text,
                "json" => LogFormat::Json,
                _ => return Err("log.format must be text or json".to_string()),
            };
        }
    }

    Ok(next)
}

fn non_empty(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(value.to_string())
}

fn set_field(
    target: &mut String,
    section: &serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<(), String> {
    if let Some(value) = section.get(key).and_then(Value::as_str) {
        *target = non_empty(value, field)?;
    }
    Ok(())
}

fn set_bool(
    target: &mut bool,
    section: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<(), String> {
    if let Some(value) = section.get(key) {
        *target = value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean"))?;
    }
    Ok(())
}

async fn preflight_binds(current: &ServerConfig, next: &ServerConfig) -> Result<(), String> {
    if next.server.control_port != current.server.control_port {
        test_bind(
            &format!("{}:{}", next.server.bind_addr, next.server.control_port),
            "server control",
        )
        .await?;
    }
    if next.http.enabled
        && (!current.http.enabled || bind_port(&next.http.bind) != bind_port(&current.http.bind))
    {
        test_bind(&next.http.bind, "http").await?;
    }
    if next.http.enabled
        && (!current.http.enabled
            || bind_port(&next.http.https_bind) != bind_port(&current.http.https_bind))
    {
        test_bind(&next.http.https_bind, "https").await?;
    }
    if next.socks5.enabled
        && (!current.socks5.enabled
            || bind_port(&next.socks5.bind) != bind_port(&current.socks5.bind))
    {
        test_bind(&next.socks5.bind, "socks5").await?;
    }
    Ok(())
}

fn bind_port(bind: &str) -> Option<u16> {
    bind.rsplit_once(':')
        .and_then(|(_, port)| port.trim().parse::<u16>().ok())
}

async fn test_bind(bind: &str, label: &str) -> Result<(), String> {
    TcpListener::bind(bind)
        .await
        .map(drop)
        .map_err(|error| format!("{label} cannot bind {bind}: {error}"))
}

fn write_allowed(request: &HttpRequest, state: &AdminState) -> Result<(), HttpResponse> {
    if !authorized(request, state) {
        return Err(HttpResponse::json(401, json!({"error": "unauthorized"})));
    }
    let Some(token) = state.admin_token.as_deref() else {
        return Ok(());
    };
    if bearer_matches(&request.headers, token) {
        return Ok(());
    }
    let Some(session) = valid_session(request, state) else {
        return Err(HttpResponse::json(401, json!({"error": "unauthorized"})));
    };
    if !csrf_matches(&request.headers, &session) {
        return Err(HttpResponse::json(
            403,
            json!({"error": "csrf token required"}),
        ));
    }
    Ok(())
}

fn proxy_error_status(error: &str) -> u16 {
    if error.contains("not registered") {
        404
    } else if error.contains("already registered") {
        409
    } else if error.contains("timeout") || error.contains("channel") {
        504
    } else {
        400
    }
}

fn new_proxy_from_json(body: &Value) -> Result<NewProxy, String> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !valid_proxy_name(name) {
        return Err("name must match [A-Za-z0-9_.-]{1,64}".to_string());
    }
    let proxy_type = match body
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "tcp" => ProxyType::Tcp as i32,
        "udp" => ProxyType::Udp as i32,
        "http" => ProxyType::Http as i32,
        "https" => ProxyType::Https as i32,
        "socks5" => ProxyType::Socks5 as i32,
        other => return Err(format!("unsupported proxy type: {other}")),
    };
    let local_addr = body
        .get("local_addr")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if local_addr.is_empty() || local_addr.len() > 255 || local_addr.chars().any(char::is_control) {
        return Err("local_addr is required and must be <=255 bytes".to_string());
    }
    let remote_port = body.get("remote_port").and_then(Value::as_u64).unwrap_or(0);
    if remote_port > u64::from(u16::MAX) {
        return Err("remote_port is out of range".to_string());
    }
    let mut domains = Vec::new();
    if let Some(array) = body.get("custom_domains").and_then(Value::as_array) {
        for value in array {
            let Some(domain) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if domain.len() > 253 || domain.chars().any(char::is_whitespace) {
                return Err(format!("invalid domain: {domain}"));
            }
            domains.push(domain.to_ascii_lowercase());
        }
    }
    domains.sort();
    domains.dedup();
    if domains.len() > 16 {
        return Err("at most 16 custom_domains are allowed".to_string());
    }

    let options = body.get("options").cloned().unwrap_or_else(|| json!({}));
    let rate_limit_bps = options
        .get("rate_limit_bps")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let max_conns = options
        .get("max_conns")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if max_conns > 4096 {
        return Err("max_conns must be <=4096".to_string());
    }
    let monthly_bytes = options
        .get("monthly_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let conn_rate_per_s = options
        .get("conn_rate_per_s")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if conn_rate_per_s > 100_000 {
        return Err("conn_rate_per_s must be <=100000".to_string());
    }
    let quota_action = options
        .get("quota_action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !matches!(quota_action.as_str(), "" | "reject" | "throttle") {
        return Err("quota_action must be reject or throttle".to_string());
    }
    let idle_timeout_s = options
        .get("idle_timeout_s")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if idle_timeout_s > 3600 {
        return Err("idle_timeout_s must be <=3600".to_string());
    }
    let mut allow_cidrs = Vec::new();
    if let Some(array) = options.get("allow_cidrs").and_then(Value::as_array) {
        for value in array {
            let Some(cidr) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            allow_cidrs.push(cidr.to_string());
        }
    }
    if allow_cidrs.len() > 16 {
        return Err("at most 16 allow_cidrs are allowed".to_string());
    }
    let mut allow_domains = Vec::new();
    if let Some(array) = options.get("allow_domains").and_then(Value::as_array) {
        for value in array {
            let Some(domain) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            allow_domains.push(domain.to_string());
        }
    }
    if allow_domains.len() > 16 {
        return Err("at most 16 allow_domains are allowed".to_string());
    }
    meatus_core::DomainPolicy::from_allow_domains(&allow_domains)
        .map_err(|err| format!("allow_domains invalid: {err}"))?;

    let kind = ProxyType::try_from(proxy_type).map_err(|_| "invalid proxy type".to_string())?;
    if matches!(kind, ProxyType::Http | ProxyType::Https) {
        if domains.is_empty() {
            return Err("http/https proxy requires custom_domains".to_string());
        }
        if remote_port != 0 {
            return Err("http/https proxy must use remote_port=0".to_string());
        }
    } else if matches!(kind, ProxyType::Socks5) {
        if !domains.is_empty() {
            return Err("socks5 proxy must not set custom_domains".to_string());
        }
        if remote_port != 0 {
            return Err("socks5 proxy must use remote_port=0".to_string());
        }
    } else if !domains.is_empty() {
        return Err("tcp/udp proxy must not set custom_domains".to_string());
    }

    Ok(NewProxy {
        name: name.to_string(),
        proxy_type,
        local_addr: local_addr.to_string(),
        remote_port: u32::try_from(remote_port).unwrap_or(0),
        custom_domains: domains,
        options: Some(ProxyOptions {
            rate_limit_bps,
            idle_timeout_s: u32::try_from(idle_timeout_s).unwrap_or(0),
            max_conns: u32::try_from(max_conns).unwrap_or(0),
            enable_compression: false,
            monthly_bytes,
            conn_rate_per_s: u32::try_from(conn_rate_per_s).unwrap_or(0),
            quota_action,
            allow_cidrs,
            allow_domains,
        }),
    })
}

fn valid_proxy_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn authorized(request: &HttpRequest, state: &AdminState) -> bool {
    let Some(token) = state.admin_token.as_deref() else {
        return true;
    };
    if bearer_matches(&request.headers, token) {
        return true;
    }
    valid_session(request, state).is_some()
}

fn valid_session(request: &HttpRequest, state: &AdminState) -> Option<Session> {
    let id = cookie_value(&request.headers, COOKIE_NAME)?;
    let mut sessions = state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session = sessions.get_mut(&id)?;
    if session.last_seen.elapsed() > SESSION_TTL {
        sessions.remove(&id);
        return None;
    }
    session.last_seen = Instant::now();
    Some(session.clone())
}

fn session_status(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    if state.admin_token.is_none() {
        return HttpResponse::json(
            200,
            json!({"authenticated": true, "csrf": Value::Null, "mode": "loopback"}),
        );
    }
    match valid_session(request, state) {
        Some(session) => HttpResponse::json(
            200,
            json!({"authenticated": true, "csrf": session.csrf, "mode": "session"}),
        ),
        None => HttpResponse::json(
            200,
            json!({"authenticated": false, "csrf": Value::Null, "mode": "session"}),
        ),
    }
}

fn login(request: &HttpRequest, peer_ip: IpAddr, state: &AdminState) -> HttpResponse {
    let Some(token) = state.admin_token.as_deref() else {
        return HttpResponse::json(
            200,
            json!({"authenticated": true, "csrf": Value::Null, "mode": "loopback"}),
        );
    };

    if let Some(lock) = login_lock(state, peer_ip) {
        return HttpResponse::json(
            429,
            json!({"error": "too many failures", "retry_after_s": lock.as_secs() + 1}),
        );
    }

    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return HttpResponse::json(400, json!({"error": "invalid json"})),
    };
    let candidate = body
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !constant_time_eq(candidate.as_bytes(), token.as_bytes()) {
        record_login_failure(state, peer_ip);
        return HttpResponse::json(401, json!({"error": "invalid token"}));
    }

    clear_login_failure(state, peer_ip);
    let session_id = random_hex(32);
    let csrf = random_hex(32);
    let session = Session {
        csrf: csrf.clone(),
        last_seen: Instant::now(),
    };
    state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id.clone(), session);

    HttpResponse::json(
        200,
        json!({"authenticated": true, "csrf": csrf, "mode": "session"}),
    )
    .with_cookie(session_cookie(
        &session_id,
        request.secure,
        SESSION_TTL.as_secs(),
    ))
}

fn logout(request: &HttpRequest, state: &AdminState) -> HttpResponse {
    let Some(session) = valid_session(request, state) else {
        return HttpResponse::json(401, json!({"error": "unauthorized"}));
    };
    if !csrf_matches(&request.headers, &session) {
        return HttpResponse::json(403, json!({"error": "csrf token required"}));
    }
    if let Some(id) = cookie_value(&request.headers, COOKIE_NAME) {
        state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }
    HttpResponse::json(200, json!({"authenticated": false}))
        .with_cookie(clear_session_cookie(request.secure))
}

fn session_cookie(session_id: &str, secure: bool, max_age: u64) -> String {
    let mut cookie =
        format!("{COOKIE_NAME}={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn clear_session_cookie(secure: bool) -> String {
    let mut cookie = format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn login_lock(state: &AdminState, peer_ip: IpAddr) -> Option<Duration> {
    let now = Instant::now();
    let failures = state
        .failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let failure = failures.get(&peer_ip)?;
    if let Some(until) = failure.locked_until
        && until > now
    {
        return Some(until - now);
    }
    None
}

fn record_login_failure(state: &AdminState, peer_ip: IpAddr) {
    let now = Instant::now();
    let mut failures = state
        .failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let failure = failures.entry(peer_ip).or_insert(LoginFailure {
        count: 0,
        first_at: now,
        locked_until: None,
    });
    if now.duration_since(failure.first_at) > LOGIN_FAILURE_WINDOW {
        failure.count = 0;
        failure.first_at = now;
        failure.locked_until = None;
    }
    failure.count += 1;
    if failure.count >= LOGIN_MAX_FAILURES {
        failure.locked_until = Some(now + LOGIN_FAILURE_WINDOW);
    }
}

fn clear_login_failure(state: &AdminState, peer_ip: IpAddr) {
    state
        .failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&peer_ip);
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // 极端情况下退回时间戳，仍然保持足够长度以维持 Cookie 形态。
        let seed = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        );
        return seed.chars().cycle().take(bytes * 2).collect();
    }
    hex::encode(buf)
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (candidate, value) = pair.split_once('=')?;
        (candidate == key).then(|| value.to_string())
    })
}

fn settings_json(state: &AdminState) -> Value {
    let config = state.current_config();
    let config = config.as_ref();
    let auth = state.auth_summary();
    let reload = state.runtime.snapshot();
    json!({
        "server": {
            "bind": config.server.bind_addr,
            "control_port": config.server.control_port,
            "tls_cert": config.server.tls_cert.as_ref().map(|path| path.display().to_string()),
            "tls_configured": config.server.tls_cert.is_some(),
        },
        "admin": {
            "bind": config.admin.bind,
            "panel": config.admin.panel,
            "loopback": auth.loopback,
            "token_configured": auth.token_configured,
            "tls_enabled": config.admin.tls.enabled,
            "tls_configured": config.admin.tls.cert.is_some() && config.admin.tls.key.is_some(),
            "hsts": config.admin.tls.hsts,
            "trust_forwarded_proto": config.admin.trust_forwarded_proto,
        },
        "http": {
            "enabled": config.http.enabled,
            "bind": config.http.bind,
            "https_bind": config.http.https_bind,
            "domain": config.http.domain,
        },
        "acme": {
            "enabled": config.acme.enabled,
            "challenge": challenge_label(config.acme.challenge),
            "staging": config.acme.staging,
            "cert_dir": config.acme.cert_dir.display().to_string(),
            "email": config.acme.email.clone(),
            "email_configured": config.acme.email.as_deref().is_some_and(|value| !value.is_empty()),
            "dns_provider": config.acme.dns_provider.clone(),
            "dns_token_configured": config
                .acme
                .dns_api_token
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
        },
        "socks5": {
            "enabled": config.socks5.enabled,
            "bind": config.socks5.bind,
        },
        "log": {
            "level": config.log.level,
            "format": match config.log.format {
                LogFormat::Text => "text",
                LogFormat::Json => "json",
            },
        },
        "tenant": {
            "default": config.tenant.default,
        },
        "ports": {
            "range": config.ports.range,
            "release_policy": match config.ports.release_policy {
                PortReleasePolicy::Immediate => "immediate",
                PortReleasePolicy::Hold => "hold",
            },
            "hold_secs": config.ports.hold_secs,
        },
        "session": {
            "ttl_s": SESSION_TTL.as_secs(),
            "cookie_secure": false,
            "csrf_required": true,
            "login_max_failures": LOGIN_MAX_FAILURES,
            "login_lock_s": LOGIN_FAILURE_WINDOW.as_secs(),
        },
        "writes": {
            "methods": ["POST", "PUT", "DELETE"],
            "proxy_crud": true,
            "settings": state.config_path.is_some(),
            "config_path": state.config_path.as_ref().map(|path| path.display().to_string()),
            "config_format": state
                .config_path
                .as_deref()
                .map(|path| ConfigFormat::from_path(path).label()),
        },
        "reload": {
            "supported": state.reloader.is_some(),
            "config_version": reload.revision,
            "last_reload_ms": reload.last_reload_ms,
            "reload_count": reload.reload_count,
            "last_error": reload.last_error,
            "restart_required": reload.restart_required,
        },
    })
}

fn challenge_label(challenge: meatus_core::AcmeChallenge) -> &'static str {
    match challenge {
        meatus_core::AcmeChallenge::Http01 => "http-01",
        meatus_core::AcmeChallenge::TlsAlpn01 => "tls-alpn-01",
        meatus_core::AcmeChallenge::Dns01 => "dns-01",
    }
}

fn certificate_covers(cert: &CachedCertificate, domain: &str) -> bool {
    cert.domains.iter().any(|name| {
        name == domain
            || name.strip_prefix("*.").is_some_and(|suffix| {
                domain.len() > suffix.len() && domain.ends_with(&format!(".{suffix}"))
            })
    })
}

fn certificates_json(state: &AdminState) -> Value {
    let snapshot = state.status.render(
        &state.metrics,
        &state.quotas,
        state.runtime.config().as_ref(),
    );
    let cached = read_cached_certificates(&state.config.acme.cert_dir);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
        });
    let mut entries = Vec::new();
    for proxy in snapshot["proxies"].as_array().into_iter().flatten() {
        for domain in proxy["domains"].as_array().into_iter().flatten() {
            let Some(domain) = domain.as_str().filter(|value| !value.is_empty()) else {
                continue;
            };
            let matched = cached
                .iter()
                .filter(|cert| certificate_covers(cert, domain))
                .min_by_key(|cert| cert.not_after);
            let acme_status = state
                .acme
                .as_ref()
                .and_then(|acme| acme.domain_status(domain));
            let challenge = if state.config.acme.enabled {
                state.acme.as_ref().map_or_else(
                    || challenge_label(state.config.acme.challenge),
                    |acme| acme.challenge_label(domain),
                )
            } else {
                "none"
            };
            let (status, source, cert_fields) = if let Some(cert) = matched {
                let days_left = (cert.not_after - now) / 86_400;
                let status = if days_left < 0 {
                    "expired"
                } else if days_left <= 30 {
                    "expiring"
                } else {
                    "managed"
                };
                (
                    status,
                    if state.config.acme.enabled {
                        "acme"
                    } else {
                        "cache"
                    },
                    json!({
                        "not_after": cert.not_after,
                        "days_left": days_left,
                        "serial": cert.serial,
                        "issuer": cert.issuer,
                        "chain_length": cert.chain_length,
                    }),
                )
            } else {
                let status = if state.config.acme.enabled {
                    acme_status
                        .as_ref()
                        .map_or("pending", |status| status.state)
                } else {
                    "not-configured"
                };
                (
                    status,
                    if state.config.acme.enabled {
                        "acme"
                    } else {
                        "manual"
                    },
                    json!({
                        "not_after": Value::Null,
                        "days_left": Value::Null,
                        "serial": Value::Null,
                        "issuer": Value::Null,
                        "chain_length": 0,
                    }),
                )
            };
            let (Some(not_after), Some(days_left), Some(serial), Some(issuer), Some(chain_length)) = (
                cert_fields.get("not_after"),
                cert_fields.get("days_left"),
                cert_fields.get("serial"),
                cert_fields.get("issuer"),
                cert_fields.get("chain_length"),
            ) else {
                continue;
            };
            entries.push(json!({
                "domain": domain,
                "proxy": proxy["name"],
                "type": proxy["type"],
                "remote_port": proxy["remote_port"],
                "source": source,
                "challenge": challenge,
                "status": status,
                "acme_state": acme_status.as_ref().map(|status| status.state),
                "error": acme_status.as_ref().and_then(|status| status.error.clone()),
                "last_attempt": acme_status.as_ref().map(|status| status.updated_at),
                "not_after": not_after,
                "days_left": days_left,
                "serial": serial,
                "issuer": issuer,
                "chain_length": chain_length,
            }));
        }
    }
    entries.sort_by(|a, b| a["domain"].as_str().cmp(&b["domain"].as_str()));
    json!({
        "acme_enabled": state.config.acme.enabled,
        "staging": state.config.acme.staging,
        "cert_dir": state.config.acme.cert_dir.display().to_string(),
        "cached_count": cached.len(),
        "entries": entries,
    })
}

#[derive(Debug, Clone)]
struct CachedCertificate {
    domains: Vec<String>,
    not_after: i64,
    serial: String,
    issuer: String,
    chain_length: usize,
}

fn read_cached_certificates(dir: &std::path::Path) -> Vec<CachedCertificate> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut certificates = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("cert-") || !name.ends_with(".pem") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let chain: Vec<_> = match rustls_pemfile::certs(&mut Cursor::new(bytes.as_slice()))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(chain) if !chain.is_empty() => chain,
            _ => continue,
        };
        let Ok((_, cert)) = parse_x509_certificate(chain[0].as_ref()) else {
            continue;
        };
        let domains = cert
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|extension| {
                extension
                    .value
                    .general_names
                    .iter()
                    .filter_map(|name| match name {
                        GeneralName::DNSName(value) => Some((*value).to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let serial = hex::encode(cert.raw_serial());
        certificates.push(CachedCertificate {
            domains,
            not_after: cert.validity().not_after.timestamp(),
            serial,
            issuer: cert.issuer().to_string(),
            chain_length: chain.len(),
        });
    }
    certificates.sort_by_key(|cert| cert.not_after);
    certificates
}

fn serve_asset(path: &str) -> HttpResponse {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let safe = !path.split('/').any(|part| part == ".." || part == ".");
    if !safe {
        return HttpResponse::text(400, "invalid path");
    }

    if let Some(asset) = WebAssets::get(path) {
        if path == "index.html" {
            return HttpResponse::html(200, asset.data.into_owned());
        }
        return HttpResponse::asset(200, content_type(path), asset.data.into_owned());
    }

    // SPA 路由回退：非 /api、非静态资源路径返回 index.html。
    if !path.contains('.')
        && let Some(index) = WebAssets::get("index.html")
    {
        return HttpResponse::html(200, index.data.into_owned());
    }
    HttpResponse::text(404, "not found")
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn is_loopback_bind(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    let host = host.trim_matches(['[', ']']);
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_policy_validates_type_domains_and_limits() {
        let body = json!({
            "allow_types": ["tcp", "tcp"],
            "allow_domains": ["*.example.com"],
            "max_proxies": 3,
            "limits": { "monthly_bytes": 100, "conn_rate_per_s": 5, "quota_action": "throttle" },
        });
        let (types, domains, max, limits) = parse_client_policy(&body).expect("policy");
        assert_eq!(types, vec![meatus_core::ProxyType::Tcp]);
        assert_eq!(domains, vec!["*.example.com".to_string()]);
        assert_eq!(max, Some(3));
        assert_eq!(limits.monthly_bytes, Some(100));
        assert_eq!(
            limits.quota_action,
            Some(meatus_core::QuotaAction::Throttle)
        );

        assert!(
            parse_client_policy(&json!({ "allow_types": ["icmp"] })).is_err(),
            "unknown type must be rejected"
        );
        assert!(
            parse_client_policy(&json!({ "limits": { "max_conns": 70000 } })).is_err(),
            "max_conns above 65535 must be rejected"
        );
        assert!(
            parse_client_policy(&json!({ "max_proxies": 2000 })).is_err(),
            "max_proxies above 1024 must be rejected"
        );
    }

    #[test]
    fn cookie_parser_handles_multiple_entries() {
        let head = "GET / HTTP/1.1\r\nCookie: a=1; meatus_session=abc; b=2\r\n\r\n";
        assert_eq!(cookie_value(head, COOKIE_NAME).as_deref(), Some("abc"));
    }

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }

    #[test]
    fn content_type_mapping() {
        assert_eq!(
            content_type("assets/app.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
    }

    #[test]
    fn loopback_bind_detection() {
        assert!(is_loopback_bind("127.0.0.1:7400"));
        assert!(is_loopback_bind("[::1]:7400"));
        assert!(!is_loopback_bind("0.0.0.0:7400"));
    }
}

#[cfg(test)]
mod certificate_tests {
    use super::*;

    #[test]
    fn parses_cached_certificate_metadata() {
        let dir = std::env::temp_dir().join(format!("meatus-cert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let certified = rcgen::generate_simple_self_signed(vec![
            "app.example.com".to_string(),
            "www.example.com".to_string(),
        ])
        .expect("generate cert");
        std::fs::write(dir.join("cert-test.pem"), certified.cert.pem()).expect("write cert");

        let parsed = read_cached_certificates(&dir);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].domains,
            vec!["app.example.com", "www.example.com"]
        );
        assert!(!parsed[0].serial.is_empty());
        assert_eq!(parsed[0].chain_length, 1);
        assert!(parsed[0].not_after > 0);
    }

    #[test]
    fn cached_wildcard_certificate_covers_child_domains() {
        let cert = CachedCertificate {
            domains: vec!["*.example.com".to_string()],
            not_after: 0,
            serial: String::new(),
            issuer: String::new(),
            chain_length: 1,
        };
        assert!(certificate_covers(&cert, "*.example.com"));
        assert!(certificate_covers(&cert, "app.example.com"));
        assert!(certificate_covers(&cert, "a.b.example.com"));
        assert!(!certificate_covers(&cert, "example.com"));
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use meatus_core::AcmeChallenge;

    fn base_config() -> meatus_core::ServerConfig {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = false
email = "ops@example.com"
dns_api_token = "existing-token"
"#;
        meatus_core::parse_server_config(toml).expect("config")
    }

    #[test]
    fn settings_accept_dns01_and_keep_token_when_blank() {
        let current = base_config();
        let body = serde_json::json!({
            "acme": {
                "enabled": false,
                "challenge": "dns-01",
                "dns_provider": "cloudflare",
                "dns_api_token": ""
            }
        });
        let next = merge_server_settings(&current, &body).expect("merge");
        assert_eq!(next.acme.challenge, AcmeChallenge::Dns01);
        assert_eq!(next.acme.dns_provider.as_deref(), Some("cloudflare"));
        assert_eq!(next.acme.dns_api_token.as_deref(), Some("existing-token"));
    }

    #[test]
    fn settings_reject_unknown_challenge() {
        let current = base_config();
        let body = serde_json::json!({"acme": {"challenge": "http-03"}});
        let err = merge_server_settings(&current, &body).expect_err("reject");
        assert!(err.contains("http-03"), "{err}");
    }
}
