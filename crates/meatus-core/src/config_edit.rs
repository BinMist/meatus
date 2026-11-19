//! 配置文件回写：保留未编辑字段，原子替换目标文件。
//!
//! TOML 尽量保留注释与键顺序；YAML 会规范化重写，注释和键顺序不保证保留。
//! 只负责“把已经通过校验的新值写回配置文件”，不负责重启或 reload。

use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Number, Sequence, Value as YamlValue};
use toml_edit::{Array, ArrayOfTables, Document, InlineTable, Item, Table, Value, value};

use crate::config::{
    ClientConfig, ClientEntry, ClientLimits, ConfigFormat, ProxySpec, ProxyType, QuotaAction,
    ServerConfig,
};

/// 写回客户端 proxy：同名替换，不存在则追加。
///
/// TOML 保持其余配置和注释不变；YAML 规范化重写。
pub fn upsert_client_proxy(path: &Path, spec: &ProxySpec) -> Result<(), String> {
    if ConfigFormat::from_path(path) == ConfigFormat::Yaml {
        return upsert_client_proxy_yaml(path, spec);
    }
    upsert_client_proxy_toml(path, spec)
}

fn upsert_client_proxy_toml(path: &Path, spec: &ProxySpec) -> Result<(), String> {
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
///
/// TOML 保持其余配置和注释不变；YAML 规范化重写。
pub fn remove_client_proxy(path: &Path, name: &str) -> Result<(), String> {
    if ConfigFormat::from_path(path) == ConfigFormat::Yaml {
        return remove_client_proxy_yaml(path, name);
    }
    remove_client_proxy_toml(path, name)
}

fn remove_client_proxy_toml(path: &Path, name: &str) -> Result<(), String> {
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

/// 将 server 的可编辑字段写回 TOML / YAML；成功时返回备份文件路径。
///
/// TOML 保留注释与键顺序；YAML 规范化重写。
pub fn write_server_config(path: &Path, next: &ServerConfig) -> Result<Option<PathBuf>, String> {
    if ConfigFormat::from_path(path) == ConfigFormat::Yaml {
        return write_server_config_yaml(path, next);
    }
    write_server_config_toml(path, next)
}

fn write_server_config_toml(path: &Path, next: &ServerConfig) -> Result<Option<PathBuf>, String> {
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
    set_string(
        &mut document,
        "ports",
        "release_policy",
        match next.ports.release_policy {
            crate::config::PortReleasePolicy::Immediate => "immediate",
            crate::config::PortReleasePolicy::Hold => "hold",
        },
    )?;
    set_integer(
        &mut document,
        "ports",
        "hold_secs",
        i64::try_from(next.ports.hold_secs).map_err(|_| "ports.hold_secs is too large")?,
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

fn write_server_config_yaml(path: &Path, next: &ServerConfig) -> Result<Option<PathBuf>, String> {
    let mut root = read_yaml_config(path, "server")?;

    yaml_set_string(&mut root, "server", "bind_addr", &next.server.bind_addr)?;
    yaml_set_u64(
        &mut root,
        "server",
        "control_port",
        u64::from(next.server.control_port),
    )?;
    yaml_set_string(&mut root, "tenant", "default", &next.tenant.default)?;
    yaml_set_u64_array(
        &mut root,
        "ports",
        "range",
        &[
            u64::from(next.ports.range[0]),
            u64::from(next.ports.range[1]),
        ],
    )?;
    yaml_set_string(
        &mut root,
        "ports",
        "release_policy",
        match next.ports.release_policy {
            crate::config::PortReleasePolicy::Immediate => "immediate",
            crate::config::PortReleasePolicy::Hold => "hold",
        },
    )?;
    yaml_set_u64(&mut root, "ports", "hold_secs", next.ports.hold_secs)?;

    yaml_set_bool(&mut root, "http", "enabled", next.http.enabled)?;
    yaml_set_string(&mut root, "http", "bind", &next.http.bind)?;
    yaml_set_string(&mut root, "http", "https_bind", &next.http.https_bind)?;
    yaml_set_string(&mut root, "http", "domain", &next.http.domain)?;

    yaml_set_bool(&mut root, "acme", "enabled", next.acme.enabled)?;
    yaml_set_string(
        &mut root,
        "acme",
        "email",
        next.acme.email.as_deref().unwrap_or_default(),
    )?;
    yaml_set_bool(&mut root, "acme", "staging", next.acme.staging)?;

    yaml_set_bool(&mut root, "socks5", "enabled", next.socks5.enabled)?;
    yaml_set_string(&mut root, "socks5", "bind", &next.socks5.bind)?;

    yaml_set_string(&mut root, "log", "level", &next.log.level)?;
    yaml_set_string(
        &mut root,
        "log",
        "format",
        match next.log.format {
            crate::config::LogFormat::Text => "text",
            crate::config::LogFormat::Json => "json",
        },
    )?;

    let text =
        serde_yaml::to_string(&root).map_err(|err| format!("serialize server YAML: {err}"))?;
    validate_server_yaml_text(&text)?;
    backup_and_write(path, &text)
}

/// 写入或替换一个 `auth.clients` 条目。
///
/// 如果配置仍在使用 `auth.token_sha256` 单客户端简写，会先把它迁移成
/// `id=default, tenant=default` 的条目，再写入新客户端。成功时返回备份路径。
pub fn upsert_server_client(path: &Path, entry: &ClientEntry) -> Result<Option<PathBuf>, String> {
    if ConfigFormat::from_path(path) == ConfigFormat::Yaml {
        return upsert_server_client_yaml(path, entry);
    }
    upsert_server_client_toml(path, entry)
}

/// 从 `auth.clients` 删除一个条目；不存在时视为成功。
///
/// 删除后如果配置不再有任何凭据，校验会失败，调用方应先做兜底检查。
pub fn remove_server_client(path: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    if ConfigFormat::from_path(path) == ConfigFormat::Yaml {
        return remove_server_client_yaml(path, id);
    }
    remove_server_client_toml(path, id)
}

fn upsert_server_client_toml(path: &Path, entry: &ClientEntry) -> Result<Option<PathBuf>, String> {
    let raw = read_config(path)?;
    let mut document = raw
        .parse::<Document>()
        .map_err(|err| format!("parse server config: {err}"))?;

    if !document.as_table().contains_key("auth") {
        document
            .as_table_mut()
            .insert("auth", Item::Table(Table::new()));
    }
    let auth = document["auth"]
        .as_table_mut()
        .ok_or_else(|| "server auth must be a table".to_string())?;

    ensure_client_array(auth)?;
    let array = auth
        .get_mut("clients")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "server auth.clients must be an array of tables".to_string())?;
    let mut replaced = false;
    for table in array.iter_mut() {
        if table.get("id").and_then(Item::as_str) == Some(entry.id.as_str()) {
            *table = client_table(entry);
            replaced = true;
            break;
        }
    }
    if !replaced {
        array.push(client_table(entry));
    }

    validate_server_text(&document.to_string())?;
    backup_and_write(path, &document.to_string())
}

fn remove_server_client_toml(path: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    let raw = read_config(path)?;
    let mut document = raw
        .parse::<Document>()
        .map_err(|err| format!("parse server config: {err}"))?;

    let mut empty = false;
    if let Some(auth) = document.get_mut("auth").and_then(Item::as_table_mut) {
        ensure_client_array(auth)?;
        if let Some(array) = auth
            .get_mut("clients")
            .and_then(Item::as_array_of_tables_mut)
        {
            array.retain(|table| table.get("id").and_then(Item::as_str) != Some(id));
            empty = array.is_empty();
        }
    }
    if empty && let Some(auth) = document.get_mut("auth").and_then(Item::as_table_mut) {
        auth.remove("clients");
    }

    validate_server_text(&document.to_string())?;
    backup_and_write(path, &document.to_string())
}

fn upsert_server_client_yaml(path: &Path, entry: &ClientEntry) -> Result<Option<PathBuf>, String> {
    let mut root = read_yaml_config(path, "server")?;
    {
        let auth = yaml_map_mut(&mut root, "auth")?;
        if !auth.contains_key(yaml_key("clients")) {
            let mut array = Sequence::new();
            if let Some(token) = auth
                .get(yaml_key("token_sha256"))
                .and_then(YamlValue::as_str)
            {
                array.push(client_yaml_value(&ClientEntry {
                    id: crate::tenant::DEFAULT_TENANT.to_string(),
                    tenant: crate::tenant::DEFAULT_TENANT.to_string(),
                    token_sha256: Some(token.to_string()),
                    allow_ports: None,
                    allow_types: Vec::new(),
                    allow_domains: Vec::new(),
                    max_proxies: None,
                    limits: ClientLimits::default(),
                }));
            }
            auth.insert(yaml_key("clients"), YamlValue::Sequence(array));
            auth.remove(yaml_key("token_sha256"));
        }

        let array = auth
            .get_mut(yaml_key("clients"))
            .and_then(YamlValue::as_sequence_mut)
            .ok_or_else(|| "server auth.clients must be a sequence".to_string())?;
        let mut replaced = false;
        for item in array.iter_mut() {
            if item
                .as_mapping()
                .and_then(|table| table.get(yaml_key("id")))
                .and_then(YamlValue::as_str)
                == Some(entry.id.as_str())
            {
                *item = client_yaml_value(entry);
                replaced = true;
                break;
            }
        }
        if !replaced {
            array.push(client_yaml_value(entry));
        }
    }

    let text =
        serde_yaml::to_string(&root).map_err(|err| format!("serialize server YAML: {err}"))?;
    validate_server_yaml_text(&text)?;
    backup_and_write(path, &text)
}

fn remove_server_client_yaml(path: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    let mut root = read_yaml_config(path, "server")?;
    let empty = {
        let auth = yaml_map_mut(&mut root, "auth")?;
        if let Some(array) = auth
            .get_mut(yaml_key("clients"))
            .and_then(YamlValue::as_sequence_mut)
        {
            array.retain(|item| {
                item.as_mapping()
                    .and_then(|table| table.get(yaml_key("id")))
                    .and_then(YamlValue::as_str)
                    != Some(id)
            });
            array.is_empty()
        } else {
            false
        }
    };
    if empty {
        yaml_map_mut(&mut root, "auth")?.remove(yaml_key("clients"));
    }

    let text =
        serde_yaml::to_string(&root).map_err(|err| format!("serialize server YAML: {err}"))?;
    validate_server_yaml_text(&text)?;
    backup_and_write(path, &text)
}

fn ensure_client_array(auth: &mut Table) -> Result<(), String> {
    if !auth.contains_key("clients") {
        let mut array = ArrayOfTables::new();
        if let Some(token) = auth.get("token_sha256").and_then(Item::as_str) {
            array.push(default_client_table(token));
        }
        auth.insert("clients", Item::ArrayOfTables(array));
        auth.remove("token_sha256");
        return Ok(());
    }
    let item = auth
        .get_mut("clients")
        .ok_or_else(|| "server auth.clients is missing".to_string())?;
    if item.as_array_of_tables().is_some() {
        auth.remove("token_sha256");
        return Ok(());
    }
    let Some(values) = item.as_value().and_then(Value::as_array) else {
        return Err("server auth.clients must be an array of tables".to_string());
    };
    let mut array = ArrayOfTables::new();
    for value in values {
        let Value::InlineTable(inline) = value else {
            return Err("server auth.clients array entries must be inline tables".to_string());
        };
        let mut table = Table::new();
        for (key, value) in inline.iter() {
            table.insert(key, Item::Value(value.clone()));
        }
        array.push(table);
    }
    *item = Item::ArrayOfTables(array);
    auth.remove("token_sha256");
    Ok(())
}

fn proxy_type_name(kind: crate::config::ProxyType) -> &'static str {
    match kind {
        crate::config::ProxyType::Tcp => "tcp",
        crate::config::ProxyType::Udp => "udp",
        crate::config::ProxyType::Http => "http",
        crate::config::ProxyType::Https => "https",
        crate::config::ProxyType::Socks5 => "socks5",
    }
}

fn quota_action_name(action: QuotaAction) -> &'static str {
    match action {
        QuotaAction::Reject => "reject",
        QuotaAction::Throttle => "throttle",
    }
}

fn default_client_table(token_sha256: &str) -> Table {
    let mut table = Table::new();
    table.insert("id", value(crate::tenant::DEFAULT_TENANT));
    table.insert("tenant", value(crate::tenant::DEFAULT_TENANT));
    table.insert("token_sha256", value(token_sha256));
    table
}

fn client_table(entry: &ClientEntry) -> Table {
    let mut table = Table::new();
    table.insert("id", value(entry.id.as_str()));
    table.insert("tenant", value(entry.tenant.as_str()));
    if let Some(token) = entry.token_sha256.as_deref() {
        table.insert("token_sha256", value(token));
    }
    if let Some([start, end]) = entry.allow_ports {
        let mut ports = Array::new();
        ports.push(Value::from(i64::from(start)));
        ports.push(Value::from(i64::from(end)));
        table.insert("allow_ports", Item::Value(Value::Array(ports)));
    }
    if !entry.allow_types.is_empty() {
        let mut types = Array::new();
        for kind in &entry.allow_types {
            types.push(Value::from(proxy_type_name(*kind)));
        }
        table.insert("allow_types", Item::Value(Value::Array(types)));
    }
    if !entry.allow_domains.is_empty() {
        let mut domains = Array::new();
        for domain in &entry.allow_domains {
            domains.push(Value::from(domain.as_str()));
        }
        table.insert("allow_domains", Item::Value(Value::Array(domains)));
    }
    if let Some(max) = entry.max_proxies {
        table.insert("max_proxies", value(i64::from(max)));
    }
    if entry.limits != ClientLimits::default() {
        let mut limits = Table::new();
        if let Some(rate) = entry.limits.rate_limit_bps {
            limits.insert(
                "rate_limit_bps",
                value(i64::try_from(rate).unwrap_or(i64::MAX)),
            );
        }
        if let Some(bytes) = entry.limits.monthly_bytes {
            limits.insert(
                "monthly_bytes",
                value(i64::try_from(bytes).unwrap_or(i64::MAX)),
            );
        }
        if let Some(conns) = entry.limits.max_conns {
            limits.insert("max_conns", value(i64::from(conns)));
        }
        if let Some(rate) = entry.limits.conn_rate_per_s {
            limits.insert("conn_rate_per_s", value(i64::from(rate)));
        }
        if let Some(action) = entry.limits.quota_action {
            limits.insert("quota_action", value(quota_action_name(action)));
        }
        table.insert("limits", Item::Table(limits));
    }
    table
}

fn client_yaml_value(entry: &ClientEntry) -> YamlValue {
    let mut table = Mapping::new();
    table.insert(yaml_key("id"), YamlValue::String(entry.id.clone()));
    table.insert(yaml_key("tenant"), YamlValue::String(entry.tenant.clone()));
    if let Some(token) = entry.token_sha256.as_deref() {
        table.insert(
            yaml_key("token_sha256"),
            YamlValue::String(token.to_string()),
        );
    }
    if let Some([start, end]) = entry.allow_ports {
        let ports = Sequence::from(vec![
            YamlValue::Number(Number::from(u64::from(start))),
            YamlValue::Number(Number::from(u64::from(end))),
        ]);
        table.insert(yaml_key("allow_ports"), YamlValue::Sequence(ports));
    }
    if !entry.allow_types.is_empty() {
        let mut types = Sequence::new();
        for kind in &entry.allow_types {
            types.push(YamlValue::String(proxy_type_name(*kind).to_string()));
        }
        table.insert(yaml_key("allow_types"), YamlValue::Sequence(types));
    }
    if !entry.allow_domains.is_empty() {
        let mut domains = Sequence::new();
        for domain in &entry.allow_domains {
            domains.push(YamlValue::String(domain.clone()));
        }
        table.insert(yaml_key("allow_domains"), YamlValue::Sequence(domains));
    }
    if let Some(max) = entry.max_proxies {
        table.insert(
            yaml_key("max_proxies"),
            YamlValue::Number(Number::from(u64::from(max))),
        );
    }
    if entry.limits != ClientLimits::default() {
        let mut limits = Mapping::new();
        if let Some(rate) = entry.limits.rate_limit_bps {
            limits.insert(
                yaml_key("rate_limit_bps"),
                YamlValue::Number(Number::from(rate)),
            );
        }
        if let Some(bytes) = entry.limits.monthly_bytes {
            limits.insert(
                yaml_key("monthly_bytes"),
                YamlValue::Number(Number::from(bytes)),
            );
        }
        if let Some(conns) = entry.limits.max_conns {
            limits.insert(
                yaml_key("max_conns"),
                YamlValue::Number(Number::from(u64::from(conns))),
            );
        }
        if let Some(rate) = entry.limits.conn_rate_per_s {
            limits.insert(
                yaml_key("conn_rate_per_s"),
                YamlValue::Number(Number::from(u64::from(rate))),
            );
        }
        if let Some(action) = entry.limits.quota_action {
            limits.insert(
                yaml_key("quota_action"),
                YamlValue::String(quota_action_name(action).to_string()),
            );
        }
        table.insert(yaml_key("limits"), YamlValue::Mapping(limits));
    }
    YamlValue::Mapping(table)
}

fn upsert_client_proxy_yaml(path: &Path, spec: &ProxySpec) -> Result<(), String> {
    let mut root = read_yaml_config(path, "client")?;
    let array = yaml_sequence_mut(&mut root, "proxy")?;

    let mut replaced = false;
    for item in array.iter_mut() {
        if item
            .as_mapping()
            .and_then(|table| table.get(yaml_key("name")))
            .and_then(YamlValue::as_str)
            == Some(spec.name.as_str())
        {
            *item = proxy_yaml_value(spec);
            replaced = true;
            break;
        }
    }
    if !replaced {
        array.push(proxy_yaml_value(spec));
    }

    let text =
        serde_yaml::to_string(&root).map_err(|err| format!("serialize client YAML: {err}"))?;
    validate_client_yaml_text(&text)?;
    write_atomic(path, &text)
}

fn remove_client_proxy_yaml(path: &Path, name: &str) -> Result<(), String> {
    let mut root = read_yaml_config(path, "client")?;
    let empty = {
        let array = yaml_sequence_mut(&mut root, "proxy")?;
        array.retain(|item| {
            item.as_mapping()
                .and_then(|table| table.get(yaml_key("name")))
                .and_then(YamlValue::as_str)
                != Some(name)
        });
        array.is_empty()
    };
    if empty {
        root.as_mapping_mut()
            .ok_or_else(|| "client config root must be a mapping".to_string())?
            .remove(yaml_key("proxy"));
    }

    let text =
        serde_yaml::to_string(&root).map_err(|err| format!("serialize client YAML: {err}"))?;
    validate_client_yaml_text(&text)?;
    write_atomic(path, &text)
}

fn proxy_yaml_value(spec: &ProxySpec) -> YamlValue {
    let mut table = Mapping::new();
    table.insert(yaml_key("name"), YamlValue::String(spec.name.clone()));
    table.insert(
        yaml_key("type"),
        YamlValue::String(proxy_kind(spec.kind).to_string()),
    );
    table.insert(
        yaml_key("local_addr"),
        YamlValue::String(spec.local_addr.clone()),
    );

    if let Some(port) = spec.remote_port {
        table.insert(
            yaml_key("remote_port"),
            YamlValue::Number(Number::from(u64::from(port))),
        );
    }

    if !spec.custom_domains.is_empty() {
        let mut domains = Sequence::new();
        for domain in &spec.custom_domains {
            domains.push(YamlValue::String(domain.clone()));
        }
        table.insert(yaml_key("custom_domains"), YamlValue::Sequence(domains));
    }

    let options = &spec.options;
    if options.rate_limit_bps.is_some()
        || options.idle_timeout_s.is_some()
        || options.max_conns.is_some()
        || !options.allow_cidrs.is_empty()
        || !options.allow_domains.is_empty()
    {
        let mut option_table = Mapping::new();
        if let Some(value) = options.rate_limit_bps {
            option_table.insert(
                yaml_key("rate_limit_bps"),
                YamlValue::Number(Number::from(value)),
            );
        }
        if let Some(value) = options.idle_timeout_s {
            option_table.insert(
                yaml_key("idle_timeout_s"),
                YamlValue::Number(Number::from(u64::from(value))),
            );
        }
        if let Some(value) = options.max_conns {
            option_table.insert(
                yaml_key("max_conns"),
                YamlValue::Number(Number::from(u64::from(value))),
            );
        }
        if !options.allow_cidrs.is_empty() {
            let mut cidrs = Sequence::new();
            for cidr in &options.allow_cidrs {
                cidrs.push(YamlValue::String(cidr.clone()));
            }
            option_table.insert(yaml_key("allow_cidrs"), YamlValue::Sequence(cidrs));
        }
        if !options.allow_domains.is_empty() {
            let mut domains = Sequence::new();
            for domain in &options.allow_domains {
                domains.push(YamlValue::String(domain.clone()));
            }
            option_table.insert(yaml_key("allow_domains"), YamlValue::Sequence(domains));
        }
        table.insert(yaml_key("options"), YamlValue::Mapping(option_table));
    }

    YamlValue::Mapping(table)
}

fn read_yaml_config(path: &Path, kind: &str) -> Result<YamlValue, String> {
    let raw = read_config(path)?;
    let root: YamlValue =
        serde_yaml::from_str(&raw).map_err(|err| format!("parse {kind} config YAML: {err}"))?;
    if !root.is_mapping() {
        return Err(format!("{kind} config YAML root must be a mapping"));
    }
    Ok(root)
}

fn yaml_key(key: &str) -> YamlValue {
    YamlValue::String(key.to_string())
}

fn yaml_map_mut<'a>(root: &'a mut YamlValue, key: &str) -> Result<&'a mut Mapping, String> {
    let table = root
        .as_mapping_mut()
        .ok_or_else(|| "config root must be a mapping".to_string())?;
    let key = yaml_key(key);
    if !table.contains_key(&key) {
        table.insert(key.clone(), YamlValue::Mapping(Mapping::new()));
    }
    table
        .get_mut(&key)
        .and_then(YamlValue::as_mapping_mut)
        .ok_or_else(|| format!("{} must be a mapping", key.as_str().unwrap_or_default()))
}

