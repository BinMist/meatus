#![forbid(unsafe_code)]
//! 端到端隧道测试：真实 TLS + yamux + HMAC + 公网端口映射。

use std::net::TcpListener as StdTcpListener;
use std::time::Duration;

use meatus_client::ClientState;
use meatus_core::{parse_client_config, parse_server_config};
use meatus_server::ServerState;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

static TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOKEN_HASH: &str = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"; // sha256("secret")

fn free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local addr").port()
}

fn distinct_ports() -> (u16, u16, u16) {
    loop {
        let echo = free_port();
        let control = free_port();
        let proxy = free_port();
        if echo != control && echo != proxy && control != proxy {
            return (echo, control, proxy);
        }
    }
}

async fn spawn_echo(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind echo");
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("echo accept");
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.split();
                let _ = copy(&mut reader, &mut writer).await;
            });
        }
    });
}

#[tokio::test]
async fn quic_dial_reaches_server() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    let _ = echo_port;
    let dir = std::env::temp_dir().join(format!("meatus-quic-{}", std::process::id()));
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
quic = true

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "quic"
"#,
        cert = cert_path.display(),
    );

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let server_cancel = CancellationToken::new();
    let server_task = {
        let cancel = server_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state, cancel).await })
    };
    sleep(Duration::from_millis(500)).await;

    let client = meatus_client::quic::connect(
        &parse_client_config(&client_config).expect("client cfg"),
        &format!("127.0.0.1:{control_port}"),
    )
    .await
    .expect("quic connect");
    let (mut send, recv) = client
        .connection()
        .open_bi()
        .await
        .expect("open quic stream");
    send.write_all(b"hello").await.expect("write");
    send.finish().expect("finish");
    drop(recv);
    client.connection().close(0u32.into(), b"test done");

    let _ = timeout(Duration::from_secs(5), server_task).await;
}

#[tokio::test]
async fn tcp_tunnel_roundtrip() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-it-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut last_error = None;
    let mut tunneled = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        match probe(proxy_port).await {
            Ok(()) => {
                tunneled = true;
                break;
            }
            Err(err) => last_error = Some(err),
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(
        tunneled,
        "tunnel did not come up: {}",
        last_error.unwrap_or_else(|| "no attempt".to_string())
    );
}

#[tokio::test]
async fn quic_tunnel_roundtrip() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-quic-it-{}", std::process::id()));
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
quic = true

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "quic"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut last_error = None;
    let mut tunneled = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        match probe(proxy_port).await {
            Ok(()) => {
                tunneled = true;
                break;
            }
            Err(err) => last_error = Some(err),
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(
        tunneled,
        "tunnel did not come up: {}",
        last_error.unwrap_or_else(|| "no attempt".to_string())
    );
}

#[tokio::test]
async fn quic_udp_tunnel_roundtrip() {
    let _guard = TEST_MUTEX.lock().await;
    let echo_port = free_udp_port();
    let control_port = free_port();
    let proxy_port = free_udp_port();
    spawn_udp_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-quic-udp-{}", std::process::id()));
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
quic = true

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "quic"

[[proxy]]
name = "dns"
type = "udp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
options.idle_timeout_s = 60
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

    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind public udp");
    let mut ok = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        socket
            .send_to(b"quic-udp-query", ("127.0.0.1", proxy_port))
            .await
            .expect("send to proxy");
        let mut buffer = [0u8; 1500];
        match timeout(Duration::from_millis(500), socket.recv(&mut buffer)).await {
            Ok(Ok(len)) if &buffer[..len] == b"quic-udp-query" => {
                ok = true;
                break;
            }
            _ => {}
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    assert!(ok, "QUIC UDP roundtrip failed");
}

#[tokio::test]
async fn quic_client_reconnects_after_server_restart() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-quic-restart-{}", std::process::id()));
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
quic = true

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "quic"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
"#,
        cert = cert_path.display(),
    );

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let server_state2 = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let client_state = ClientState::new(parse_client_config(&client_config).expect("client cfg"))
        .expect("client state");

    let server_cancel = CancellationToken::new();
    let client_cancel = CancellationToken::new();
    let mut server_task = {
        let cancel = server_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state, cancel).await })
    };
    let client_task = {
        let cancel = client_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(client_state, cancel).await })
    };

    let mut first = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            first = true;
            break;
        }
    }
    assert!(first, "initial QUIC tunnel did not come up");

    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), &mut server_task).await;
    sleep(Duration::from_millis(300)).await;

    let server2_cancel = CancellationToken::new();
    let mut server2_task = {
        let cancel = server2_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state2, cancel).await })
    };

    let mut restarted = false;
    for _ in 0..100 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            restarted = true;
            break;
        }
    }

    client_cancel.cancel();
    server2_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), &mut server2_task).await;
    assert!(restarted, "client did not reconnect after server restart");
}

