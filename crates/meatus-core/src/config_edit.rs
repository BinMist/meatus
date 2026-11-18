//! 配置文件回写：保留注释与未编辑字段，原子替换目标文件。
//!
//! 只负责“把已经通过校验的新值写回 TOML”，不负责重启或 reload。

use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, Document, InlineTable, Item, Table, Value, value};

use crate::config::{ClientConfig, ProxySpec, ProxyType, ServerConfig};

/// 写回客户端 proxy：同名替换，不存在则追加。保持其余配置和注释不变。
pub fn upsert_client_proxy(path: &Path, spec: &ProxySpec) -> Result<(), String> {
    let raw = read_config(path)?;
    let mut document = raw
        .parse::<Document>()
        .map_err(|err| format!("parse client config: {err}"))?;

    ensure_array_of_tables(&mut document, "proxy")?;
    let array = document["proxy"]
        .as_array_of_tables_mut()
        .ok_or_else(|| "client proxy must be an array of tables".to_string())?;

    let mut replaced = false;
    for table in array.iter_mut() {
        if table.get("name").and_then(Item::as_str) == Some(spec.name.as_str()) {
            *table = proxy_table(spec);
            replaced = true;
            break;
        }
    }
    if !replaced {
        array.push(proxy_table(spec));
    }

    validate_client_text(&document.to_string())?;
    write_atomic(path, &document.to_string())
}

/// 从客户端配置移除指定 proxy；不存在时视为成功。
pub fn remove_client_proxy(path: &Path, name: &str) -> Result<(), String> {
    let raw = read_config(path)?;
    let mut document = raw
        .parse::<Document>()
        .map_err(|err| format!("parse client config: {err}"))?;

    let mut empty = false;
    if let Some(array) = document
        .get_mut("proxy")
        .and_then(Item::as_array_of_tables_mut)
    {
        array.retain(|table| table.get("name").and_then(Item::as_str) != Some(name));
        empty = array.is_empty();
    }
    if empty {
        document.as_table_mut().remove("proxy");
    }

    validate_client_text(&document.to_string())?;
    write_atomic(path, &document.to_string())
}

/// 将 server 的可编辑字段写回 TOML；成功时返回备份文件路径。
pub fn write_server_config(path: &Path, next: &ServerConfig) -> Result<Option<PathBuf>, String> {
    let raw = read_config(path)?;
    let mut document = raw
        .parse::<Document>()
        .map_err(|err| format!("parse server config: {err}"))?;

    set_string(&mut document, "server", "bind_addr", &next.server.bind_addr)?;
    set_integer(
        &mut document,
        "server",
        "control_port",
        i64::from(next.server.control_port),
    )?;
    set_string(&mut document, "tenant", "default", &next.tenant.default)?;
    set_integer_array(
        &mut document,
        "ports",
        "range",
        &[
            i64::from(next.ports.range[0]),
            i64::from(next.ports.range[1]),
        ],
    )?;

    set_bool(&mut document, "http", "enabled", next.http.enabled)?;
    set_string(&mut document, "http", "bind", &next.http.bind)?;
    set_string(&mut document, "http", "https_bind", &next.http.https_bind)?;
    set_string(&mut document, "http", "domain", &next.http.domain)?;

    set_bool(&mut document, "acme", "enabled", next.acme.enabled)?;
    set_string(
        &mut document,
        "acme",
        "email",
        next.acme.email.as_deref().unwrap_or_default(),
    )?;
    set_bool(&mut document, "acme", "staging", next.acme.staging)?;

    set_bool(&mut document, "socks5", "enabled", next.socks5.enabled)?;
    set_string(&mut document, "socks5", "bind", &next.socks5.bind)?;

    set_string(&mut document, "log", "level", &next.log.level)?;
    set_string(
        &mut document,
        "log",
        "format",
        match next.log.format {
            crate::config::LogFormat::Text => "text",
            crate::config::LogFormat::Json => "json",
        },
    )?;

    validate_server_text(&document.to_string())?;

    let backup = backup_path(path);
    let had_original = path.exists();
    if had_original {
        fs::copy(path, &backup).map_err(|err| format!("backup {}: {err}", path.display()))?;
    }
    write_atomic(path, &document.to_string())?;
    Ok(had_original.then_some(backup))
}

