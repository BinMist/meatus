#![forbid(unsafe_code)]

//! `meatus` 命令行入口。M0 提供 server/client/check/gen-token 的最小闭环。

use std::fmt::Write as _;
use std::fs;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use base64::Engine;
use clap::{Args, Parser, Subcommand, ValueEnum};
use meatus_client::ClientState;
use meatus_core::{
    ClientConfig, ConfigFormat, ConfigKind, LogFormat, ServerConfig, detect_kind_with_format,
    load_client_config_with_format, load_server_config_with_format,
};
use meatus_server::LogReloadHandle;
use meatus_server::ServerState;
use meatus_server::logbuf::LogBuffer;
use sha2::Digest;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(name = "meatus", version, about = "Rust 内网穿透：完全开源，自托管")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 relay（服务端）
    Server(ServerArgs),
    /// 启动客户端
    Client(ClientArgs),
    /// 校验配置并打印启动计划
    Check(CheckArgs),
    /// 生成客户端 token 与其 sha256
    GenToken,
    /// 生成自签证书（M3 实现）
    GenCert(GenCertArgs),
    /// 查询运行状态（M4）
    Status(StatusArgs),
    /// 配置备份与恢复
    Config(ConfigArgs),
}

#[derive(Args)]
struct ServerArgs {
    /// 服务端配置文件
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 配置格式；默认按扩展名识别
    #[arg(long, value_enum, default_value_t = FormatArg::Auto)]
    format: FormatArg,
    /// 只打印解析与监听计划，不启动
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ClientArgs {
    /// 客户端配置文件
    #[arg(short, long, default_value = "client.toml")]
    config: PathBuf,
    /// 配置格式；默认按扩展名识别
    #[arg(long, value_enum, default_value_t = FormatArg::Auto)]
    format: FormatArg,
    /// 只打印解析与代理清单，不启动
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct CheckArgs {
    /// 配置文件（自动识别 server / client）
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 强制按指定类型解析
    #[arg(long, value_enum)]
    kind: Option<KindArg>,
    /// 配置格式；默认按扩展名识别
    #[arg(long, value_enum, default_value_t = FormatArg::Auto)]
    format: FormatArg,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Server,
    Client,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Auto,
    Toml,
    Yaml,
}

impl From<FormatArg> for ConfigFormat {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Auto => Self::Auto,
            FormatArg::Toml => Self::Toml,
            FormatArg::Yaml => Self::Yaml,
        }
    }
}

impl From<KindArg> for ConfigKind {
    fn from(value: KindArg) -> Self {
        match value {
            KindArg::Server => Self::Server,
            KindArg::Client => Self::Client,
        }
    }
}

#[derive(Args)]
struct GenCertArgs {
    /// 证书域名或 IP，可重复
    #[arg(long = "domain")]
    domains: Vec<String>,
    /// 输出目录
    #[arg(short, long, default_value = "certs")]
    out: PathBuf,
    /// 覆盖已有证书
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// 导出脱敏配置备份
    Backup(BackupArgs),
    /// 从备份恢复配置
    Restore(RestoreArgs),
}

#[derive(Args)]
struct BackupArgs {
    /// 配置文件
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 输出备份 JSON；缺省为 meatus-backup-<server|client>-<时间>.json
    #[arg(short, long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct RestoreArgs {
    /// 目标配置文件
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 备份 JSON
    #[arg(long = "from")]
    from: PathBuf,
    /// 恢复时使用的 admin token；缺省从当前配置读取
    #[arg(long, env = "MEATUS_ADMIN_TOKEN")]
    admin_token: Option<String>,
    /// 恢复时使用的 DNS API token；缺省从当前配置读取
    #[arg(long, env = "MEATUS_DNS_API_TOKEN")]
    dns_api_token: Option<String>,
    /// 恢复时使用的 client token；缺省从当前配置读取
    #[arg(long, env = "MEATUS_CLIENT_TOKEN")]
    client_token: Option<String>,
}