#[tokio::test]
async fn auto_falls_back_to_tcp_when_quic_is_off() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-auto-it-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "auto"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut last_error = None;
    let mut tunneled = false;
    for _ in 0..80 {
        sleep(Duration::from_millis(200)).await;
        match probe(proxy_port).await {
            Ok(()) => {
                tunneled = true;
                break;
            }
            Err(err) => last_error = Some(err),
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(
        tunneled,
        "tunnel did not come up: {}",
        last_error.unwrap_or_else(|| "no attempt".to_string())
    );
}

#[tokio::test]
async fn client_fails_over_to_second_server_addr() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    let dead_port = free_port();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-failover-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
"#,
        cert = cert_path.display(),
        key = key_path.display(),
    );

    let client_config = format!(
        r#"
[client]
server_addrs = ["127.0.0.1:{dead_port}", "127.0.0.1:{control_port}"]
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut tunneled = false;
    for _ in 0..60 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            tunneled = true;
            break;
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    assert!(tunneled, "client did not fail over to the second entry");
}

#[tokio::test]
async fn duplicate_proxy_name_is_rejected_and_client_offline_is_removed() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_a, echo_b, control_port, proxy_port, admin_port) = loop {
        let echo_a = free_port();
        let echo_b = free_port();
        let control = free_port();
        let proxy = free_port();
        let admin = free_port();
        if [echo_a, echo_b, control, proxy, admin]
            .iter()
            .enumerate()
            .all(|(index, port)| {
                [echo_a, echo_b, control, proxy, admin]
                    .iter()
                    .skip(index + 1)
                    .all(|other| port != other)
            })
        {
            break (echo_a, echo_b, control, proxy, admin);
        }
    };
    spawn_echo(echo_a).await;
    spawn_echo(echo_b).await;

    let dir = std::env::temp_dir().join(format!("meatus-it-c2-{}", std::process::id()));
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
  {{ id = "nas-a", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
  {{ id = "nas-b", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        cert = cert_path.display(),
        key = key_path.display(),
    );

    let client_config = |id: &str, local_port: u16| {
        format!(
            r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "{id}"
tenant = "default"
token = "secret"

[[proxy]]
name = "web"
type = "tcp"
local_addr = "127.0.0.1:{local_port}"
remote_port = {proxy_port}
"#,
            cert = cert_path.display(),
        )
    };

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let a_state =
        ClientState::new(parse_client_config(&client_config("nas-a", echo_a)).expect("a"))
            .expect("a state");
    let b_state =
        ClientState::new(parse_client_config(&client_config("nas-b", echo_b)).expect("b"))
            .expect("b state");

    let server_cancel = CancellationToken::new();
    let a_cancel = CancellationToken::new();
    let b_cancel = CancellationToken::new();
    let server_task = {
        let cancel = server_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state, cancel).await })
    };
    let a_task = {
        let cancel = a_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(a_state, cancel).await })
    };
    let b_task = {
        let cancel = b_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(b_state, cancel).await })
    };

    let mut snapshot = serde_json::Value::Null;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        snapshot = status_json(admin_port)
            .await
            .unwrap_or(serde_json::Value::Null);
        if snapshot["totals"]["clients"] == 2 && snapshot["totals"]["proxies"] == 1 {
            break;
        }
    }
    assert_eq!(snapshot["totals"]["clients"], 2, "both clients connect");
    assert_eq!(
        snapshot["totals"]["proxies"], 1,
        "same proxy name must be exclusive"
    );
    assert!(snapshot["proxies"][0]["client_id"].as_str().is_some());

    a_cancel.cancel();
    let mut offline = false;
    for _ in 0..150 {
        sleep(Duration::from_millis(200)).await;
        if let Ok(snapshot) = status_json(admin_port).await
            && snapshot["totals"]["clients"] == 1
        {
            offline = true;
            break;
        }
    }
    assert!(offline, "client must be removed after disconnect");

    b_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), a_task).await;
    let _ = timeout(Duration::from_secs(5), b_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
}