fn proxy_table(spec: &ProxySpec) -> Table {
    let mut table = Table::new();
    table["name"] = value(spec.name.as_str());
    table["type"] = value(proxy_kind(spec.kind));
    table["local_addr"] = value(spec.local_addr.as_str());

    if let Some(port) = spec.remote_port {
        table["remote_port"] = value(i64::from(port));
    }

    if !spec.custom_domains.is_empty() {
        let mut domains = Array::new();
        for domain in &spec.custom_domains {
            domains.push(Value::from(domain.as_str()));
        }
        table["custom_domains"] = Item::Value(Value::Array(domains));
    }

    let options = &spec.options;
    if options.rate_limit_bps.is_some()
        || options.idle_timeout_s.is_some()
        || options.max_conns.is_some()
        || !options.allow_cidrs.is_empty()
    {
        let mut inline = InlineTable::new();
        if let Some(value) = options.rate_limit_bps {
            inline.insert("rate_limit_bps", Value::from(value as i64));
        }
        if let Some(value) = options.idle_timeout_s {
            inline.insert("idle_timeout_s", Value::from(i64::from(value)));
        }
        if let Some(value) = options.max_conns {
            inline.insert("max_conns", Value::from(i64::from(value)));
        }
        if !options.allow_cidrs.is_empty() {
            let mut cidrs = Array::new();
            for cidr in &options.allow_cidrs {
                cidrs.push(Value::from(cidr.as_str()));
            }
            inline.insert("allow_cidrs", Value::Array(cidrs));
        }
        table["options"] = Item::Value(Value::InlineTable(inline));
    }

    table
}

fn proxy_kind(kind: ProxyType) -> &'static str {
    match kind {
        ProxyType::Tcp => "tcp",
        ProxyType::Udp => "udp",
        ProxyType::Http => "http",
        ProxyType::Https => "https",
        ProxyType::Socks5 => "socks5",
    }
}

fn ensure_array_of_tables(document: &mut Document, key: &str) -> Result<(), String> {
    if !document.as_table().contains_key(key) {
        document
            .as_table_mut()
            .insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    if document[key].as_array_of_tables().is_none() {
        return Err(format!("{key} must be an array of tables"));
    }
    Ok(())
}

fn table_mut<'a>(document: &'a mut Document, name: &str) -> Result<&'a mut Table, String> {
    if !document.as_table().contains_key(name) {
        document
            .as_table_mut()
            .insert(name, Item::Table(Table::new()));
    }
    document[name]
        .as_table_mut()
        .ok_or_else(|| format!("{name} must be a table"))
}

