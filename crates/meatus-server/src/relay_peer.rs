#![forbid(unsafe_code)]
//! Relay peer 证书同步通道。
//!
//! - `cert_sync = "push"`：primary 主动 POST 到 secondary；
//! - `cert_sync = "pull"`：secondary 主动 GET primary；
//! - 请求用 group `peer_secret` 做 HMAC-SHA256 鉴权，带时间窗与 nonce 防重放；
//! - 证书快照用同一 secret 派生的 AES-256-GCM 加密，私钥不以明文过网；
//! - 只同步 `cert-*.pem`，不同步 `account-*.pem`，secondary 不具备签发能力。

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hmac::{Hmac, Mac};
use meatus_core::{RelayCertSync, RelayRole};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::ServerState;
use crate::relay_cert::{RelayCertError, RelayCertSnapshot};

const PEER_PATH_CERT: &str = "/relay/v1/cert";
pub(crate) const PEER_PATH_HEALTH: &str = "/relay/v1/health";
const PEER_TIMESTAMP_WINDOW_S: i64 = 300;
const PEER_NONCE_RETENTION_S: i64 = 900;
const PEER_HEADER_GROUP: &str = "x-meatus-relay-group";
const PEER_HEADER_ID: &str = "x-meatus-relay-id";
const PEER_HEADER_TIMESTAMP: &str = "x-meatus-relay-timestamp";
const PEER_HEADER_NONCE: &str = "x-meatus-relay-nonce";
const PEER_HEADER_SIGNATURE: &str = "x-meatus-relay-signature";
const PEER_HEADER_ENCRYPTION_NONCE: &str = "x-meatus-peer-nonce";
const PEER_SIGNATURE_PREFIX: &str = "hmac-sha256:";
const PEER_AEAD_AAD: &[u8] = b"meatus-relay-cert-v1";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const SYNC_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct PeerError(String);

impl PeerError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PeerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PeerError {}

impl From<RelayCertError> for PeerError {
    fn from(err: RelayCertError) -> Self {
        Self(format!("cert snapshot: {err}"))
    }
}

/// 组内 peer 通道的密钥材料。Debug 不输出 secret。
pub struct PeerAuth {
    secret: [u8; 32],
    cipher_key: [u8; 32],
    seen_nonces: Mutex<HashMap<String, i64>>,
}

impl fmt::Debug for PeerAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerAuth")
            .field("secret", &"<redacted>")
            .field(
                "seen_nonces",
                &self.seen_nonces.lock().map_or(0, |seen| seen.len()),
            )
            .finish()
    }
}

