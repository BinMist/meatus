#![forbid(unsafe_code)]
//! G2-02 端到端：primary 主动推送证书，secondary 落盘并可由只读证书库重新加载。

use std::net::TcpListener as StdTcpListener;
use std::path::Path;
use std::time::Duration;

use meatus_core::{
    RelayBundleBody, RelayMember, RelayRole, RelaySigningKey, parse_server_config, write_bundle,
};
use meatus_server::ServerState;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

static TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local addr").port()
}

fn write_server_tls(dir: &Path) -> (String, String) {
    let cert = dir.join("server-cert.pem");
    let key = dir.join("server-key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("server cert");
    std::fs::write(&cert, certified.cert.pem()).expect("write cert");
    std::fs::write(&key, certified.signing_key.serialize_pem()).expect("write key");
    (cert.display().to_string(), key.display().to_string())
}

fn write_relay_cert(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("cert dir");
    let certified =
        rcgen::generate_simple_self_signed(vec!["app.relay.test".to_string()]).expect("relay cert");
    let pem = format!(
        "{}{}",
        certified.cert.pem(),
        certified.signing_key.serialize_pem()
    );
    let name = format!("cert-{}.pem", "a".repeat(64));
    let path = dir.join(&name);
    std::fs::write(&path, &pem).expect("write relay cert");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("relay cert mode");
    }
    pem
}

#[allow(clippy::too_many_arguments)]
fn server_config(
    id: &str,
    role: &str,
    control_port: u16,
    http_port: u16,
    https_port: u16,
    admin_port: u16,
    peer_port: u16,
    acme_enabled: bool,
    sync_mode: &str,
    cert_dir: &Path,
    tls: (&str, &str),
    bundle: &Path,
    public: &str,
    peer_secret: &Path,
) -> String {
    let acme = if acme_enabled {
        format!(
            r#"
[acme]
enabled = true
email = "ops@relay.test"
staging = true
cert_dir = "{}"
"#,
            cert_dir.display()
        )
    } else {
        format!(
            r#"
[acme]
enabled = false
cert_dir = "{}"
"#,
            cert_dir.display()
        )
    };
    format!(
        r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{tls_cert}"
tls_key = "{tls_key}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{token}" }},
]

[ports]
range = [{first}, {last}]

[http]
enabled = true
bind = "127.0.0.1:{http_port}"
https_bind = "127.0.0.1:{https_port}"
domain = "app.relay.test"

[admin]
bind = "127.0.0.1:{admin_port}"

[relay]
id = "{id}"
group = "prod"
role = "{role}"
advertise_addr = "127.0.0.1:{control_port}"
bundle = "{bundle}"
bundle_pubkey = "{public}"
peer_secret_file = "{peer_secret}"
health_bind = "127.0.0.1:{peer_port}"
cert_sync = "{sync_mode}"
{acme}"#,
        tls_cert = tls.0,
        tls_key = tls.1,
        token = "00".repeat(32),
        first = if role == "primary" { 20_000 } else { 21_000 },
        last = if role == "primary" { 20_999 } else { 21_999 },
        id = id,
        role = role,
        bundle = bundle.display(),
        public = public,
        peer_secret = peer_secret.display(),
        acme = acme,
        sync_mode = sync_mode,
    )
}