fn set_string(document: &mut Document, table: &str, key: &str, value: &str) -> Result<(), String> {
    table_mut(document, table)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_bool(document: &mut Document, table: &str, key: &str, value: bool) -> Result<(), String> {
    table_mut(document, table)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_integer(document: &mut Document, table: &str, key: &str, value: i64) -> Result<(), String> {
    table_mut(document, table)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_integer_array(
    document: &mut Document,
    table: &str,
    key: &str,
    values: &[i64],
) -> Result<(), String> {
    let mut array = Array::new();
    for value in values {
        array.push(Value::from(*value));
    }
    table_mut(document, table)?[key] = Item::Value(Value::Array(array));
    Ok(())
}

fn read_config(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))
}

fn validate_client_text(text: &str) -> Result<(), String> {
    let config: ClientConfig =
        toml::from_str(text).map_err(|err| format!("invalid client TOML: {err}"))?;
    config
        .validate()
        .map_err(|err| format!("invalid client config: {err}"))
}

fn validate_server_text(text: &str) -> Result<(), String> {
    let config: ServerConfig =
        toml::from_str(text).map_err(|err| format!("invalid server TOML: {err}"))?;
    config
        .validate()
        .map_err(|err| format!("invalid server config: {err}"))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "server.toml".into(), |name| name.to_os_string());
    name.push(".bak");
    path.with_file_name(name)
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    let permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
    if let Some(permissions) = &permissions {
        let _ = fs::set_permissions(&tmp, permissions.clone());
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) if matches!(err.raw_os_error(), Some(16 | 18)) => {
            // 单文件 bind mount 的挂载点不能 rename 覆盖（EBUSY / EXDEV），
            // 退化为截断写回；调用方已先写备份，异常时仍可人工恢复。
            let bytes = fs::read(&tmp).map_err(|read_err| {
                let _ = fs::remove_file(&tmp);
                format!("read {}: {read_err}", tmp.display())
            })?;
            let result = (|| -> std::io::Result<()> {
                use std::io::Write as _;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                if let Some(permissions) = permissions {
                    fs::set_permissions(path, permissions)?;
                }
                Ok(())
            })();
            let _ = fs::remove_file(&tmp);
            result.map_err(|write_err| format!("write {}: {write_err}", path.display()))
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("replace {}: {err}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyOptions;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |value| value.as_nanos());
        std::env::temp_dir().join(format!(
            "meatus-config-edit-{}-{stamp}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn upsert_and_remove_client_proxy_preserves_comments() {
        let path = temp_path("client.toml");
        let original = r#"
# client level comment
[client]
server_addr = "tunnel.example.com:7000"
token = "secret"

# keep this proxy
[[proxy]]
name = "old"
type = "tcp"
local_addr = "127.0.0.1:8080"
remote_port = 21000
"#;
        fs::write(&path, original).expect("write original");

        let spec = ProxySpec {
            name: "new".to_string(),
            kind: ProxyType::Http,
            local_addr: "127.0.0.1:3000".to_string(),
            remote_port: Some(21001),
            custom_domains: vec!["app.example.com".to_string()],
            options: ProxyOptions {
                rate_limit_bps: Some(1_000_000),
                idle_timeout_s: Some(30),
                max_conns: Some(4),
                allow_cidrs: vec!["10.0.0.0/8".to_string()],
            },
        };
        upsert_client_proxy(&path, &spec).expect("upsert");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("# client level comment"));
        assert!(raw.contains("# keep this proxy"));
        assert!(raw.contains("name = \"new\""));
        assert!(raw.contains("custom_domains = [\"app.example.com\"]"));
        let parsed: ClientConfig = toml::from_str(&raw).expect("parse");
        assert_eq!(parsed.proxies.len(), 2);

        remove_client_proxy(&path, "old").expect("remove");
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: ClientConfig = toml::from_str(&raw).expect("parse");
        assert_eq!(parsed.proxies.len(), 1);
        assert_eq!(parsed.proxies[0].name, "new");
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod server_tests {
    use super::*;

    #[test]
    fn server_write_preserves_comments_and_updates_editable_fields() {
        let path =
            std::env::temp_dir().join(format!("meatus-server-edit-{}.toml", std::process::id()));
        let original = include_str!("../../../config/server.example.toml");
        fs::write(&path, original).expect("write original");
        let mut next: ServerConfig = toml::from_str(original).expect("parse example");
        next.http.enabled = false;
        next.http.domain = "panel.example.com".to_string();
        next.log.level = "warn".to_string();
        next.log.format = crate::config::LogFormat::Json;

        write_server_config(&path, &next).expect("write server config");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("# TLS 1.3"));
        assert!(raw.contains("domain = \"panel.example.com\""));
        assert!(raw.contains("level = \"warn\""));
        assert!(raw.contains("format = \"json\""));
        let parsed: ServerConfig = toml::from_str(&raw).expect("parse result");
        assert_eq!(parsed.log.level, "warn");
        assert_eq!(parsed.log.format, crate::config::LogFormat::Json);
        let _ = fs::remove_file(path);
    }
}