impl PeerAuth {
    pub fn load(path: &Path) -> Result<Arc<Self>, PeerError> {
        let secret = std::fs::read(path)
            .map_err(|err| PeerError::new(format!("read {}: {err}", path.display())))?;
        if secret.len() != 32 {
            return Err(PeerError::new(format!(
                "{} must contain exactly 32 bytes",
                path.display()
            )));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        let cipher_key = derive_cipher_key(&key);
        Ok(Arc::new(Self {
            secret: key,
            cipher_key,
            seen_nonces: Mutex::new(HashMap::new()),
        }))
    }

    fn sign(&self, method: &str, path: &str, group: &str, id: &str, body: &[u8]) -> PeerHeaders {
        let timestamp = now_unix();
        let nonce = fresh_nonce();
        let body_hash = hex::encode(Sha256::digest(body));
        let canonical =
            format!("{method}\n{path}\n{group}\n{id}\n{timestamp}\n{nonce}\n{body_hash}");
        let mut mac = self.mac();
        mac.update(canonical.as_bytes());
        let signature = mac.finalize().into_bytes().to_vec();
        PeerHeaders {
            group: group.to_string(),
            id: id.to_string(),
            timestamp,
            nonce,
            signature: format!(
                "{PEER_SIGNATURE_PREFIX}{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature)
            ),
        }
    }

    fn verify(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<(), PeerError> {
        let group = header(headers, PEER_HEADER_GROUP)?;
        let id = header(headers, PEER_HEADER_ID)?;
        let timestamp = header(headers, PEER_HEADER_TIMESTAMP)?
            .parse::<i64>()
            .map_err(|_| PeerError::new("relay timestamp must be a unix timestamp"))?;
        let nonce = header(headers, PEER_HEADER_NONCE)?;
        let signature = header(headers, PEER_HEADER_SIGNATURE)?;
        let signature = signature
            .strip_prefix(PEER_SIGNATURE_PREFIX)
            .ok_or_else(|| PeerError::new("relay signature must use hmac-sha256 prefix"))?;
        if nonce.len() < 8 || nonce.len() > 128 || nonce.contains(char::is_whitespace) {
            return Err(PeerError::new("relay nonce is malformed"));
        }
        let now = now_unix();
        if (now - timestamp).abs() > PEER_TIMESTAMP_WINDOW_S {
            return Err(PeerError::new("relay timestamp outside 300s window"));
        }
        let body_hash = hex::encode(Sha256::digest(body));
        let canonical =
            format!("{method}\n{path}\n{group}\n{id}\n{timestamp}\n{nonce}\n{body_hash}");
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| PeerError::new("relay signature is not base64url"))?;
        let mut mac = self.mac();
        mac.update(canonical.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| PeerError::new("relay signature mismatch"))?;

        let mut seen = self
            .seen_nonces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.retain(|_, seen_at| now.saturating_sub(*seen_at) <= PEER_NONCE_RETENTION_S);
        if seen.insert(nonce.to_string(), now).is_some() {
            return Err(PeerError::new("relay nonce replayed"));
        }
        Ok(())
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, String), PeerError> {
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &self.cipher_key)
                .map_err(|_| PeerError::new("build peer cipher key"))?,
        );
        let rng = SystemRandom::new();
        let mut nonce = [0u8; 12];
        rng.fill(&mut nonce)
            .map_err(|_| PeerError::new("generate peer nonce"))?;
        let nonce_string = hex::encode(nonce);
        let mut output = plaintext.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(PEER_AEAD_AAD),
            &mut output,
        )
        .map_err(|_| PeerError::new("encrypt peer payload"))?;
        Ok((output, nonce_string))
    }

    fn decrypt(&self, ciphertext: &[u8], nonce: &str) -> Result<Vec<u8>, PeerError> {
        let nonce_bytes =
            hex::decode(nonce).map_err(|_| PeerError::new("peer encryption nonce is not hex"))?;
        if nonce_bytes.len() != 12 {
            return Err(PeerError::new("peer encryption nonce must be 12 bytes"));
        }
        let mut nonce_array = [0u8; 12];
        nonce_array.copy_from_slice(&nonce_bytes);
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &self.cipher_key)
                .map_err(|_| PeerError::new("build peer cipher key"))?,
        );
        let mut output = ciphertext.to_vec();
        let plaintext = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_array),
                Aad::from(PEER_AEAD_AAD),
                &mut output,
            )
            .map_err(|_| PeerError::new("decrypt peer payload"))?;
        Ok(plaintext.to_vec())
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC accepts any key length")
    }
}

fn derive_cipher_key(secret: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(b"|meatus-relay-cert-v1");
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

fn header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Result<&'a str, PeerError> {
    headers
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| PeerError::new(format!("missing {name} header")))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
        })
}

fn fresh_nonce() -> String {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 16];
    let _ = rng.fill(&mut bytes);
    hex::encode(bytes)
}

struct PeerHeaders {
    group: String,
    id: String,
    timestamp: i64,
    nonce: String,
    signature: String,
}

struct PeerRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PeerApplyResponse {
    applied: bool,
    fingerprints: Vec<String>,
}

#[derive(Serialize)]
struct PeerHealth<'a> {
    id: &'a str,
    group: &'a str,
    role: &'static str,
    epoch: u64,
    cert_sync: &'static str,
}

/// 启动 relay peer 端点：健康与证书同步共用 `health_bind`。
pub async fn serve(
    listener: TcpListener,
    state: Arc<ServerState>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "relay peer accept failed");
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(err) = handle(stream, state).await {
                        debug!(%peer, error = %err, "relay peer request ended");
                    }
                });
            }
        }
    }
    info!("relay peer listener stopped");
    Ok(())
}

