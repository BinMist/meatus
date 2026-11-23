#![forbid(unsafe_code)]
//! 服务端视角的 relay 组成员上下文。
//!
//! G2-01 只负责把验签后的 bundle 与本地成员绑定到运行时；peer 通信、
//! 证书同步与路由镜像分别由 G2-02 / G3 接入。

use std::sync::Arc;

use meatus_core::{ConfigError, RelayBundle, RelayMember, RelayRole, ServerConfig};

#[derive(Debug, Clone)]
pub struct RelayContext {
    pub bundle: RelayBundle,
    pub member: RelayMember,
}

impl RelayContext {
    pub fn new(config: &ServerConfig, bundle: RelayBundle) -> Result<Self, ConfigError> {
        let relay = config
            .relay
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("relay context requires [relay]".to_string()))?;
        let member = bundle
            .relays
            .iter()
            .find(|member| member.id == relay.id)
            .cloned()
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "relay.id {:?} is not present in verified bundle",
                    relay.id
                ))
            })?;
        if relay.cert_sync != meatus_core::RelayCertSync::Off
            && let Some(missing) = bundle
                .relays
                .iter()
                .find(|peer| peer.id != relay.id && peer.peer_addr.is_none())
        {
            return Err(ConfigError::Invalid(format!(
                "relay.id {:?} must set peer_addr for cert_sync",
                missing.id
            )));
        }
        Ok(Self { bundle, member })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.member.id
    }

    #[must_use]
    pub fn group(&self) -> &str {
        &self.bundle.group
    }

    #[must_use]
    pub fn role(&self) -> RelayRole {
        self.member.role
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.bundle.epoch
    }

    /// 组内对端成员；用于后续 peer 健康、证书同步和路由镜像。
    pub fn peers(&self) -> impl Iterator<Item = &RelayMember> {
        self.bundle
            .relays
            .iter()
            .filter(move |member| member.id != self.member.id)
    }
}