#[tokio::test]
async fn offline_http_route_returns_503() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, http_port, https_port, admin_port) = loop {
        let echo = free_port();
        let control = free_port();
        let http = free_port();
        let https = free_port();
        let admin = free_port();
        let ports = [echo, control, http, https, admin];
        if ports
            .iter()
            .enumerate()
            .all(|(index, port)| ports.iter().skip(index + 1).all(|other| port != other))
        {
            break (echo, control, http, https, admin);
        }
    };
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-it-http-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [20000, 30000]
release_policy = "immediate"
hold_secs = 30

[http]
enabled = true
bind = "127.0.0.1:{http_port}"
https_bind = "127.0.0.1:{https_port}"
domain = "app.test"

[admin]
bind = "127.0.0.1:{admin_port}"
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
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "web"
type = "http"
local_addr = "127.0.0.1:{echo_port}"
remote_port = 0
custom_domains = ["app.test"]
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

    let mut online = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        if let Ok(snapshot) = status_json(admin_port).await
            && snapshot["totals"]["proxies"] == 1
        {
            online = true;
            break;
        }
    }
    assert!(online, "http proxy must register");

    client_cancel.cancel();
    let mut offline = false;
    for _ in 0..150 {
        sleep(Duration::from_millis(200)).await;
        if let Ok(snapshot) = status_json(admin_port).await
            && snapshot["totals"]["clients"] == 0
        {
            offline = true;
            break;
        }
    }
    assert!(offline, "client must disconnect");

    let (status, _, _) = http_request(http_port, "app.test").await;
    assert_eq!(status, 503, "offline http route must return 503");

    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
}

#[tokio::test]
async fn client_policy_rejects_disallowed_proxy_type() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, http_port, https_port, admin_port) = loop {
        let ports = [
            free_port(),
            free_port(),
            free_port(),
            free_port(),
            free_port(),
        ];
        if ports
            .iter()
            .enumerate()
            .all(|(index, port)| ports.iter().skip(index + 1).all(|other| port != other))
        {
            break (ports[0], ports[1], ports[2], ports[3], ports[4]);
        }
    };
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-it-policy-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}", allow_types = ["tcp"] }},
]

[ports]
range = [20000, 30000]

[http]
enabled = true
bind = "127.0.0.1:{http_port}"
https_bind = "127.0.0.1:{https_port}"
domain = "app.test"

[admin]
bind = "127.0.0.1:{admin_port}"
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
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "web"
type = "http"
local_addr = "127.0.0.1:{echo_port}"
remote_port = 0
custom_domains = ["app.test"]
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

    let mut rejected = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        if let Ok(snapshot) = status_json(admin_port).await
            && snapshot["totals"]["clients"].as_u64().unwrap_or(0) == 1
            && snapshot["totals"]["proxies"].as_u64().unwrap_or(0) == 0
        {
            rejected = true;
            break;
        }
    }
    assert!(
        rejected,
        "http proxy must be rejected for a tcp-only client"
    );

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
}

async fn http_request(port: u16, host: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect http");
    stream
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .expect("write http");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read http");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (status, head.to_string(), body.to_string())
}

async fn status_json(port: u16) -> Result<serde_json::Value, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|err| format!("connect admin: {err}"))?;
    stream
        .write_all(
            format!(
                "GET /api/v1/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .map_err(|err| format!("write: {err}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .map_err(|err| format!("read: {err}"))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or_else(|| "missing response body".to_string())?;
    serde_json::from_str(body).map_err(|err| format!("json: {err}"))
}

async fn probe(proxy_port: u16) -> Result<(), String> {
    let mut stream = timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|err| format!("connect: {err}"))?;

    stream
        .write_all(b"ping-through-tunnel")
        .await
        .map_err(|err| format!("write: {err}"))?;

    let mut buf = vec![0u8; 64];
    let n = timeout(Duration::from_secs(2), stream.read(&mut buf))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|err| format!("read: {err}"))?;
    if &buf[..n] != b"ping-through-tunnel" {
        return Err(format!("unexpected echo: {:?}", &buf[..n]));
    }
    Ok(())
}

fn free_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind udp");
    socket.local_addr().expect("udp addr").port()
}

async fn spawn_udp_echo(port: u16) {
    let socket = tokio::net::UdpSocket::bind(("127.0.0.1", port))
        .await
        .expect("bind udp echo");
    tokio::spawn(async move {
        let mut buffer = [0u8; 1500];
        loop {
            let (len, peer) = socket.recv_from(&mut buffer).await.expect("udp recv");
            socket
                .send_to(&buffer[..len], peer)
                .await
                .expect("udp send");
        }
    });
}