fn yaml_sequence_mut<'a>(root: &'a mut YamlValue, key: &str) -> Result<&'a mut Sequence, String> {
    let table = root
        .as_mapping_mut()
        .ok_or_else(|| "config root must be a mapping".to_string())?;
    let key = yaml_key(key);
    if !table.contains_key(&key) {
        table.insert(key.clone(), YamlValue::Sequence(Sequence::new()));
    }
    table
        .get_mut(&key)
        .and_then(YamlValue::as_sequence_mut)
        .ok_or_else(|| format!("{} must be a sequence", key.as_str().unwrap_or_default()))
}

fn yaml_set_string(
    root: &mut YamlValue,
    table: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    yaml_map_mut(root, table)?.insert(yaml_key(key), YamlValue::String(value.to_string()));
    Ok(())
}

fn yaml_set_bool(root: &mut YamlValue, table: &str, key: &str, value: bool) -> Result<(), String> {
    yaml_map_mut(root, table)?.insert(yaml_key(key), YamlValue::Bool(value));
    Ok(())
}

fn yaml_set_u64(root: &mut YamlValue, table: &str, key: &str, value: u64) -> Result<(), String> {
    yaml_map_mut(root, table)?.insert(yaml_key(key), YamlValue::Number(Number::from(value)));
    Ok(())
}