pub fn build_context(
    config: &ServerConfig,
    bundle: Option<RelayBundle>,
) -> Result<Option<Arc<RelayContext>>, ConfigError> {
    bundle
        .map(|bundle| RelayContext::new(config, bundle).map(Arc::new))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use meatus_core::{
        RelayBundleBody, RelayCertSync, RelaySigningKey, config::RelayRole, load_bundle,
        parse_server_config, write_bundle,
    };

    struct Fixture {
        dir: PathBuf,
        bundle: PathBuf,
        public: String,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("meatus-relay-g2-{}-{label}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir");
            let (signing, _pkcs8) = RelaySigningKey::generate().expect("generate key");
            let public = signing.public_key();
            let body = RelayBundleBody {
                version: 1,
                group: "prod".to_string(),
                epoch: 3,
                generated_at: "2026-11-23T00:00:00Z".to_string(),
                relays: vec![
                    RelayMember {
                        id: "relay-a".to_string(),
                        role: RelayRole::Primary,
                        control_addr: "relay-a.example.com:7000".to_string(),
                        http_domains: vec!["*.example.com".to_string()],
                        peer_addr: Some("relay-a.example.com:7401".to_string()),
                        ports: [20_000, 20_999],
                    },
                    RelayMember {
                        id: "relay-b".to_string(),
                        role: RelayRole::Secondary,
                        control_addr: "relay-b.example.com:7000".to_string(),
                        http_domains: vec!["*.example.com".to_string()],
                        peer_addr: Some("relay-b.example.com:7401".to_string()),
                        ports: [21_000, 21_999],
                    },
                ],
            };
            let bundle = signing.sign(&body).expect("sign bundle");
            let path = dir.join("relay-bundle.json");
            write_bundle(&path, &bundle).expect("write bundle");
            Self {
                dir,
                bundle: path,
                public,
            }
        }

        fn config(&self, id: &str, role: &str, advertise: &str, ports: [u16; 2]) -> ServerConfig {
            let text = format!(
                r#"
[server]
bind_addr = "127.0.0.1"
control_port = 7000

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{}" }},
]

[ports]
range = {ports:?}

[http]
enabled = true
bind = "127.0.0.1:18080"
https_bind = "127.0.0.1:18443"
domain = "tunnel.example.com"

[relay]
id = "{id}"
group = "prod"
role = "{role}"
advertise_addr = "{advertise}"
bundle = "{}"
bundle_pubkey = "{}"
peer_secret_file = "{}"
"#,
                "00".repeat(32),
                self.bundle.display(),
                self.public,
                self.dir.join("relay-peer.key").display()
            );
            parse_server_config(&text).expect("server config")
        }

        fn cleanup(self) {
            let _ = fs::remove_dir_all(self.dir);
        }
    }

    #[test]
    fn cert_sync_requires_every_peer_addr() {
        let fixture = Fixture::new("peer-addr");
        let mut config = fixture.config(
            "relay-a",
            "primary",
            "relay-a.example.com:7000",
            [20_000, 20_999],
        );
        config.relay.as_mut().expect("relay").cert_sync = RelayCertSync::Push;
        let mut bundle = load_bundle(&fixture.bundle).expect("bundle");
        bundle.relays[1].peer_addr = None;
        let err = RelayContext::new(&config, bundle).expect_err("missing peer_addr");
        assert!(err.to_string().contains("peer_addr"), "{err}");
        fixture.cleanup();
    }

    #[test]
    fn both_group_members_start_with_shared_domains() {
        let fixture = Fixture::new("both");
        let primary = fixture.config(
            "relay-a",
            "primary",
            "relay-a.example.com:7000",
            [20_000, 20_999],
        );
        let secondary = fixture.config(
            "relay-b",
            "secondary",
            "relay-b.example.com:7000",
            [21_000, 21_999],
        );

        let primary_state = crate::ServerState::new(primary).expect("primary state");
        let primary_relay = primary_state.relay.as_ref().expect("primary relay");
        assert_eq!(primary_relay.id(), "relay-a");
        assert_eq!(primary_relay.group(), "prod");
        assert_eq!(primary_relay.role(), RelayRole::Primary);
        assert_eq!(primary_relay.epoch(), 3);
        assert_eq!(primary_relay.peers().count(), 1);
        assert!(
            primary_state
                .planned_listeners()
                .iter()
                .any(|listener| listener.starts_with("https"))
        );

        let secondary_state = crate::ServerState::new(secondary).expect("secondary state");
        let secondary_relay = secondary_state.relay.as_ref().expect("secondary relay");
        assert_eq!(secondary_relay.id(), "relay-b");
        assert_eq!(secondary_relay.role(), RelayRole::Secondary);
        assert_eq!(secondary_relay.epoch(), 3);
        assert!(
            secondary_state
                .planned_listeners()
                .iter()
                .any(|listener| listener.starts_with("http"))
        );

        fixture.cleanup();
    }

    #[test]
    fn http_domain_must_be_covered_by_group_bundle() {
        let fixture = Fixture::new("domain");
        let mut config = fixture.config(
            "relay-a",
            "primary",
            "relay-a.example.com:7000",
            [20_000, 20_999],
        );
        config.http.domain = "other.example".to_string();
        let err = match crate::ServerState::new(config) {
            Ok(_) => panic!("domain must be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("http.domain"), "{err}");
        fixture.cleanup();
    }

    #[test]
    fn relay_context_requires_verified_member() {
        let fixture = Fixture::new("context");
        let config = fixture.config(
            "relay-a",
            "primary",
            "relay-a.example.com:7000",
            [20_000, 20_999],
        );
        let bundle = meatus_core::load_bundle(&fixture.bundle).expect("load bundle");
        let context = RelayContext::new(&config, bundle).expect("context");
        assert_eq!(context.peers().count(), 1);

        let mut wrong = config;
        wrong.relay.as_mut().expect("relay").id = "missing".to_string();
        let err = RelayContext::new(
            &wrong,
            meatus_core::load_bundle(&fixture.bundle).expect("bundle"),
        )
        .expect_err("missing member");
        assert!(err.to_string().contains("not present"), "{err}");
        fixture.cleanup();
    }
}