#[tokio::test]
async fn udp_tunnel_roundtrip() {
    let _guard = TEST_MUTEX.lock().await;
    let echo_port = free_udp_port();
    let control_port = free_port();
    let proxy_port = free_udp_port();
    let admin_port = free_port();
    spawn_udp_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-udp-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:{admin_port}"
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
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "dns"
type = "udp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
options.idle_timeout_s = 60
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

    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind public udp");
    let mut ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        socket
            .send_to(b"udp-dns-query", ("127.0.0.1", proxy_port))
            .await
            .expect("send to proxy");
        let mut buffer = [0u8; 1500];
        match timeout(Duration::from_millis(500), socket.recv(&mut buffer)).await {
            Ok(Ok(len)) if &buffer[..len] == b"udp-dns-query" => {
                ok = true;
                break;
            }
            _ => {}
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    assert!(ok, "udp roundtrip failed");
}

fn range_ports(count: u16) -> u16 {
    loop {
        let base = free_port();
        if base > 60000 || base + count >= 65000 {
            continue;
        }
        let mut all = Vec::new();
        for offset in 0..count {
            match StdTcpListener::bind(("127.0.0.1", base + offset)) {
                Ok(listener) => all.push(listener),
                Err(_) => break,
            }
        }
        if all.len() == usize::from(count) {
            drop(all);
            return base;
        }
    }
}

async fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|err| format!("connect: {err}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| format!("write: {err}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|err| format!("read: {err}"))?;
    let text = String::from_utf8_lossy(&response).to_string();
    Ok(text)
}

#[tokio::test]
async fn ten_proxies_and_metrics_endpoint() {
    let _guard = TEST_MUTEX.lock().await;
    let control_port = free_port();
    let admin_port = free_port();
    let base = range_ports(10);
    let mut echo_ports = Vec::new();
    for _ in 0..10 {
        let port = free_port();
        echo_ports.push(port);
        spawn_echo(port).await;
    }

    let dir = std::env::temp_dir().join(format!("meatus-metrics-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).expect("write key");

    let mut proxies = String::new();
    for (index, echo_port) in echo_ports.iter().enumerate() {
        proxies.push_str(&format!(
            "\n[[proxy]]\nname = \"p{index}\"\ntype = \"tcp\"\nlocal_addr = \"127.0.0.1:{echo_port}\"\nremote_port = {}\n",
            base + index as u16
        ));
    }

    let server_config = format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{cert}"
tls_key = "{key}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{base}, {}]

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        base + 9,
        cert = cert_path.display(),
        key = key_path.display(),
    );

    let client_config = format!(
        r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "nas"
tenant = "default"
token = "secret"
{proxies}"#,
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

    let mut all_ok = true;
    for offset in 0..10 {
        let mut ok = false;
        for _ in 0..30 {
            sleep(Duration::from_millis(200)).await;
            if probe(base + offset).await.is_ok() {
                ok = true;
                break;
            }
        }
        if !ok {
            all_ok = false;
            eprintln!("proxy p{offset} did not come up");
        }
    }

    let mut metrics_body = String::new();
    for _ in 0..30 {
        match http_get(admin_port, "/metrics").await {
            Ok(body) if body.contains("meatus_client_connected") => {
                metrics_body = body;
                break;
            }
            _ => sleep(Duration::from_millis(200)).await,
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(all_ok, "not all ten proxies accepted traffic");
    assert!(
        metrics_body.contains("meatus_client_connected 1"),
        "metrics missing connected gauge: {metrics_body}"
    );
    assert!(
        metrics_body.contains("meatus_stream_total{proxy=\"p0\"}"),
        "metrics missing proxy stream counter: {metrics_body}"
    );
    assert!(
        metrics_body.contains("meatus_bytes_total{proxy=\"p0\",direction=\"in\"}"),
        "metrics missing byte counter: {metrics_body}"
    );
}

#[tokio::test]
async fn spki_pin_accepts_match_and_rejects_mismatch() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    let admin_port = free_port();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-pin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).expect("write key");

    let (_, parsed) =
        x509_parser::parse_x509_certificate(certified.cert.der()).expect("parse cert");
    use sha2::Digest;
    let digest = sha2::Sha256::digest(parsed.tbs_certificate.subject_pki.raw);
    let pin = format!("sha256:{}", hex::encode(digest));

    let server_config = format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{cert}"
tls_key = "{key}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        cert = cert_path.display(),
        key = key_path.display(),
    );

    let client_template = |pin: &str| {
        format!(
            r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
server_public_key_pin = "{pin}"
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
"#,
            cert = cert_path.display(),
        )
    };

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let server_cancel = CancellationToken::new();
    let server_task = {
        let cancel = server_cancel.clone();
        tokio::spawn(async move { meatus_server::run_until(server_state, cancel).await })
    };

    let good_state =
        ClientState::new(parse_client_config(&client_template(&pin)).expect("good cfg"))
            .expect("good state");
    let good_cancel = CancellationToken::new();
    let good_task = {
        let cancel = good_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(good_state, cancel).await })
    };

    let mut pinned_ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            pinned_ok = true;
            break;
        }
    }
    good_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), good_task).await;
    assert!(pinned_ok, "client with matching SPKI pin did not connect");

    // 端口释放后再用错误 pin 连接，预期始终无法建立代理。
    sleep(Duration::from_millis(500)).await;
    let bad_pin = format!("sha256:{}", "00".repeat(32));
    let bad_state =
        ClientState::new(parse_client_config(&client_template(&bad_pin)).expect("bad cfg"))
            .expect("bad state");
    let bad_cancel = CancellationToken::new();
    let bad_task = {
        let cancel = bad_cancel.clone();
        tokio::spawn(async move { meatus_client::run_until(bad_state, cancel).await })
    };
    sleep(Duration::from_secs(2)).await;
    let mismatch_rejected = probe(proxy_port).await.is_err();
    bad_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), bad_task).await;
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(mismatch_rejected, "client with wrong SPKI pin was accepted");
}