fn yaml_set_u64_array(
    root: &mut YamlValue,
    table: &str,
    key: &str,
    values: &[u64],
) -> Result<(), String> {
    let mut array = Sequence::new();
    for value in values {
        array.push(YamlValue::Number(Number::from(*value)));
    }
    yaml_map_mut(root, table)?.insert(yaml_key(key), YamlValue::Sequence(array));
    Ok(())
}

fn backup_and_write(path: &Path, text: &str) -> Result<Option<PathBuf>, String> {
    let backup = backup_path(path);
    let had_original = path.exists();
    if had_original {
        fs::copy(path, &backup).map_err(|err| format!("backup {}: {err}", path.display()))?;
    }
    write_atomic(path, text)?;
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
        || !options.allow_domains.is_empty()
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
        if !options.allow_domains.is_empty() {
            let mut domains = Array::new();
            for domain in &options.allow_domains {
                domains.push(Value::from(domain.as_str()));
            }
            inline.insert("allow_domains", Value::Array(domains));
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

fn validate_client_yaml_text(text: &str) -> Result<(), String> {
    let config: ClientConfig =
        serde_yaml::from_str(text).map_err(|err| format!("invalid client YAML: {err}"))?;
    config
        .validate()
        .map_err(|err| format!("invalid client config: {err}"))
}

fn validate_server_yaml_text(text: &str) -> Result<(), String> {
    let config: ServerConfig =
        serde_yaml::from_str(text).map_err(|err| format!("invalid server YAML: {err}"))?;
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
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("tmp");
    let tmp = path.with_extension(format!("{extension}.tmp"));
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

    fn temp_path(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
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
                monthly_bytes: None,
                conn_rate_per_s: None,
                quota_action: None,
                allow_cidrs: vec!["10.0.0.0/8".to_string()],
                allow_domains: Vec::new(),
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

    #[test]
    fn upsert_and_remove_client_proxy_yaml_preserves_env_placeholder() {
        let path = temp_path("client.yaml");
        let original = r#"
client:
  server_addr: "tunnel.example.com:7000"
  server_name: "tunnel.example.com"
  client_id: "home-nas"
  tenant: "default"
  token: "${MEATUS_CLIENT_TOKEN}"
  server_ca_cert: "certs/server.crt"

proxy:
  - name: "old"
    type: "tcp"
    local_addr: "127.0.0.1:8080"
    remote_port: 21000
"#;
        fs::write(&path, original).expect("write original");

        let spec = ProxySpec {
            name: "new".to_string(),
            kind: ProxyType::Http,
            local_addr: "127.0.0.1:3000".to_string(),
            remote_port: None,
            custom_domains: vec!["app.example.com".to_string()],
            options: ProxyOptions {
                rate_limit_bps: Some(1_000_000),
                idle_timeout_s: Some(30),
                max_conns: Some(4),
                monthly_bytes: None,
                conn_rate_per_s: None,
                quota_action: None,
                allow_cidrs: vec!["10.0.0.0/8".to_string()],
                allow_domains: Vec::new(),
            },
        };
        upsert_client_proxy(&path, &spec).expect("upsert yaml");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("${MEATUS_CLIENT_TOKEN}"), "{raw}");
        let parsed: ClientConfig = serde_yaml::from_str(&raw).expect("parse written yaml");
        assert_eq!(parsed.proxies.len(), 2);
        let new = parsed
            .proxies
            .iter()
            .find(|proxy| proxy.name == "new")
            .expect("new proxy");
        assert_eq!(new.kind, ProxyType::Http);
        assert_eq!(new.custom_domains, vec!["app.example.com".to_string()]);

        remove_client_proxy(&path, "old").expect("remove yaml");
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: ClientConfig = serde_yaml::from_str(&raw).expect("parse written yaml");
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

    #[test]
    fn server_yaml_write_updates_fields_and_keeps_env_placeholder() {
        let path = std::env::temp_dir().join(format!(
            "meatus-server-yaml-edit-{}.yaml",
            std::process::id()
        ));
        let original = r#"
server:
  bind_addr: "127.0.0.1"
  control_port: 7000
  tls_cert: "certs/server.crt"
  tls_key: "certs/server.key"

tenant:
  default: "default"

auth:
  mode: "hmac"
  provider: "local"
  clients:
    - id: "home-nas"
      tenant: "default"
      token_sha256: "${MEATUS_TOKEN_SHA256}"

ports:
  range: [20000, 30000]

admin:
  bind: "127.0.0.1:7400"
  panel: true
  admin_token: "${MEATUS_ADMIN_TOKEN}"

log:
  level: "info"
  format: "text"
"#;
        fs::write(&path, original).expect("write original");
        let mut next: ServerConfig = serde_yaml::from_str(original).expect("parse example");
        next.http.enabled = false;
        next.http.domain = "panel.example.com".to_string();
        next.log.level = "warn".to_string();
        next.log.format = crate::config::LogFormat::Json;

        let backup = write_server_config(&path, &next).expect("write server yaml");
        assert!(backup.is_some());
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("${MEATUS_TOKEN_SHA256}"), "{raw}");
        assert!(raw.contains("${MEATUS_ADMIN_TOKEN}"), "{raw}");
        assert!(raw.contains("domain: panel.example.com"), "{raw}");
        assert!(raw.contains("level: warn"), "{raw}");
        assert!(raw.contains("format: json"), "{raw}");
        let parsed: ServerConfig = serde_yaml::from_str(&raw).expect("parse result");
        assert_eq!(parsed.log.level, "warn");
        assert_eq!(parsed.log.format, crate::config::LogFormat::Json);
        let _ = fs::remove_file(path);
        if let Some(backup) = backup {
            let _ = fs::remove_file(backup);
        }
    }
    #[test]
    fn upsert_server_client_migrates_inline_auth_clients() {
        let path = std::env::temp_dir().join(format!(
            "meatus-config-edit-client-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |value| value.as_nanos())
        ));
        let original = r#"
# server comment
[server]
bind_addr = "127.0.0.1"
control_port = 17000

[auth]
mode = "hmac"
clients = [
  { id = "old", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" },
]
"#;
        fs::write(&path, original).expect("write original");
        let entry = ClientEntry {
            id: "new".to_string(),
            tenant: "default".to_string(),
            token_sha256: Some(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            ),
            allow_ports: Some([20000, 21000]),
            allow_types: Vec::new(),
            allow_domains: Vec::new(),
            max_proxies: None,
            limits: ClientLimits::default(),
        };
        upsert_server_client(&path, &entry).expect("upsert");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("# server comment"));
        let parsed = crate::config::parse_server_config(&raw).expect("parse");
        assert_eq!(parsed.auth.clients.len(), 2);
        assert!(parsed.auth.clients.iter().any(|client| client.id == "new"));
        assert!(parsed.auth.clients.iter().any(|client| client.id == "old"));

        remove_server_client(&path, "old").expect("remove old");
        let parsed = crate::config::parse_server_config(&fs::read_to_string(&path).expect("read"))
            .expect("parse after remove");
        assert_eq!(parsed.auth.clients.len(), 1);
        assert_eq!(parsed.auth.clients[0].id, "new");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn upsert_server_client_migrates_token_shorthand() {
        let path = std::env::temp_dir().join(format!(
            "meatus-config-edit-shorthand-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |value| value.as_nanos())
        ));
        let original = r#"
[server]
bind_addr = "127.0.0.1"
control_port = 17000

[auth]
token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
"#;
        fs::write(&path, original).expect("write original");
        let entry = ClientEntry {
            id: "home-nas".to_string(),
            tenant: "default".to_string(),
            token_sha256: Some(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            ),
            allow_ports: None,
            allow_types: Vec::new(),
            allow_domains: Vec::new(),
            max_proxies: None,
            limits: ClientLimits::default(),
        };
        upsert_server_client(&path, &entry).expect("upsert");
        let raw = fs::read_to_string(&path).expect("read");
        let parsed = crate::config::parse_server_config(&raw).expect("parse");
        assert!(parsed.auth.token_sha256.is_none());
        assert_eq!(parsed.auth.clients.len(), 2);
        assert!(
            parsed
                .auth
                .clients
                .iter()
                .any(|client| client.id == "default")
        );
        assert!(
            parsed
                .auth
                .clients
                .iter()
                .any(|client| client.id == "home-nas")
        );
        let _ = fs::remove_file(path);
    }
}