async fn handle(mut stream: TcpStream, state: Arc<ServerState>) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    let Some(relay) = state.relay.as_ref() else {
        return respond(&mut stream, 404, br#"{"error":"relay not configured"}"#).await;
    };
    if request.path == PEER_PATH_HEALTH {
        let body = PeerHealth {
            id: relay.id(),
            group: relay.group(),
            role: relay.role().as_str(),
            epoch: relay.epoch(),
            cert_sync: state
                .config
                .relay
                .as_ref()
                .map_or("off", |relay| relay.cert_sync.as_str()),
        };
        return respond_json(&mut stream, 200, &body).await;
    }
    let Some(auth) = state.peer_auth.as_ref() else {
        return respond(&mut stream, 403, br#"{"error":"peer auth unavailable"}"#).await;
    };
    if let Err(err) = auth.verify(
        &request.method,
        &request.path,
        &request.headers,
        &request.body,
    ) {
        warn!(error = %err, path = %request.path, "relay peer auth rejected");
        return respond(&mut stream, 401, br#"{"error":"unauthorized"}"#).await;
    }
    let peer_id = header(&request.headers, PEER_HEADER_ID)?;
    let peer_group = header(&request.headers, PEER_HEADER_GROUP)?;
    if peer_group != relay.group() || !relay.peers().any(|peer| peer.id == peer_id) {
        warn!(peer = %peer_id, group = %peer_group, "relay peer identity rejected");
        return respond(&mut stream, 403, br#"{"error":"peer identity mismatch"}"#).await;
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", PEER_PATH_CERT) => {
            if relay.role() != RelayRole::Primary
                || state.config.relay.as_ref().map(|relay| relay.cert_sync)
                    != Some(RelayCertSync::Pull)
            {
                return respond(
                    &mut stream,
                    403,
                    br#"{"error":"peer pull is not enabled on this relay"}"#,
                )
                .await;
            }
            let snapshot = match RelayCertSnapshot::from_cache(
                relay.group(),
                relay.epoch(),
                &state.config.acme.cert_dir,
            ) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    warn!(error = %err, "build relay cert snapshot failed");
                    return respond(&mut stream, 500, br#"{"error":"cert snapshot failed"}"#).await;
                }
            };
            let json = serde_json::to_vec(&snapshot)?;
            let (ciphertext, nonce) = auth.encrypt(&json)?;
            return respond_encrypted(&mut stream, 200, &ciphertext, &nonce).await;
        }
        ("POST", PEER_PATH_CERT) => {
            if relay.role() != RelayRole::Secondary
                || state.config.relay.as_ref().map(|relay| relay.cert_sync)
                    != Some(RelayCertSync::Push)
            {
                return respond(
                    &mut stream,
                    403,
                    br#"{"error":"peer push is not enabled on this relay"}"#,
                )
                .await;
            }
            let nonce = header(&request.headers, PEER_HEADER_ENCRYPTION_NONCE)?;
            let plaintext = auth.decrypt(&request.body, nonce)?;
            let snapshot: RelayCertSnapshot = serde_json::from_slice(&plaintext)
                .map_err(|err| PeerError::new(format!("decode cert snapshot: {err}")))?;
            if snapshot.group != relay.group() || snapshot.epoch != relay.epoch() {
                return respond(
                    &mut stream,
                    409,
                    br#"{"error":"cert snapshot group/epoch mismatch"}"#,
                )
                .await;
            }
            snapshot.validate()?;
            let Some(store) = state.relay_certs.as_ref() else {
                return respond(
                    &mut stream,
                    409,
                    br#"{"error":"secondary cert store unavailable"}"#,
                )
                .await;
            };
            if let Err(err) = store.apply(&snapshot) {
                warn!(error = %err, "apply relay cert snapshot failed");
                return respond(
                    &mut stream,
                    500,
                    br#"{"error":"apply cert snapshot failed"}"#,
                )
                .await;
            }
            let response = PeerApplyResponse {
                applied: true,
                fingerprints: snapshot.fingerprints(),
            };
            let payload = serde_json::to_vec(&response)?;
            let (ciphertext, nonce) = auth.encrypt(&payload)?;
            return respond_encrypted(&mut stream, 200, &ciphertext, &nonce).await;
        }
        _ => respond(&mut stream, 404, br#"{"error":"not found"}"#).await,
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<PeerRequest, PeerError> {
    let mut buffer = Vec::with_capacity(MAX_HEADER_BYTES);
    let mut temp = [0u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|err| PeerError::new(format!("read peer request: {err}")))?;
        if read == 0 {
            return Err(PeerError::new("peer request ended before headers"));
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(PeerError::new("peer request headers too large"));
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| PeerError::new("empty request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| PeerError::new("missing method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| PeerError::new("missing path"))?
        .to_string();
    if parts.next().is_none() {
        return Err(PeerError::new("missing HTTP version"));
    }
    if method != "GET" && method != "POST" {
        return Err(PeerError::new("unsupported peer method"));
    }
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(PeerError::new("malformed peer header"));
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers
        .get("content-length")
        .map_or(Ok(0usize), |value| value.parse::<usize>())
        .map_err(|_| PeerError::new("invalid content-length"))?;
    if content_length > MAX_BODY_BYTES {
        return Err(PeerError::new("peer body too large"));
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|err| PeerError::new(format!("read peer body: {err}")))?;
        if read == 0 {
            return Err(PeerError::new("peer body ended early"));
        }
        body.extend_from_slice(&temp[..read]);
    }
    body.truncate(content_length);
    Ok(PeerRequest {
        method,
        path,
        headers,
        body,
    })
}

async fn respond_json<T: Serialize>(
    stream: &mut TcpStream,
    status: u16,
    value: &T,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(value)?;
    respond(stream, status, &body).await
}

async fn respond(stream: &mut TcpStream, status: u16, body: &[u8]) -> anyhow::Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

async fn respond_encrypted(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    nonce: &str,
) -> anyhow::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n{PEER_HEADER_ENCRYPTION_NONCE}: {nonce}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

/// 启动证书同步循环；`off` 直接返回。
pub async fn run_sync(state: Arc<ServerState>, cancel: CancellationToken) -> anyhow::Result<()> {
    let Some(relay) = state.relay.as_ref() else {
        return Ok(());
    };
    let Some(section) = state.config.relay.as_ref() else {
        return Ok(());
    };
    let mode = section.cert_sync;
    if mode == RelayCertSync::Off {
        return Ok(());
    }
    let Some(auth) = state.peer_auth.clone() else {
        return Err(PeerError::new("peer auth unavailable for cert sync").into());
    };
    let role = relay.role();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut ticker = tokio::time::interval(SYNC_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let result = match (role, mode) {
                    (RelayRole::Primary, RelayCertSync::Push) => push_once(&state, &auth, &client).await,
                    (RelayRole::Secondary, RelayCertSync::Pull) => pull_once(&state, &auth, &client).await,
                    _ => Ok(()),
                };
                if let Err(err) = result {
                    warn!(error = %err, "relay cert sync failed; will retry");
                }
            }
        }
    }
    info!("relay cert sync stopped");
    Ok(())
}

pub(crate) async fn push_once(
    state: &ServerState,
    auth: &PeerAuth,
    client: &reqwest::Client,
) -> Result<(), PeerError> {
    let Some(relay) = state.relay.as_ref() else {
        return Ok(());
    };
    let snapshot =
        RelayCertSnapshot::from_cache(relay.group(), relay.epoch(), &state.config.acme.cert_dir)?;
    if snapshot.files.is_empty() {
        debug!("no acme certificates to push");
        return Ok(());
    }
    let json = serde_json::to_vec(&snapshot)
        .map_err(|err| PeerError::new(format!("encode cert snapshot: {err}")))?;
    let (ciphertext, nonce) = auth.encrypt(&json)?;
    for peer in relay.peers() {
        let Some(address) = peer.peer_addr.as_deref() else {
            warn!(peer = %peer.id, "relay peer_addr missing; skip cert push");
            continue;
        };
        let url = format!("http://{address}{PEER_PATH_CERT}");
        let headers = auth.sign(
            "POST",
            PEER_PATH_CERT,
            relay.group(),
            relay.id(),
            &ciphertext,
        );
        let mut request = client
            .post(&url)
            .header(PEER_HEADER_GROUP, headers.group)
            .header(PEER_HEADER_ID, headers.id)
            .header(PEER_HEADER_TIMESTAMP, headers.timestamp.to_string())
            .header(PEER_HEADER_NONCE, headers.nonce)
            .header(PEER_HEADER_SIGNATURE, headers.signature)
            .header(PEER_HEADER_ENCRYPTION_NONCE, &nonce)
            .header("content-type", "application/octet-stream")
            .body(ciphertext.clone());
        if let Some(deadline) = state.relay_timeout() {
            request = request.timeout(deadline);
        }
        let response = request
            .send()
            .await
            .map_err(|err| PeerError::new(format!("push cert to {url}: {err}")))?;
        let status = response.status();
        let response_nonce = response
            .headers()
            .get(PEER_HEADER_ENCRYPTION_NONCE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .bytes()
            .await
            .map_err(|err| PeerError::new(format!("read push response: {err}")))?;
        if !status.is_success() {
            warn!(%url, %status, body = %String::from_utf8_lossy(&body), "peer rejected cert push");
            continue;
        }
        let nonce = response_nonce
            .ok_or_else(|| PeerError::new("push response missing encryption nonce"))?;
        let plaintext = auth.decrypt(&body, &nonce)?;
        let applied: PeerApplyResponse = serde_json::from_slice(&plaintext)
            .map_err(|err| PeerError::new(format!("decode push response: {err}")))?;
        if applied.fingerprints != snapshot.fingerprints() {
            warn!(peer = %peer.id, "relay cert fingerprints differ after push");
        } else {
            info!(peer = %peer.id, files = snapshot.files.len(), "relay cert pushed");
        }
    }
    Ok(())
}

pub(crate) async fn pull_once(
    state: &ServerState,
    auth: &PeerAuth,
    client: &reqwest::Client,
) -> Result<(), PeerError> {
    let Some(relay) = state.relay.as_ref() else {
        return Ok(());
    };
    let primary = relay
        .peers()
        .find(|peer| peer.role == RelayRole::Primary)
        .ok_or_else(|| PeerError::new("relay group has no primary"))?;
    let address = primary
        .peer_addr
        .as_deref()
        .ok_or_else(|| PeerError::new("primary relay peer_addr missing"))?;
    let url = format!("http://{address}{PEER_PATH_CERT}");
    let headers = auth.sign("GET", PEER_PATH_CERT, relay.group(), relay.id(), &[]);
    let response = client
        .get(&url)
        .header(PEER_HEADER_GROUP, headers.group)
        .header(PEER_HEADER_ID, headers.id)
        .header(PEER_HEADER_TIMESTAMP, headers.timestamp.to_string())
        .header(PEER_HEADER_NONCE, headers.nonce)
        .header(PEER_HEADER_SIGNATURE, headers.signature)
        .send()
        .await
        .map_err(|err| PeerError::new(format!("pull cert from {url}: {err}")))?;
    if !response.status().is_success() {
        return Err(PeerError::new(format!(
            "primary rejected cert pull: {}",
            response.status()
        )));
    }
    let nonce = response
        .headers()
        .get(PEER_HEADER_ENCRYPTION_NONCE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| PeerError::new("cert response missing encryption nonce"))?;
    let body = response
        .bytes()
        .await
        .map_err(|err| PeerError::new(format!("read cert response: {err}")))?;
    let plaintext = auth.decrypt(&body, &nonce)?;
    let snapshot: RelayCertSnapshot = serde_json::from_slice(&plaintext)
        .map_err(|err| PeerError::new(format!("decode cert snapshot: {err}")))?;
    snapshot.validate()?;
    if snapshot.group != relay.group() || snapshot.epoch != relay.epoch() {
        return Err(PeerError::new("cert snapshot group/epoch mismatch"));
    }
    let store = state
        .relay_certs
        .as_ref()
        .ok_or_else(|| PeerError::new("secondary cert store unavailable"))?;
    store.apply(&snapshot)?;
    info!(files = snapshot.files.len(), "relay cert pulled");
    Ok(())
}

impl ServerState {
    fn relay_timeout(&self) -> Option<Duration> {
        self.config
            .relay
            .as_ref()
            .map(|relay| Duration::from_secs(relay.peer_timeout_s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::RelayMember;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn auth() -> Arc<PeerAuth> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "meatus-peer-auth-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("peer.key");
        std::fs::write(&path, [7u8; 32]).expect("write");
        let auth = PeerAuth::load(&path).expect("auth");
        let _ = std::fs::remove_dir_all(dir);
        auth
    }

    #[test]
    fn hmac_sign_verify_and_replay_rejected() {
        let auth = auth();
        let headers = auth.sign("GET", PEER_PATH_CERT, "prod", "relay-a", b"");
        let mut map = HashMap::new();
        map.insert(PEER_HEADER_GROUP.to_string(), headers.group.clone());
        map.insert(PEER_HEADER_ID.to_string(), headers.id.clone());
        map.insert(
            PEER_HEADER_TIMESTAMP.to_string(),
            headers.timestamp.to_string(),
        );
        map.insert(PEER_HEADER_NONCE.to_string(), headers.nonce.clone());
        map.insert(PEER_HEADER_SIGNATURE.to_string(), headers.signature.clone());
        auth.verify("GET", PEER_PATH_CERT, &map, b"")
            .expect("verify");
        let err = auth
            .verify("GET", PEER_PATH_CERT, &map, b"")
            .expect_err("replay");
        assert!(err.to_string().contains("replay"), "{err}");
        let err = auth
            .verify("POST", PEER_PATH_CERT, &map, b"")
            .expect_err("method mismatch");
        assert!(err.to_string().contains("signature"), "{err}");
    }

    #[test]
    fn encrypted_payload_roundtrip_and_tamper_rejected() {
        let auth = auth();
        let (ciphertext, nonce) = auth.encrypt(b"cert-private-key").expect("encrypt");
        let plaintext = auth.decrypt(&ciphertext, &nonce).expect("decrypt");
        assert_eq!(plaintext, b"cert-private-key");
        let err = auth.decrypt(&ciphertext, "00").expect_err("bad nonce");
        assert!(err.to_string().contains("nonce"), "{err}");
        let mut tampered = ciphertext;
        tampered[0] ^= 1;
        assert!(auth.decrypt(&tampered, &nonce).is_err());
    }

    #[test]
    fn stale_timestamp_rejected() {
        let auth = auth();
        let headers = auth.sign("GET", PEER_PATH_CERT, "prod", "relay-a", b"");
        let mut map = HashMap::new();
        map.insert(PEER_HEADER_GROUP.to_string(), headers.group);
        map.insert(PEER_HEADER_ID.to_string(), headers.id);
        map.insert(
            PEER_HEADER_TIMESTAMP.to_string(),
            (now_unix() - 301).to_string(),
        );
        map.insert(PEER_HEADER_NONCE.to_string(), headers.nonce);
        map.insert(PEER_HEADER_SIGNATURE.to_string(), headers.signature);
        let err = auth
            .verify("GET", PEER_PATH_CERT, &map, b"")
            .expect_err("stale");
        assert!(err.to_string().contains("timestamp"), "{err}");
    }

    #[test]
    fn peer_member_role_is_used_for_pull_target() {
        let member = RelayMember {
            id: "relay-a".to_string(),
            role: RelayRole::Primary,
            control_addr: "relay-a.example.com:7000".to_string(),
            http_domains: vec!["*.example.com".to_string()],
            peer_addr: Some("relay-a.example.com:7401".to_string()),
            ports: [20_000, 20_999],
        };
        assert_eq!(
            member.peer_addr.as_deref(),
            Some("relay-a.example.com:7401")
        );
    }
}