fn client_config_text(
    control_port: u16,
    cert: &std::path::Path,
    echo0: u16,
    proxy0: u16,
    extra: Option<(u16, u16)>,
) -> String {
    let mut text = format!(
        r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "p0"
type = "tcp"
local_addr = "127.0.0.1:{echo0}"
remote_port = {proxy0}
"#,
        cert = cert.display(),
    );
    if let Some((echo1, proxy1)) = extra {
        text.push_str(&format!(
            r#"
[[proxy]]
name = "p1"
type = "tcp"
local_addr = "127.0.0.1:{echo1}"
remote_port = {proxy1}
"#
        ));
    }
    text
}

#[tokio::test]
#[ignore = "libtest 屏蔽 SIGHUP；用 meatus CLI 手动验收热重载"]
async fn sighup_hot_reload_add_and_remove_without_disconnect() {
    let _guard = TEST_MUTEX.lock().await;
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let mut echo_ports = Vec::new();
    for _ in 0..2 {
        let port = free_port();
        echo_ports.push(port);
        spawn_echo(port).await;
    }
    let control_port = free_port();
    let admin_port = free_port();
    let base = range_ports(2);
    let dir = std::env::temp_dir().join(format!("meatus-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let config_path = dir.join("client.toml");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    std::fs::write(&cert_path, certified.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).expect("write key");
    std::fs::write(
        &config_path,
        client_config_text(control_port, &cert_path, echo_ports[0], base, None),
    )
    .expect("write client config");

    let server_config = format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{cert}"
tls_key = "{key}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{base}, {}]

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        base + 1,
        cert = cert_path.display(),
        key = key_path.display(),
    );

    let server_state = ServerState::new(parse_server_config(&server_config).expect("server cfg"))
        .expect("server state");
    let client_state = ClientState::new(
        parse_client_config(&std::fs::read_to_string(&config_path).expect("read cfg"))
            .expect("client cfg"),
    )
    .expect("client state")
    .with_config_path(&config_path);

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

    let mut p0_ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        if probe(base).await.is_ok() {
            p0_ok = true;
            break;
        }
    }
    assert!(p0_ok, "initial proxy p0 did not come up");

    std::fs::write(
        &config_path,
        client_config_text(
            control_port,
            &cert_path,
            echo_ports[0],
            base,
            Some((echo_ports[1], base + 1)),
        ),
    )
    .expect("rewrite config with p1");
    let status = std::process::Command::new("kill")
        .args(["-HUP", &std::process::id().to_string()])
        .status()
        .expect("send SIGHUP");
    assert!(status.success(), "kill -HUP failed");

    let mut p1_ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        if probe(base + 1).await.is_ok() {
            p1_ok = true;
            break;
        }
    }
    assert!(p1_ok, "hot-added proxy p1 did not come up");
    assert!(
        probe(base).await.is_ok(),
        "existing proxy p0 broke after reload"
    );

    std::fs::write(
        &config_path,
        client_config_text(control_port, &cert_path, echo_ports[0], base, None),
    )
    .expect("rewrite config without p1");
    let status = std::process::Command::new("kill")
        .args(["-HUP", &std::process::id().to_string()])
        .status()
        .expect("send SIGHUP");
    assert!(status.success(), "kill -HUP failed");

    let mut p1_gone = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        if probe(base + 1).await.is_err() {
            p1_gone = true;
            break;
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    assert!(p1_gone, "hot-removed proxy p1 still accepting traffic");
    assert!(
        probe(base).await.is_ok(),
        "existing proxy p0 broke after removal"
    );
}