#[derive(Args)]
struct StatusArgs {
    /// admin 面板地址
    #[arg(long, default_value = "http://127.0.0.1:7400")]
    admin: String,
    /// admin_token；也可用环境变量 MEATUS_ADMIN_TOKEN
    #[arg(long, env = "MEATUS_ADMIN_TOKEN")]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Server(args) => run_server(args).await,
        Command::Client(args) => run_client(args).await,
        Command::Check(args) => run_check(args),
        Command::GenToken => gen_token(),
        Command::GenCert(args) => gen_cert(&args),
        Command::Status(args) => status(&args).await,
        Command::Config(args) => run_config(&args),
    }
}

async fn run_server(args: ServerArgs) -> anyhow::Result<()> {
    let config = load_server_config_with_format(&args.config, args.format.into())
        .with_context(|| format!("load server config {}", args.config.display()))?;
    let logs = Arc::new(LogBuffer::default());
    let log_reload = init_logging(
        &config.log.level,
        config.log.format,
        Some(Arc::clone(&logs)),
    );

    let mut state = ServerState::new(config)?;
    state.logs = logs;
    state.set_log_reload(log_reload);
    let state = state.with_config_path(&args.config);
    if args.dry_run {
        state.config.validate_runtime()?;
        println!("{}", server_summary(&state.config));
        println!("\nplanned listeners:");
        for listener in state.planned_listeners() {
            println!("  {listener}");
        }
        return Ok(());
    }

    meatus_server::run(state).await
}

async fn run_client(args: ClientArgs) -> anyhow::Result<()> {
    let config = load_client_config_with_format(&args.config, args.format.into())
        .with_context(|| format!("load client config {}", args.config.display()))?;
    let _log_reload = init_logging(&config.log.level, config.log.format, None);

    let state = ClientState::new(config)?.with_config_path(&args.config);
    if args.dry_run {
        println!("{}", client_summary(&state.config));
        return Ok(());
    }

    meatus_client::run(state).await
}

fn run_check(args: CheckArgs) -> anyhow::Result<()> {
    let raw = fs::read_to_string(&args.config)
        .with_context(|| format!("read config {}", args.config.display()))?;
    let requested = args.format.into();
    let actual = meatus_core::resolve_config_format(&raw, &args.config, requested);

    let kind = match args.kind {
        Some(kind) => ConfigKind::from(kind),
        None => detect_kind_with_format(&raw, requested).with_context(|| {
            format!(
                "cannot detect config type in {} (expected [server] or [client])",
                args.config.display()
            )
        })?,
    };

    match kind {
        ConfigKind::Server => {
            let config = load_server_config_with_format(&args.config, requested)?;
            let state = ServerState::new(config)?;
            state.config.validate_runtime()?;
            println!("OK  {} (server, {})", args.config.display(), actual.label());
            print!("{}", server_summary(&state.config));
            println!("\nplanned listeners:");
            for listener in state.planned_listeners() {
                println!("  {listener}");
            }
        }
        ConfigKind::Client => {
            let config = load_client_config_with_format(&args.config, requested)?;
            let state = ClientState::new(config)?;
            println!("OK  {} (client, {})", args.config.display(), actual.label());
            print!("{}", client_summary(&state.config));
        }
    }

    Ok(())
}

fn run_config(args: &ConfigArgs) -> anyhow::Result<()> {
    match &args.command {
        ConfigCommand::Backup(args) => backup_config(args),
        ConfigCommand::Restore(args) => restore_config(args),
    }
}

fn backup_config(args: &BackupArgs) -> anyhow::Result<()> {
    let raw = fs::read_to_string(&args.config)
        .with_context(|| format!("read config {}", args.config.display()))?;
    let backup = meatus_core::backup::create_backup(&args.config, &raw, env!("CARGO_PKG_VERSION"))
        .map_err(anyhow::Error::msg)?;
    let out = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "meatus-backup-{}-{}.json",
            backup.kind, backup.created_at_ms
        ))
    });
    let json = serde_json::to_string_pretty(&backup).context("serialize backup")?;
    fs::write(&out, json).with_context(|| format!("write backup {}", out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&out, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", out.display()))?;
    }
    println!("backup  {}", out.display());
    println!("kind    {}", backup.kind);
    println!("format  {}", backup.format);
    println!("version {}", backup.meatus_version);
    println!("sha256  {}", backup.config_sha256);
    for certificate in &backup.certificates {
        println!("ref     {} = {}", certificate.role, certificate.path);
    }
    Ok(())
}

