#![forbid(unsafe_code)]
//! M5 admin 面板：静态资源、会话 Cookie、CSRF 与只读 API 的集成测试。

use std::io::Cursor;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use meatus_client::ClientState;
use meatus_core::{parse_client_config, parse_server_config};
use meatus_server::ServerState;
use rustls::RootCertStore;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

fn free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local addr").port()
}

async fn request(port: u16, raw: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect admin");
    stream
        .write_all(raw.as_bytes())
        .await
        .expect("write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read response");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (status, head.to_string(), body.to_string())
}

fn cookie_from(head: &str) -> String {
    head.lines()
        .find_map(|line| {
            line.strip_prefix("Set-Cookie: ")
                .and_then(|value| value.split(';').next())
        })
        .unwrap_or_default()
        .to_string()
}

async fn tls_request(port: u16, raw: &str, trust_der: &[u8]) -> (u16, String, String) {
    let mut roots = RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(trust_der.to_vec()))
        .expect("trust test certificate");
    let config = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect admin TLS");
    let mut stream = connector
        .connect(
            ServerName::try_from("localhost").expect("server name"),
            stream,
        )
        .await
        .expect("TLS handshake");
    stream
        .write_all(raw.as_bytes())
        .await
        .expect("write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read response");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (status, head.to_string(), body.to_string())
}

#[tokio::test]
async fn admin_tls_sets_secure_cookie_hsts_and_strict_csp() {
    let control_port = free_port();
    let admin_port = free_port();
    let dir = std::env::temp_dir().join(format!("meatus-admin-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_pem = certified.cert.pem();
    let key_pem = certified.signing_key.serialize_pem();
    std::fs::write(&cert_path, &cert_pem).expect("write cert");
    std::fs::write(&key_path, &key_pem).expect("write key");
    let cert_der = rustls_pemfile::certs(&mut Cursor::new(cert_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .expect("parse cert")
        .into_iter()
        .next()
        .expect("certificate");

    let config = parse_server_config(&format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{}"
tls_key = "{}"

[auth]
clients = [
  {{ id = "client", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" }},
]

[ports]
range = [24100, 24110]

[admin]
bind = "127.0.0.1:{admin_port}"
admin_token = "admin-secret"

[admin.tls]
enabled = true
cert = "{}"
key = "{}"
hsts = true
"#,
        cert_path.display(),
        key_path.display(),
        cert_path.display(),
        key_path.display()
    ))
    .expect("parse config");

    let state = ServerState::new(config).expect("state");
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let task = tokio::spawn(async move { meatus_server::run_until(state, run_cancel).await });
    sleep(Duration::from_millis(300)).await;

    let (status, head, body) = tls_request(
        admin_port,
        "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n",
        &cert_der,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("Meatus 管理面板"));
    assert!(head.contains("Strict-Transport-Security:"));
    assert!(head.contains("style-src 'self'"));
    assert!(!head.contains("unsafe-inline"));

    let (status, head, _) = tls_request(
        admin_port,
        "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"token\":\"admin-secret\"}",
        &cert_der,
    )
    .await;
    assert_eq!(status, 200);
    let set_cookie = head
        .lines()
        .find(|line| line.starts_with("Set-Cookie:"))
        .expect("Set-Cookie");
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));

    cancel.cancel();
    let _ = task.await;
}

#[tokio::test]
async fn admin_panel_login_csrf_and_static_assets() {
    let control_port = free_port();
    let admin_port = free_port();
    let dir = std::env::temp_dir().join(format!("meatus-admin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).expect("write key");

    let config = parse_server_config(&format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{}"
tls_key = "{}"

[auth]
clients = [
  {{ id = "client", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" }},
]

[ports]
range = [24000, 24010]

[admin]
bind = "127.0.0.1:{admin_port}"
admin_token = "admin-secret"
"#,
        cert_path.display(),
        key_path.display()
    ))
    .expect("parse config");

    let state = ServerState::new(config).expect("state");
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let task = tokio::spawn(async move { meatus_server::run_until(state, run_cancel).await });
    sleep(Duration::from_millis(300)).await;

    let (status, _, body) = request(admin_port, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert_eq!(status, 200);
    assert!(body.contains("Meatus 管理面板"));

    let (status, _, _) = request(
        admin_port,
        "GET /api/v1/status HTTP/1.1\r\nHost: localhost\r\n\r\n",
    )
    .await;
    assert_eq!(status, 401);

    let (status, head, body) = request(
        admin_port,
        "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"token\":\"admin-secret\"}",
    )
    .await;
    assert_eq!(status, 200);
    let cookie = cookie_from(&head);
    assert!(cookie.starts_with("meatus_session="));
    let csrf = body
        .split("\"csrf\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf token")
        .to_string();

    let auth_request =
        |path: &str| format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n");
    let (status, _, body) = request(admin_port, &auth_request("/api/v1/status")).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"ok\""));

    let (status, _, body) = request(admin_port, &auth_request("/api/v1/settings")).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"token_configured\":true"));

    let (status, _, body) = request(admin_port, &auth_request("/api/v1/logs?limit=20")).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"lines\""));

    let (status, _, _) = request(
        admin_port,
        &format!("DELETE /api/v1/session HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"),
    )
    .await;
    assert_eq!(status, 403);

    let (status, _, _) = request(
        admin_port,
        &format!(
            "DELETE /api/v1/session HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nX-Meatus-CSRF: {csrf}\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(status, 200);

    let (status, _, _) = request(
        admin_port,
        "GET /api/v1/status HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer admin-secret\r\n\r\n",
    )
    .await;
    assert_eq!(status, 200);

    cancel.cancel();
    let _ = task.await;
}

async fn spawn_echo(port: u16) -> tokio::task::JoinHandle<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind echo");
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut outbound = match TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let _ = copy(&mut inbound, &mut outbound).await;
            });
        }
    })
}