async fn wait_health(port: u16) -> String {
    let url = format!("http://127.0.0.1:{port}/relay/v1/health");
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if let Ok(response) = client.get(&url).send().await
            && let Ok(body) = response.text().await
        {
            return body;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("peer health endpoint did not become ready on {port}");
}

async fn run_case(mode: &str) {
    let _guard = TEST_MUTEX.lock().await;
    let dir = std::env::temp_dir().join(format!("meatus-relay-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    let primary_cert_dir = dir.join("acme-primary");
    let secondary_cert_dir = dir.join("acme-secondary");
    let expected_pem = write_relay_cert(&primary_cert_dir);
    std::fs::create_dir_all(&secondary_cert_dir).expect("secondary cert dir");

    let tls = write_server_tls(&dir);
    let peer_secret = dir.join("relay-peer.key");
    std::fs::write(&peer_secret, b"0123456789abcdef0123456789abcdef").expect("peer secret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&peer_secret, std::fs::Permissions::from_mode(0o600))
            .expect("peer secret mode");
    }

    let primary_peer = free_port();
    let secondary_peer = free_port();
    let primary_control = free_port();
    let secondary_control = free_port();
    let (signing, _) = RelaySigningKey::generate().expect("relay key");
    let body = RelayBundleBody {
        version: 1,
        group: "prod".to_string(),
        epoch: 3,
        generated_at: "2026-11-23T00:00:00Z".to_string(),
        relays: vec![
            RelayMember {
                id: "relay-a".to_string(),
                role: RelayRole::Primary,
                control_addr: format!("127.0.0.1:{primary_control}"),
                http_domains: vec!["*.relay.test".to_string()],
                peer_addr: Some(format!("127.0.0.1:{primary_peer}")),
                ports: [20_000, 20_999],
            },
            RelayMember {
                id: "relay-b".to_string(),
                role: RelayRole::Secondary,
                control_addr: format!("127.0.0.1:{secondary_control}"),
                http_domains: vec!["*.relay.test".to_string()],
                peer_addr: Some(format!("127.0.0.1:{secondary_peer}")),
                ports: [21_000, 21_999],
            },
        ],
    };
    let bundle = signing.sign(&body).expect("sign bundle");
    let bundle_path = dir.join("relay-bundle.json");
    write_bundle(&bundle_path, &bundle).expect("write bundle");

    let secondary_cfg = parse_server_config(&server_config(
        "relay-b",
        "secondary",
        secondary_control,
        free_port(),
        free_port(),
        free_port(),
        secondary_peer,
        false,
        mode,
        &secondary_cert_dir,
        (&tls.0, &tls.1),
        &bundle_path,
        &signing.public_key(),
        &peer_secret,
    ))
    .expect("secondary config");
    let secondary_state = ServerState::new(secondary_cfg).expect("secondary state");
    let secondary_cancel = CancellationToken::new();

    let primary_cfg = parse_server_config(&server_config(
        "relay-a",
        "primary",
        primary_control,
        free_port(),
        free_port(),
        free_port(),
        primary_peer,
        true,
        mode,
        &primary_cert_dir,
        (&tls.0, &tls.1),
        &bundle_path,
        &signing.public_key(),
        &peer_secret,
    ))
    .expect("primary config");
    let primary_state = ServerState::new(primary_cfg).expect("primary state");
    let primary_cancel = CancellationToken::new();

    let primary_task;
    let secondary_task;
    if mode == "push" {
        secondary_task = {
            let cancel = secondary_cancel.clone();
            tokio::spawn(async move { meatus_server::run_until(secondary_state, cancel).await })
        };
        let health = wait_health(secondary_peer).await;
        assert!(health.contains("\"role\":\"secondary\""), "{health}");
        primary_task = {
            let cancel = primary_cancel.clone();
            tokio::spawn(async move { meatus_server::run_until(primary_state, cancel).await })
        };
    } else {
        primary_task = {
            let cancel = primary_cancel.clone();
            tokio::spawn(async move { meatus_server::run_until(primary_state, cancel).await })
        };
        let health = wait_health(primary_peer).await;
        assert!(health.contains("\"role\":\"primary\""), "{health}");
        secondary_task = {
            let cancel = secondary_cancel.clone();
            tokio::spawn(async move { meatus_server::run_until(secondary_state, cancel).await })
        };
    }

    let mut applied = None;
    for _ in 0..50 {
        let entries = std::fs::read_dir(&secondary_cert_dir)
            .expect("secondary cert dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("cert-") && name.ends_with(".pem"))
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        if let Some(path) = entries.first() {
            applied = Some(std::fs::read_to_string(path).expect("read synced cert"));
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let applied = applied.expect("secondary must receive cert within 5 seconds");
    assert_eq!(applied, expected_pem, "secondary cert must match primary");

    primary_cancel.cancel();
    secondary_cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), primary_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), secondary_task).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn primary_pushes_cert_and_secondary_loads_it() {
    run_case("push").await;
}

#[tokio::test]
async fn secondary_pulls_cert_from_primary() {
    run_case("pull").await;
}