fn restore_config(args: &RestoreArgs) -> anyhow::Result<()> {
    let raw = fs::read_to_string(&args.from)
        .with_context(|| format!("read backup {}", args.from.display()))?;
    let backup: meatus_core::backup::BackupDocument =
        serde_json::from_str(&raw).context("parse backup json")?;
    let current = fs::read_to_string(&args.config).ok();
    let mut secrets = current
        .as_deref()
        .map(|raw| meatus_core::backup::secrets_from_raw(&args.config, raw))
        .unwrap_or_default();
    if args.admin_token.is_some() {
        secrets.admin_token.clone_from(&args.admin_token);
    }
    if args.dns_api_token.is_some() {
        secrets.dns_api_token.clone_from(&args.dns_api_token);
    }
    if args.client_token.is_some() {
        secrets.client_token.clone_from(&args.client_token);
    }
    let outcome = meatus_core::backup::restore_backup(&backup, &args.config, &secrets)
        .map_err(anyhow::Error::msg)?;
    println!("restored {}", args.config.display());
    println!("kind     {:?}", outcome.kind);
    println!("format   {:?}", outcome.format);
    if let Some(path) = outcome.backup_path {
        println!("previous {}", path.display());
    }
    for warning in outcome.warnings {
        println!("warning  {warning}");
    }
    run_check(CheckArgs {
        config: args.config.clone(),
        kind: None,
        format: FormatArg::Auto,
    })
    .context("恢复后自动校验失败")?;
    Ok(())
}

fn gen_token() -> anyhow::Result<()> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).context("generate random token failed")?;

    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));

    println!("token   {token}");
    println!("sha256  {hash}");
    println!();
    println!("服务端只保存 sha256；token 交给客户端，勿写入仓库。");
    Ok(())
}