async fn spawn_http_echo(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind http echo");
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("http accept");
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&request);
                let path = request_text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body = format!("hello:{path}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
}

async fn http_get_host(port: u16, host: &str, path: &str) -> Result<String, String> {
    let mut stream = timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|err| format!("connect: {err}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| format!("write: {err}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|err| format!("read: {err}"))?;
    Ok(String::from_utf8_lossy(&response).to_string())
}

#[tokio::test]
async fn http_host_routing_and_404() {
    let _guard = TEST_MUTEX.lock().await;
    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let admin_port = free_port();
    let local_port = free_port();
    spawn_http_echo(local_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-http-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[http]
enabled = true
bind = "127.0.0.1:{http_port}"
https_bind = "127.0.0.1:{https_port}"
domain = "test.local"

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        cert = cert_path.display(),
        key = key_path.display(),
        proxy_port = free_port(),
    );

    let client_config = format!(
        r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "web"
type = "http"
local_addr = "127.0.0.1:{local_port}"
custom_domains = ["*.test.local"]

[[proxy]]
name = "web-exact"
type = "http"
local_addr = "127.0.0.1:{local_port}"
custom_domains = ["exact.test.local"]
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

    let mut wildcard_routed = false;
    let mut exact_routed = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        if !wildcard_routed
            && let Ok(body) = http_get_host(http_port, "app.test.local", "/hello").await
            && body.contains("hello:/hello")
        {
            wildcard_routed = true;
        }
        if !exact_routed
            && let Ok(body) = http_get_host(http_port, "exact.test.local", "/exact").await
            && body.contains("hello:/exact")
        {
            exact_routed = true;
        }
        if wildcard_routed && exact_routed {
            break;
        }
    }
    let not_found = http_get_host(http_port, "other.example.net", "/hello").await;

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;

    assert!(wildcard_routed, "http wildcard routing failed");
    assert!(exact_routed, "http exact routing failed");
    assert!(
        not_found.as_ref().is_ok_and(|body| body.contains("404")),
        "unknown host should return 404: {not_found:?}"
    );
}

async fn spawn_tls_http_echo(
    port: u16,
    cert_path: std::path::PathBuf,
    key_path: std::path::PathBuf,
) {
    let cert_file = std::fs::File::open(cert_path).expect("open local cert");
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .expect("parse local certs");
    let key_file = std::fs::File::open(key_path).expect("open local key");
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key_file))
        .expect("parse local key")
        .expect("private key");
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("build local tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind local tls");
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("local tls accept");
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let mut tls = acceptor.accept(socket).await.expect("local tls handshake");
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = tls.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhello-secure";
                let _ = tls.write_all(response).await;
                let _ = tls.shutdown().await;
            });
        }
    });
}