fn json_body(value: &str) -> String {
    format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        value.len(),
        value
    )
}

#[tokio::test]
async fn admin_proxy_crud_requires_csrf_and_updates_client() {
    let control_port = free_port();
    let admin_port = free_port();
    let echo_port = free_port();
    let proxy_port = free_port();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-admin-crud-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).expect("write key");

    let server_config = format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{cert}"
tls_key = "{key}"

[auth]
clients = [
  {{ id = "client", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:{admin_port}"
admin_token = "admin-secret"
"#,
        cert = cert_path.display(),
        key = key_path.display(),
    );
    let client_config = format!(
        r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "client"
tenant = "default"
token = "secret"
"#,
        cert = cert_path.display(),
    );

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let client_state = ClientState::new(parse_client_config(&client_config).expect("client cfg"))
        .expect("client state");
    let server_cancel = CancellationToken::new();
    let client_cancel = CancellationToken::new();
    let server_task = {
        let cancel = server_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state, cancel).await })
    };
    let client_task = {
        let cancel = client_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(client_state, cancel).await })
    };

    let mut run_id = String::new();
    for _ in 0..50 {
        sleep(Duration::from_millis(120)).await;
        let (status, head, _) = request(
            admin_port,
            "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"token\":\"admin-secret\"}",
        )
        .await;
        if status != 200 {
            continue;
        }
        let cookie = cookie_from(&head);
        let (status, _, status_body) = request(
            admin_port,
            &format!("GET /api/v1/status HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"),
        )
        .await;
        if status == 200 && status_body.contains("\"run_id\"") {
            run_id = status_body
                .split("\"run_id\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_default()
                .to_string();
            if !run_id.is_empty() {
                break;
            }
        }
    }
    assert!(!run_id.is_empty(), "client did not register");

    let login = "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"token\":\"admin-secret\"}";
    let (status, head, body) = request(admin_port, login).await;
    assert_eq!(status, 200);
    let cookie = cookie_from(&head);
    let csrf = body
        .split("\"csrf\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("csrf token");

    let payload = format!(
        r#"{{"run_id":"{run_id}","name":"echo-admin","type":"tcp","local_addr":"127.0.0.1:{echo_port}","remote_port":0,"custom_domains":[],"options":{{"max_conns":32}}}}"#
    );

    let (status, _, _) = request(
        admin_port,
        &format!(
            "POST /api/v1/proxies HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n{}\r\n",
            json_body(&payload)
        ),
    )
    .await;
    assert_eq!(status, 403, "write without CSRF must be rejected");

    let (status, _, body) = request(
        admin_port,
        &format!(
            "POST /api/v1/proxies HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nX-Meatus-CSRF: {csrf}\r\n{}\r\n",
            json_body(&payload)
        ),
    )
    .await;
    assert_eq!(status, 200, "add proxy: {body}");
    assert!(body.contains("\"remote_port\""));

    let (status, _, body) = request(
        admin_port,
        &format!("GET /api/v1/status HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("echo-admin"));

    let update = format!(
        r#"{{"run_id":"{run_id}","name":"echo-admin","type":"tcp","local_addr":"127.0.0.1:{echo_port}","remote_port":0,"custom_domains":[],"options":{{"max_conns":64}}}}"#
    );
    let (status, _, body) = request(
        admin_port,
        &format!(
            "PUT /api/v1/proxies HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nX-Meatus-CSRF: {csrf}\r\n{}\r\n",
            json_body(&update)
        ),
    )
    .await;
    assert_eq!(status, 200, "update proxy: {body}");

    let remove = format!(r#"{{"run_id":"{run_id}","name":"echo-admin"}}"#);
    let (status, _, _) = request(
        admin_port,
        &format!(
            "DELETE /api/v1/proxies HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n{}\r\n",
            json_body(&remove)
        ),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _, body) = request(
        admin_port,
        &format!(
            "DELETE /api/v1/proxies HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nX-Meatus-CSRF: {csrf}\r\n{}\r\n",
            json_body(&remove)
        ),
    )
    .await;
    assert_eq!(status, 200, "delete proxy: {body}");

    let (status, _, body) = request(
        admin_port,
        &format!("GET /api/v1/status HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!body.contains("echo-admin"));

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = client_task.await;
    let _ = server_task.await;
    let _ = proxy_port;
}