fn gen_cert(args: &GenCertArgs) -> anyhow::Result<()> {
    if args.domains.is_empty() {
        anyhow::bail!("至少需要一个 --domain（域名或 IP）");
    }
    fs::create_dir_all(&args.out)
        .with_context(|| format!("create cert dir {}", args.out.display()))?;
    let crt = args.out.join("server.crt");
    let key = args.out.join("server.key");
    if !args.force && (crt.exists() || key.exists()) {
        anyhow::bail!("{} 已存在；确需覆盖请加 --force", args.out.display());
    }

    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(args.domains.clone())
            .context("generate self-signed certificate")?;
    fs::write(&crt, cert.pem()).with_context(|| format!("write {}", crt.display()))?;
    fs::write(&key, signing_key.serialize_pem())
        .with_context(|| format!("write {}", key.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", key.display()))?;
    }

    println!("cert    {}", crt.display());
    println!("key     {} (0600)", key.display());
    println!("domains {}", args.domains.join(", "));
    Ok(())
}

async fn status(args: &StatusArgs) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let raw = args.admin.trim_end_matches('/');
    let endpoint = if raw.starts_with("http://") {
        raw.to_string()
    } else if raw.contains("://") {
        anyhow::bail!("meatus status 目前只支持 http:// admin 地址");
    } else {
        format!("http://{raw}")
    };
    let Some(rest) = endpoint.strip_prefix("http://") else {
        anyhow::bail!("meatus status 目前只支持 http:// admin 地址");
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let addr = authority
        .to_socket_addrs()
        .with_context(|| format!("resolve admin address {authority}"))?
        .next()
        .with_context(|| format!("no address for {authority}"))?;
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect admin {addr}"))?;
    let auth = args
        .token
        .as_deref()
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET /api/v1/status HTTP/1.1\r\nHost: {authority}\r\n{auth}Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("admin response missing header terminator")?;
    let head = String::from_utf8_lossy(&response[..split]);
    let status_line = head.lines().next().unwrap_or_default();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if code != 200 {
        anyhow::bail!(
            "admin returned {status_line}: {}",
            String::from_utf8_lossy(&response[split + 4..]).trim()
        );
    }
    let body: serde_json::Value =
        serde_json::from_slice(&response[split + 4..]).context("parse /api/v1/status JSON")?;
    print_status(&body);
    Ok(())
}

fn print_status(body: &serde_json::Value) {
    let clients = body["clients"].as_array().map_or(&[][..], Vec::as_slice);
    let proxies = body["proxies"].as_array().map_or(&[][..], Vec::as_slice);
    println!(
        "meatus status: {} clients, {} proxies, uptime {}s",
        clients.len(),
        proxies.len(),
        body["uptime_s"].as_u64().unwrap_or(0)
    );
    if clients.is_empty() {
        println!("\nCLIENTS\n  (none)");
    } else {
        println!("\nCLIENTS\n  TENANT            CLIENT            RUN_ID            PROXIES");
        for client in clients {
            println!(
                "  {:<17} {:<17} {:<17} {}",
                client["tenant_id"].as_str().unwrap_or("-"),
                client["client_id"].as_str().unwrap_or("-"),
                client["run_id"].as_str().unwrap_or("-"),
                client["proxy_count"].as_u64().unwrap_or(0),
            );
        }
    }
    if proxies.is_empty() {
        println!("\nPROXIES\n  (none)");
        return;
    }
    println!(
        "\nPROXIES\n  NAME             TYPE      PORT   ACTIVE  TOTAL  IN_BYTES  OUT_BYTES  FAIL"
    );
    for proxy in proxies {
        println!(
            "  {:<16} {:<9} {:<6} {:<7} {:<6} {:<9} {:<10} {}",
            proxy["name"].as_str().unwrap_or("-"),
            proxy["type"].as_str().unwrap_or("-"),
            proxy["remote_port"].as_u64().unwrap_or(0),
            proxy["active_conns"].as_u64().unwrap_or(0),
            proxy["conn_total"].as_u64().unwrap_or(0),
            proxy["bytes_in"].as_u64().unwrap_or(0),
            proxy["bytes_out"].as_u64().unwrap_or(0),
            proxy["local_connect_fail"].as_u64().unwrap_or(0),
        );
    }
}

fn init_logging(
    level: &str,
    format: LogFormat,
    logs: Option<Arc<LogBuffer>>,
) -> Option<LogReloadHandle> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let (filter_layer, filter_handle) = tracing_subscriber::reload::Layer::new(filter);

    match format {
        LogFormat::Json => {
            let registry = tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt::layer().json().with_writer(std::io::stdout));
            if let Some(logs) = logs {
                let _ = registry
                    .with(
                        fmt::layer()
                            .json()
                            .with_ansi(false)
                            .with_writer((*logs).clone()),
                    )
                    .try_init();
            } else {
                let _ = registry.try_init();
            }
        }
        LogFormat::Text => {
            let registry = tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt::layer().compact().with_writer(std::io::stdout));
            if let Some(logs) = logs {
                let _ = registry
                    .with(
                        fmt::layer()
                            .compact()
                            .with_ansi(false)
                            .with_writer((*logs).clone()),
                    )
                    .try_init();
            } else {
                let _ = registry.try_init();
            }
        }
    }

    let handle = Arc::new(filter_handle);
    Some(Arc::new(move |level: &str| {
        let filter = EnvFilter::try_new(level).map_err(|error| error.to_string())?;
        handle
            .modify(|current| *current = filter)
            .map_err(|error| error.to_string())
    }) as LogReloadHandle)
}