#[tokio::test]
async fn https_sni_passthrough() {
    let _guard = TEST_MUTEX.lock().await;
    let control_port = free_port();
    let http_port = free_port();
    let https_port = free_port();
    let admin_port = free_port();
    let local_port = free_port();

    let dir = std::env::temp_dir().join(format!("meatus-sni-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let control_cert = dir.join("control-cert.pem");
    let control_key = dir.join("control-key.pem");
    let control =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("control cert");
    std::fs::write(&control_cert, control.cert.pem()).expect("write control cert");
    std::fs::write(&control_key, control.signing_key.serialize_pem()).expect("write control key");

    let app_cert = dir.join("app-cert.pem");
    let app_key = dir.join("app-key.pem");
    let app =
        rcgen::generate_simple_self_signed(vec!["app.test.local".to_string()]).expect("app cert");
    std::fs::write(&app_cert, app.cert.pem()).expect("write app cert");
    std::fs::write(&app_key, app.signing_key.serialize_pem()).expect("write app key");
    spawn_tls_http_echo(local_port, app_cert.clone(), app_key.clone()).await;

    let server_config = format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{control_cert}"
tls_key = "{control_key}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[http]
enabled = true
bind = "127.0.0.1:{http_port}"
https_bind = "127.0.0.1:{https_port}"
domain = "test.local"

[admin]
bind = "127.0.0.1:{admin_port}"
"#,
        control_cert = control_cert.display(),
        control_key = control_key.display(),
        proxy_port = free_port(),
    );

    let client_config = format!(
        r#"
[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{control_cert}"
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "secure-web"
type = "https"
local_addr = "127.0.0.1:{local_port}"
custom_domains = ["app.test.local"]
"#,
        control_cert = control_cert.display(),
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

    let mut roots = rustls::RootCertStore::empty();
    roots.add(app.cert.der().clone()).expect("add trust anchor");
    let client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(client_tls));
    let server_name = rustls::pki_types::ServerName::try_from("app.test.local")
        .expect("server name")
        .to_owned();

    let mut ok = false;
    for _ in 0..30 {
        sleep(Duration::from_millis(200)).await;
        let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", https_port)).await else {
            continue;
        };
        let mut tls = match connector.connect(server_name.clone(), stream).await {
            Ok(tls) => tls,
            Err(_) => continue,
        };
        let request = "GET /secure HTTP/1.1\r\nHost: app.test.local\r\nConnection: close\r\n\r\n";
        if tls.write_all(request.as_bytes()).await.is_err() {
            continue;
        }
        let mut response = Vec::new();
        let _ = tls.read_to_end(&mut response).await;
        if String::from_utf8_lossy(&response).contains("hello-secure") {
            ok = true;
            break;
        }
    }

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    assert!(ok, "https sni passthrough failed");
}

async fn socks_connect(
    socks_port: u16,
    target: (u8, u8, u8, u8),
    target_port: u16,
) -> Result<(TcpStream, u8), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", socks_port))
        .await
        .map_err(|err| err.to_string())?;
    stream
        .write_all(&[5, 1, 0])
        .await
        .map_err(|err| err.to_string())?;
    let mut greeting = [0u8; 2];
    stream
        .read_exact(&mut greeting)
        .await
        .map_err(|err| err.to_string())?;
    if greeting != [5, 0] {
        return Err(format!("bad greeting {greeting:?}"));
    }
    let mut request = vec![5, 1, 0, 1];
    request.extend_from_slice(&[target.0, target.1, target.2, target.3]);
    request.extend_from_slice(&target_port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|err| err.to_string())?;
    let mut reply = [0u8; 10];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|err| err.to_string())?;
    if reply[0] != 5 {
        return Err(format!("bad reply {reply:?}"));
    }
    Ok((stream, reply[1]))
}

async fn admin_get(port: u16, token: Option<&str>, path: &str) -> Result<(u16, String), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|err| err.to_string())?;
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .map_err(|err| err.to_string())?;
    let code = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Ok((code, response))
}

#[tokio::test]
async fn socks5_tunnel_roundtrip_and_status() {
    let _guard = TEST_MUTEX.lock().await;
    let echo_port = free_port();
    let control_port = free_port();
    let socks_port = free_port();
    let admin_port = free_port();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-socks-it-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[socks5]
enabled = true
bind = "127.0.0.1:{socks_port}"

[admin]
bind = "127.0.0.1:{admin_port}"
admin_token = "test-admin-token"
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
client_id = "nas"
tenant = "default"
token = "secret"

[[proxy]]
name = "socks"
type = "socks5"
local_addr = "127.0.0.1:0"
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

    let mut tunneled = false;
    for _ in 0..50 {
        sleep(Duration::from_millis(200)).await;
        if let Ok((mut stream, 0)) = socks_connect(socks_port, (127, 0, 0, 1), echo_port).await
            && stream.write_all(b"ping").await.is_ok()
        {
            let mut echoed = [0u8; 4];
            if stream.read_exact(&mut echoed).await.is_ok() && echoed == *b"ping" {
                tunneled = true;
                break;
            }
        }
    }
    assert!(tunneled, "socks5 CONNECT did not succeed");

    // Public target must be rejected by allow_cidrs.
    let denied = socks_connect(socks_port, (192, 0, 2, 1), 80)
        .await
        .map(|(_, code)| code);
    assert_eq!(denied, Ok(2), "public target should be denied");

    // Admin status requires the bearer token and reflects the socks5 proxy.
    let (unauthorized, _) = admin_get(admin_port, None, "/api/v1/status")
        .await
        .expect("admin request");
    assert_eq!(unauthorized, 401, "status without token must be 401");
    let (ok, status) = admin_get(admin_port, Some("test-admin-token"), "/api/v1/status")
        .await
        .expect("admin request");
    assert_eq!(ok, 200, "status with token must be 200");
    assert!(
        status.contains("\"type\":\"socks5\""),
        "status missing socks5 proxy: {status}"
    );
    let (ok, metrics) = admin_get(admin_port, Some("test-admin-token"), "/metrics")
        .await
        .expect("metrics request");
    assert_eq!(ok, 200);
    assert!(metrics.contains("meatus_client_connected 1"));
    assert!(metrics.contains("meatus_proxy_active{proxy=\"socks\"}"));

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
}

