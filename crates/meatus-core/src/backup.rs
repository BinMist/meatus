//! 配置备份与恢复：备份文件为 JSON 信封，配置正文已脱敏，恢复前校验版本与校验和。
//!
//! 备份只保留 token 的 sha256 / 路径引用，不写入 admin token、DNS API token、
//! 客户端明文 token 或私钥内容。恢复时由调用方提供当前进程已持有的秘密值。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value as YamlValue};
use sha2::{Digest, Sha256};
use toml_edit::{Document, value};

use crate::config::{
    ConfigFormat, ConfigKind, parse_client_config_with_format, parse_server_config_with_format,
};
use crate::config_edit::write_atomic;

/// 备份中秘密值的占位符；恢复时必须被当前秘密值替换。
pub const SECRET_PLACEHOLDER: &str = "__MEATUS_SECRET_PRESERVED__";
pub const BACKUP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateReference {
    pub role: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupDocument {
    pub schema_version: u32,
    pub meatus_version: String,
    pub created_at_ms: i64,
    pub kind: String,
    pub format: String,
    pub config: String,
    pub config_sha256: String,
    pub certificates: Vec<CertificateReference>,
    pub checksum: String,
}

#[derive(Debug, Clone, Default)]
pub struct BackupSecrets {
    pub admin_token: Option<String>,
    pub dns_api_token: Option<String>,
    pub client_token: Option<String>,
    pub admin_user_tokens: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub kind: ConfigKind,
    pub format: ConfigFormat,
    pub backup_path: Option<PathBuf>,
    pub warnings: Vec<String>,
}

/// 从配置文件文本生成备份 JSON。`raw` 为当前文件原文。
pub fn create_backup(
    path: &Path,
    raw: &str,
    meatus_version: &str,
) -> Result<BackupDocument, String> {
    let kind = crate::config::detect_kind_with_format(raw, ConfigFormat::from_path(path))
        .ok_or_else(|| "无法识别配置类型（缺少 [server] 或 [client]）".to_string())?;
    let format = ConfigFormat::from_path(path);
    let (redacted, certificates) = match kind {
        ConfigKind::Server => {
            let config = parse_server_config_with_format(raw, format)
                .map_err(|err| format!("parse server config: {err}"))?;
            let mut certificates = Vec::new();
            push_ref(
                &mut certificates,
                "server.tls_cert",
                config.server.tls_cert.as_deref(),
            );
            push_ref(
                &mut certificates,
                "server.tls_key",
                config.server.tls_key.as_deref(),
            );
            push_ref(
                &mut certificates,
                "admin.tls.cert",
                config.admin.tls.cert.as_deref(),
            );
            push_ref(
                &mut certificates,
                "admin.tls.key",
                config.admin.tls.key.as_deref(),
            );
            if config.acme.enabled {
                push_ref(
                    &mut certificates,
                    "acme.cert_dir",
                    Some(config.acme.cert_dir.as_path()),
                );
            }
            (redact_server(raw, format)?, certificates)
        }
        ConfigKind::Client => {
            let config = parse_client_config_with_format(raw, format)
                .map_err(|err| format!("parse client config: {err}"))?;
            let mut certificates = Vec::new();
            push_ref(
                &mut certificates,
                "client.server_ca_cert",
                config.client.server_ca_cert.as_deref(),
            );
            (redact_client(raw, format)?, certificates)
        }
    };

    let config_sha256 = sha256_hex(redacted.as_bytes());
    let mut document = BackupDocument {
        schema_version: BACKUP_SCHEMA_VERSION,
        meatus_version: meatus_version.to_string(),
        created_at_ms: now_ms(),
        kind: match kind {
            ConfigKind::Server => "server".to_string(),
            ConfigKind::Client => "client".to_string(),
        },
        format: format.label().to_string(),
        config: redacted,
        config_sha256,
        certificates,
        checksum: String::new(),
    };
    document.checksum = checksum(&document);
    Ok(document)
}

/// 校验备份并写回目标配置；成功前不会覆盖现有文件。
pub fn restore_backup(
    backup: &BackupDocument,
    target: &Path,
    secrets: &BackupSecrets,
) -> Result<RestoreOutcome, String> {
    verify_backup(backup)?;
    let format = ConfigFormat::from_path(target);
    if backup.format != format.label() {
        return Err(format!(
            "备份格式为 {}，目标文件要求 {}",
            backup.format,
            format.label()
        ));
    }
    let target_kind = detect_target_kind(target)?;
    if backup.kind != kind_label(target_kind) {
        return Err(format!(
            "备份类型为 {}，目标文件检测为 {}",
            backup.kind,
            kind_label(target_kind)
        ));
    }

    let candidate = match target_kind {
        ConfigKind::Server => restore_server(&backup.config, format, secrets)?,
        ConfigKind::Client => restore_client(&backup.config, format, secrets)?,
    };
    if candidate.contains(SECRET_PLACEHOLDER) {
        return Err(
            "备份中包含未回填的秘密值；请用 --admin-token / --dns-api-token / --client-token 提供，或先修复当前配置"
                .to_string(),
        );
    }
    let mut warnings = Vec::new();
    match target_kind {
        ConfigKind::Server => {
            let config = parse_server_config_with_format(&candidate, format)
                .map_err(|err| format!("恢复后的 server 配置无效: {err}"))?;
            config
                .validate_runtime()
                .map_err(|err| format!("恢复后的 server 配置无效: {err}"))?;
            if secrets.admin_token.is_none() {
                warnings.push(
                    "备份中的 admin_token 未替换；如管理面非 loopback，请重新设置".to_string(),
                );
            }
        }
        ConfigKind::Client => {
            let config = parse_client_config_with_format(&candidate, format)
                .map_err(|err| format!("恢复后的 client 配置无效: {err}"))?;
            config
                .validate()
                .map_err(|err| format!("恢复后的 client 配置无效: {err}"))?;
            if secrets.client_token.is_none() {
                warnings.push("备份未包含客户端 token；恢复后需重新填写 client.token".to_string());
            }
        }
    }

    let backup_path = backup_existing(target)?;
    write_atomic(target, &candidate).map_err(|err| format!("写入配置失败: {err}"))?;
    Ok(RestoreOutcome {
        kind: target_kind,
        format,
        backup_path,
        warnings,
    })
}

/// 从当前配置文本提取可复用的秘密；解析失败时返回空，由调用方补充显式值。
#[must_use]
pub fn secrets_from_raw(path: &Path, raw: &str) -> BackupSecrets {
    let format = ConfigFormat::from_path(path);
    if let Ok(config) = crate::config::parse_server_config_unchecked_with_format(raw, format) {
        return BackupSecrets {
            admin_token: config.admin.admin_token,
            dns_api_token: config.acme.dns_api_token,
            client_token: None,
            admin_user_tokens: config
                .admin
                .users
                .into_iter()
                .filter_map(|user| user.token.map(|token| (user.name, token)))
                .collect(),
        };
    }
    if let Ok(config) = crate::config::parse_client_config_unchecked_with_format(raw, format) {
        return BackupSecrets {
            admin_token: None,
            dns_api_token: None,
            client_token: Some(config.client.token),
            admin_user_tokens: Vec::new(),
        };
    }
    BackupSecrets::default()
}

fn verify_backup(backup: &BackupDocument) -> Result<(), String> {
    if backup.schema_version != BACKUP_SCHEMA_VERSION {
        return Err(format!(
            "不支持的备份版本 {}，当前只接受 {}",
            backup.schema_version, BACKUP_SCHEMA_VERSION
        ));
    }
    let current_major = env!("CARGO_PKG_VERSION")
        .split('.')
        .next()
        .unwrap_or_default();
    let backup_major = backup.meatus_version.split('.').next().unwrap_or_default();
    if backup_major != current_major {
        return Err(format!(
            "备份来自 Meatus {}，与当前主线 {} 不兼容",
            backup.meatus_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    if checksum(backup) != backup.checksum {
        return Err("备份校验和不匹配；文件可能被修改".to_string());
    }
    if sha256_hex(backup.config.as_bytes()) != backup.config_sha256 {
        return Err("配置正文校验和不匹配；文件可能被修改".to_string());
    }
    Ok(())
}

fn detect_target_kind(path: &Path) -> Result<ConfigKind, String> {
    if !path.exists()
        && let Some(stem) = path.file_name().and_then(|name| name.to_str())
    {
        return Ok(if stem.starts_with("client") {
            ConfigKind::Client
        } else {
            ConfigKind::Server
        });
    }
    let raw = fs::read_to_string(path).map_err(|err| format!("读取 {}: {err}", path.display()))?;
    crate::config::detect_kind_with_format(&raw, ConfigFormat::from_path(path))
        .ok_or_else(|| "无法识别目标配置类型".to_string())
}

fn backup_existing(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let backup = path.with_file_name(format!("{name}.bak-{}", now_ms()));
    fs::copy(path, &backup)
        .map_err(|err| format!("备份当前配置到 {} 失败: {err}", backup.display()))?;
    Ok(Some(backup))
}

fn redact_server(raw: &str, format: ConfigFormat) -> Result<String, String> {
    match format {
        ConfigFormat::Yaml => redact_yaml(raw, true),
        _ => {
            let mut document = raw
                .parse::<Document>()
                .map_err(|err| format!("parse server config: {err}"))?;
            replace_toml(
                document.as_table_mut(),
                "admin",
                "admin_token",
                SECRET_PLACEHOLDER,
            );
            replace_admin_user_tokens(document.as_table_mut(), SECRET_PLACEHOLDER);
            replace_toml(
                document.as_table_mut(),
                "acme",
                "dns_api_token",
                SECRET_PLACEHOLDER,
            );
            Ok(document.to_string())
        }
    }
}

fn redact_client(raw: &str, format: ConfigFormat) -> Result<String, String> {
    match format {
        ConfigFormat::Yaml => redact_yaml(raw, false),
        _ => {
            let mut document = raw
                .parse::<Document>()
                .map_err(|err| format!("parse client config: {err}"))?;
            replace_toml(
                document.as_table_mut(),
                "client",
                "token",
                SECRET_PLACEHOLDER,
            );
            Ok(document.to_string())
        }
    }
}

fn restore_server(
    raw: &str,
    format: ConfigFormat,
    secrets: &BackupSecrets,
) -> Result<String, String> {
    match format {
        ConfigFormat::Yaml => restore_yaml(raw, secrets, &["admin_token", "dns_api_token"]),
        _ => {
            let mut document = raw
                .parse::<Document>()
                .map_err(|err| format!("parse server backup config: {err}"))?;
            apply_toml(
                document.as_table_mut(),
                "admin",
                "admin_token",
                secrets.admin_token.as_deref(),
            );
            apply_admin_user_tokens(document.as_table_mut(), &secrets.admin_user_tokens);
            apply_toml(
                document.as_table_mut(),
                "acme",
                "dns_api_token",
                secrets.dns_api_token.as_deref(),
            );
            ensure_no_user_token_placeholder(&document.to_string(), ConfigFormat::Toml)?;
            Ok(document.to_string())
        }
    }
}

fn restore_client(
    raw: &str,
    format: ConfigFormat,
    secrets: &BackupSecrets,
) -> Result<String, String> {
    match format {
        ConfigFormat::Yaml => restore_yaml(raw, secrets, &["token"]),
        _ => {
            let mut document = raw
                .parse::<Document>()
                .map_err(|err| format!("parse client backup config: {err}"))?;
            apply_toml(
                document.as_table_mut(),
                "client",
                "token",
                secrets.client_token.as_deref(),
            );
            Ok(document.to_string())
        }
    }
}

fn replace_toml(table: &mut toml_edit::Table, section: &str, key: &str, replacement: &str) {
    if let Some(item) = table.get_mut(section)
        && let Some(section) = item.as_table_mut()
        && let Some(item) = section.get_mut(key)
    {
        if item.as_str().is_some_and(is_env_placeholder) {
            return;
        }
        *item = value(replacement);
    }
}

fn apply_toml(table: &mut toml_edit::Table, section: &str, key: &str, secret: Option<&str>) {
    let Some(secret) = secret else {
        return;
    };
    if let Some(item) = table.get_mut(section)
        && let Some(section) = item.as_table_mut()
        && let Some(value_item) = section.get_mut(key)
        && value_item
            .as_str()
            .is_some_and(|value| value == SECRET_PLACEHOLDER)
    {
        *value_item = value(secret);
    }
}

fn redact_yaml(raw: &str, server: bool) -> Result<String, String> {
    let mut value: YamlValue =
        serde_yaml::from_str(raw).map_err(|err| format!("parse yaml config: {err}"))?;
    if let Some(root) = value.as_mapping_mut() {
        if server {
            set_yaml_secret(root, "admin", "admin_token");
            set_yaml_admin_user_tokens(root);
            set_yaml_secret(root, "acme", "dns_api_token");
        } else {
            set_yaml_secret(root, "client", "token");
        }
    }
    serde_yaml::to_string(&value).map_err(|err| format!("serialize yaml config: {err}"))
}

fn set_yaml_secret(root: &mut Mapping, section: &str, key: &str) {
    let Some(section) = root
        .get_mut(YamlValue::String(section.to_string()))
        .and_then(YamlValue::as_mapping_mut)
    else {
        return;
    };
    if let Some(item) = section.get_mut(YamlValue::String(key.to_string()))
        && !item.as_str().is_some_and(is_env_placeholder)
    {
        *item = YamlValue::String(SECRET_PLACEHOLDER.to_string());
    }
}

fn replace_admin_user_tokens(table: &mut toml_edit::Table, replacement: &str) {
    if let Some(users) = table
        .get_mut("admin")
        .and_then(|item| item.as_table_mut())
        .and_then(|admin| admin.get_mut("users"))
        .and_then(|item| item.as_array_of_tables_mut())
    {
        for user in users.iter_mut() {
            if let Some(item) = user.get_mut("token")
                && !item.as_str().is_some_and(is_env_placeholder)
            {
                *item = value(replacement);
            }
        }
    }
}

fn apply_admin_user_tokens(table: &mut toml_edit::Table, secrets: &[(String, String)]) {
    if let Some(users) = table
        .get_mut("admin")
        .and_then(|item| item.as_table_mut())
        .and_then(|admin| admin.get_mut("users"))
        .and_then(|item| item.as_array_of_tables_mut())
    {
        for user in users.iter_mut() {
            let Some(name) = user.get("name").and_then(toml_edit::Item::as_str) else {
                continue;
            };
            let Some(secret) = secrets
                .iter()
                .find(|(user, _)| user == name)
                .map(|(_, token)| token)
            else {
                continue;
            };
            if let Some(item) = user.get_mut("token")
                && item
                    .as_str()
                    .is_some_and(|value| value == SECRET_PLACEHOLDER)
            {
                *item = value(secret);
            }
        }
    }
}

fn ensure_no_user_token_placeholder(raw: &str, format: ConfigFormat) -> Result<(), String> {
    let config = crate::config::parse_server_config_unchecked_with_format(raw, format)
        .map_err(|error| format!("parse restored server config: {error}"))?;
    if config
        .admin
        .users
        .iter()
        .any(|user| user.token.as_deref() == Some(SECRET_PLACEHOLDER))
    {
        return Err("备份中的 admin.users token 未替换；请使用当前配置文件执行恢复".to_string());
    }
    Ok(())
}

fn set_yaml_admin_user_tokens(root: &mut Mapping) {
    let Some(users) = root
        .get_mut(YamlValue::String("admin".to_string()))
        .and_then(YamlValue::as_mapping_mut)
        .and_then(|admin| admin.get_mut(YamlValue::String("users".to_string())))
        .and_then(YamlValue::as_sequence_mut)
    else {
        return;
    };
    for user in users.iter_mut() {
        if let Some(user) = user.as_mapping_mut()
            && let Some(item) = user.get_mut(YamlValue::String("token".to_string()))
            && !item.as_str().is_some_and(is_env_placeholder)
        {
            *item = YamlValue::String(SECRET_PLACEHOLDER.to_string());
        }
    }
}

fn apply_yaml_admin_user_tokens(root: &mut Mapping, secrets: &[(String, String)]) {
    let Some(users) = root
        .get_mut(YamlValue::String("admin".to_string()))
        .and_then(YamlValue::as_mapping_mut)
        .and_then(|admin| admin.get_mut(YamlValue::String("users".to_string())))
        .and_then(YamlValue::as_sequence_mut)
    else {
        return;
    };
    for user in users.iter_mut() {
        let Some(user) = user.as_mapping_mut() else {
            continue;
        };
        let Some(name) = user
            .get(YamlValue::String("name".to_string()))
            .and_then(YamlValue::as_str)
        else {
            continue;
        };
        let Some(secret) = secrets
            .iter()
            .find(|(user, _)| user == name)
            .map(|(_, token)| token)
        else {
            continue;
        };
        if let Some(item) = user.get_mut(YamlValue::String("token".to_string()))
            && item
                .as_str()
                .is_some_and(|value| value == SECRET_PLACEHOLDER)
        {
            *item = YamlValue::String(secret.clone());
        }
    }
}

fn is_env_placeholder(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("${") && value.ends_with('}')
}

fn restore_yaml(
    raw: &str,
    secrets: &BackupSecrets,
    server_keys: &[&str],
) -> Result<String, String> {
    let mut value: YamlValue =
        serde_yaml::from_str(raw).map_err(|err| format!("parse yaml backup config: {err}"))?;
    if let Some(root) = value.as_mapping_mut() {
        if server_keys.contains(&"admin_token") {
            apply_yaml_secret(root, "admin", "admin_token", secrets.admin_token.as_deref());
            apply_yaml_admin_user_tokens(root, &secrets.admin_user_tokens);
            apply_yaml_secret(
                root,
                "acme",
                "dns_api_token",
                secrets.dns_api_token.as_deref(),
            );
        } else {
            apply_yaml_secret(root, "client", "token", secrets.client_token.as_deref());
        }
    }
    let rendered =
        serde_yaml::to_string(&value).map_err(|err| format!("serialize yaml config: {err}"))?;
    if server_keys.contains(&"admin_token") {
        ensure_no_user_token_placeholder(&rendered, ConfigFormat::Yaml)?;
    }
    Ok(rendered)
}

fn apply_yaml_secret(root: &mut Mapping, section: &str, key: &str, secret: Option<&str>) {
    let Some(secret) = secret else {
        return;
    };
    let Some(section) = root
        .get_mut(YamlValue::String(section.to_string()))
        .and_then(YamlValue::as_mapping_mut)
    else {
        return;
    };
    if let Some(item) = section.get_mut(YamlValue::String(key.to_string()))
        && item
            .as_str()
            .is_some_and(|value| value == SECRET_PLACEHOLDER)
    {
        *item = YamlValue::String(secret.to_string());
    }
}

fn push_ref(refs: &mut Vec<CertificateReference>, role: &str, path: Option<&Path>) {
    if let Some(path) = path {
        refs.push(CertificateReference {
            role: role.to_string(),
            path: path.display().to_string(),
        });
    }
}

fn checksum(document: &BackupDocument) -> String {
    let certificates = document
        .certificates
        .iter()
        .map(|entry| format!("{}:{}", entry.role, entry.path))
        .collect::<Vec<_>>()
        .join("|");
    sha256_hex(
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}",
            document.schema_version,
            document.meatus_version,
            document.created_at_ms,
            document.kind,
            document.format,
            document.config_sha256,
            document.config
        )
        .as_bytes(),
    )
    .to_string()
        + &sha256_hex(certificates.as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn kind_label(kind: ConfigKind) -> &'static str {
    match kind {
        ConfigKind::Server => "server",
        ConfigKind::Client => "client",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = r#"
[server]
bind_addr = "127.0.0.1"
control_port = 7000
tls_cert = "server.crt"
tls_key = "server.key"

[auth]
clients = [
  { id = "nas", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" },
]

[admin]
bind = "127.0.0.1:7400"
admin_token = "admin-secret"

[[admin.users]]
name = "alice"
role = "operator"
token = "alice-secret"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
dns_provider = "cloudflare"
dns_api_token = "cf-secret"
"#;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meatus-backup-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn backup_redacts_secrets_and_roundtrips() {
        let dir = temp("roundtrip");
        let path = dir.join("server.toml");
        fs::write(&path, SERVER).expect("write");
        let backup = create_backup(&path, SERVER, env!("CARGO_PKG_VERSION")).expect("backup");
        assert!(!backup.config.contains("admin-secret"));
        assert!(!backup.config.contains("cf-secret"));
        assert!(!backup.config.contains("alice-secret"));
        assert!(backup.config.contains("token_sha256"));
        assert_eq!(
            backup.certificates.len(),
            3,
            "cert/key paths and ACME dir only"
        );

        let json = serde_json::to_string(&backup).expect("json");
        let parsed = serde_json::from_str::<BackupDocument>(&json).expect("parse");
        let outcome = restore_backup(
            &parsed,
            &path,
            &BackupSecrets {
                admin_token: Some("admin-secret".to_string()),
                dns_api_token: Some("cf-secret".to_string()),
                client_token: None,
                admin_user_tokens: vec![("alice".to_string(), "alice-secret".to_string())],
            },
        )
        .expect("restore");
        assert!(outcome.backup_path.is_some());
        let restored = fs::read_to_string(&path).expect("read");
        assert!(restored.contains("admin-secret"));
        assert!(restored.contains("cf-secret"));
        assert!(restored.contains("alice-secret"));
    }

    #[test]
    fn yaml_backup_redacts_and_restores_admin_user_tokens() {
        let dir = temp("yaml-users");
        let path = dir.join("server.yaml");
        let raw = r#"
server:
  bind_addr: "127.0.0.1"
  control_port: 7000
  tls_cert: "server.crt"
  tls_key: "server.key"
auth:
  clients:
    - id: nas
      tenant: default
      token_sha256: "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
admin:
  bind: "127.0.0.1:7400"
  admin_token: "admin-secret"
  users:
    - name: alice
      role: operator
      token: "alice-secret"
acme:
  enabled: true
  email: "ops@example.com"
  challenge: dns-01
  dns_provider: cloudflare
  dns_api_token: "cf-secret"
"#;
        fs::write(&path, raw).expect("write");
        let backup = create_backup(&path, raw, env!("CARGO_PKG_VERSION")).expect("backup");
        assert!(!backup.config.contains("admin-secret"));
        assert!(!backup.config.contains("alice-secret"));
        let secrets = BackupSecrets {
            admin_token: Some("admin-secret".to_string()),
            dns_api_token: Some("cf-secret".to_string()),
            client_token: None,
            admin_user_tokens: vec![("alice".to_string(), "alice-secret".to_string())],
        };
        let outcome = restore_backup(&backup, &path, &secrets).expect("restore");
        assert!(outcome.backup_path.is_some());
        let restored = fs::read_to_string(&path).expect("read");
        assert!(restored.contains("admin-secret"));
        assert!(restored.contains("alice-secret"));
    }

    #[test]
    fn secrets_are_recovered_from_semantically_broken_config() {
        let broken = SERVER.replace("control_port = 7000", "control_port = 1");
        let secrets = secrets_from_raw(Path::new("server.toml"), &broken);
        assert_eq!(secrets.admin_token.as_deref(), Some("admin-secret"));
        assert_eq!(secrets.dns_api_token.as_deref(), Some("cf-secret"));
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let dir = temp("checksum");
        let path = dir.join("server.toml");
        fs::write(&path, SERVER).expect("write");
        let mut backup = create_backup(&path, SERVER, env!("CARGO_PKG_VERSION")).expect("backup");
        backup.config.push_str("\n# tampered");
        let error = restore_backup(&backup, &path, &BackupSecrets::default()).expect_err("reject");
        assert!(error.contains("校验和"));
    }

    #[test]
    fn invalid_restore_keeps_original() {
        let dir = temp("invalid");
        let path = dir.join("server.toml");
        fs::write(&path, SERVER).expect("write");
        let backup = create_backup(&path, SERVER, env!("CARGO_PKG_VERSION")).expect("backup");
        let mut tampered = backup.clone();
        tampered.config = "[server]\ncontrol_port = 1\n".to_string();
        tampered.config_sha256 = sha256_hex(tampered.config.as_bytes());
        tampered.checksum = checksum(&tampered);
        assert!(restore_backup(&tampered, &path, &BackupSecrets::default()).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), SERVER);
    }
}