fn server_summary(config: &ServerConfig) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "server   bind={} control_port={}",
        config.server.bind_addr, config.server.control_port
    );
    let _ = writeln!(
        out,
        "tls      cert={} key={}",
        config
            .server
            .tls_cert
            .as_ref()
            .map_or_else(|| "unset".to_string(), |path| path.display().to_string()),
        config
            .server
            .tls_key
            .as_ref()
            .map_or_else(|| "unset".to_string(), |path| path.display().to_string())
    );
    let _ = writeln!(out, "tenant   default={}", config.tenant.default);
    let _ = writeln!(
        out,
        "auth     mode={:?} provider={:?} clients={}",
        config.auth.mode,
        config.auth.provider,
        config.auth.clients.len()
    );
    if config.auth.clients.is_empty() {
        let _ = writeln!(out, "  client default/default (shared token hash)");
    } else {
        for client in &config.auth.clients {
            let _ = writeln!(
                out,
                "  client {}/{} allow_ports={:?}",
                client.tenant, client.id, client.allow_ports
            );
        }
    }
    let _ = writeln!(
        out,
        "ports    {}..={}",
        config.ports.range[0], config.ports.range[1]
    );
    let _ = writeln!(
        out,
        "http     enabled={} domain={}",
        config.http.enabled, config.http.domain
    );
    let _ = writeln!(
        out,
        "acme     enabled={} challenge={:?} staging={}",
        config.acme.enabled, config.acme.challenge, config.acme.staging
    );
    let _ = writeln!(
        out,
        "socks5   enabled={} bind={}",
        config.socks5.enabled, config.socks5.bind
    );
    let _ = writeln!(
        out,
        "admin    bind={} panel={} admin_token={} tls={}",
        config.admin.bind,
        config.admin.panel,
        if config.admin.admin_token.is_some() {
            "set"
        } else {
            "unset"
        },
        config.admin.tls.enabled
    );
    if config.admin.tls.enabled {
        let _ = writeln!(
            out,
            "  tls cert={} key={} hsts={} trust_forwarded_proto={}",
            config
                .admin
                .tls
                .cert
                .as_ref()
                .map_or_else(|| "unset".to_string(), |path| path.display().to_string()),
            config
                .admin
                .tls
                .key
                .as_ref()
                .map_or_else(|| "unset".to_string(), |path| path.display().to_string()),
            config.admin.tls.hsts,
            config.admin.trust_forwarded_proto
        );
    }
    out
}

fn client_summary(config: &ClientConfig) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "client   server_addr={}", config.client.server_addr);
    let _ = writeln!(
        out,
        "identity tenant={} client_id={}",
        config.client.tenant, config.client.client_id
    );
    let _ = writeln!(
        out,
        "tls      server_name={} ca={}",
        config.client.server_name.as_deref().unwrap_or("-"),
        config.client.server_ca_cert.as_ref().map_or_else(
            || "system roots".to_string(),
            |path| path.display().to_string()
        )
    );
    let _ = writeln!(out, "token    <redacted>");
    let pool = &config.client.pool;
    if pool.enabled {
        let _ = writeln!(
            out,
            "pool     enabled size={} max_streams_per_conn={} idle_timeout_s={}",
            pool.size, pool.max_streams_per_conn, pool.idle_timeout_s
        );
    } else {
        let _ = writeln!(out, "pool     disabled");
    }
    let _ = writeln!(out, "proxies  {}", config.proxies.len());
    for proxy in &config.proxies {
        let remote_port = proxy
            .remote_port
            .map_or_else(|| "auto".to_string(), |port| port.to_string());
        let domains = if proxy.custom_domains.is_empty() {
            "-".to_string()
        } else {
            proxy.custom_domains.join(",")
        };
        let _ = writeln!(
            out,
            "  - {} type={:?} local={} remote_port={} domains={}",
            proxy.name, proxy.kind, proxy.local_addr, remote_port, domains
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::{parse_client_config, parse_server_config};

    #[test]
    fn client_summary_redacts_token() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:22"
"#,
        )
        .expect("valid config");

        let summary = client_summary(&config);
        assert!(!summary.contains("secret-token"));
        assert!(summary.contains("<redacted>"));
    }

    #[test]
    fn server_summary_does_not_leak_hashes() {
        let config = parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
clients = [
  { id = "nas", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" },
]
"#,
        )
        .expect("valid config");

        let summary = server_summary(&config);
        assert!(!summary.contains("2bb80d537b1da3e38bd30361aa855686"));
        assert!(summary.contains("nas"));
    }
}