#[tokio::test]
async fn pool_size_and_stream_limit_allow_two_simultaneous_tunnels() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-pool-it-{}", std::process::id()));
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
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"

[client.pool]
enabled = true
size = 2
max_streams_per_conn = 1
idle_timeout_s = 300

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut tunneled = false;
    for _ in 0..80 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            tunneled = true;
            break;
        }
    }
    assert!(tunneled, "tunnel did not come up");

    // 等数据连接注册完成；单连接 max_streams=1 时第二条并发连接会被拒绝。
    sleep(Duration::from_secs(1)).await;
    let mut first = timeout(
        Duration::from_secs(5),
        TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .expect("first connect timeout")
    .expect("first connect");
    first.write_all(b"one").await.expect("write one");
    let mut buf = [0u8; 3];
    timeout(Duration::from_secs(5), first.read_exact(&mut buf))
        .await
        .expect("read one timeout")
        .expect("read one");
    assert_eq!(&buf, b"one");

    let mut second = timeout(
        Duration::from_secs(5),
        TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .expect("second connect timeout")
    .expect("second connect");
    second.write_all(b"two").await.expect("write two");
    let mut buf = [0u8; 3];
    timeout(Duration::from_secs(5), second.read_exact(&mut buf))
        .await
        .expect("read two timeout")
        .expect("read two");
    assert_eq!(&buf, b"two");

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn quic_pool_allows_two_simultaneous_tunnels() {
    let _guard = TEST_MUTEX.lock().await;
    let (echo_port, control_port, proxy_port) = distinct_ports();
    spawn_echo(echo_port).await;

    let dir = std::env::temp_dir().join(format!("meatus-quic-pool-it-{}", std::process::id()));
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
quic = true

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:0"
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
client_id = "nas"
tenant = "default"
token = "secret"
transport = "quic"

[client.pool]
enabled = true
size = 2
max_streams_per_conn = 1
idle_timeout_s = 300

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
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

    let mut tunneled = false;
    for _ in 0..80 {
        sleep(Duration::from_millis(200)).await;
        if probe(proxy_port).await.is_ok() {
            tunneled = true;
            break;
        }
    }
    assert!(tunneled, "tunnel did not come up");

    // 等数据连接注册完成；单连接 max_streams=1 时第二条并发连接会被拒绝。
    sleep(Duration::from_secs(1)).await;
    let mut first = timeout(
        Duration::from_secs(5),
        TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .expect("first connect timeout")
    .expect("first connect");
    first.write_all(b"one").await.expect("write one");
    let mut buf = [0u8; 3];
    timeout(Duration::from_secs(5), first.read_exact(&mut buf))
        .await
        .expect("read one timeout")
        .expect("read one");
    assert_eq!(&buf, b"one");

    let mut second = timeout(
        Duration::from_secs(5),
        TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .expect("second connect timeout")
    .expect("second connect");
    second.write_all(b"two").await.expect("write two");
    let mut buf = [0u8; 3];
    timeout(Duration::from_secs(5), second.read_exact(&mut buf))
        .await
        .expect("read two timeout")
        .expect("read two");
    assert_eq!(&buf, b"two");

    client_cancel.cancel();
    server_cancel.cancel();
    let _ = timeout(Duration::from_secs(5), client_task).await;
    let _ = timeout(Duration::from_secs(5), server_task).await;
    let _ = std::fs::remove_dir_all(dir);
}
