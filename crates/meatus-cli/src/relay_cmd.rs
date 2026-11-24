#![forbid(unsafe_code)]
//! `meatus relay bundle` 子命令：离线签名、验签与按配置导入。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use base64::Engine;
use clap::{Args, Subcommand};
use meatus_core::{
    ConfigFormat, RelayBundle, RelayBundleBody, RelaySigningKey, ServerConfig,
    check_epoch_rollback, current_epoch, load_bundle, load_server_config_with_format, now_rfc3339,
    verify_against_config, verify_bundle, write_bundle, write_epoch_state,
};

#[derive(Args)]
pub struct RelayArgs {
    #[command(subcommand)]
    command: RelayCommand,
}

#[derive(Subcommand)]
enum RelayCommand {
    /// 管理 relay 组签名配置包
    Bundle(RelayBundleArgs),
}

#[derive(Args)]
struct RelayBundleArgs {
    #[command(subcommand)]
    command: RelayBundleCommand,
}

#[derive(Subcommand)]
enum RelayBundleCommand {
    /// 生成 relay bundle 签名用 Ed25519 私钥（PKCS#8 PEM）
    Keygen(KeygenArgs),
    /// 对 draft JSON 做 Ed25519 签名，生成可分发 bundle
    Export(ExportArgs),
    /// 按服务端配置验签、校验 group/member/epoch
    Verify(VerifyArgs),
    /// 验签后把 bundle 安装到配置指定的路径
    Import(ImportArgs),
}

#[derive(Args)]
struct KeygenArgs {
    /// 输出的 PKCS#8 PEM 私钥路径
    #[arg(short, long, default_value = "relay-signing.pem")]
    out: PathBuf,
    /// 覆盖已有私钥
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct ExportArgs {
    /// 未签名的 bundle draft JSON
    #[arg(short, long)]
    input: PathBuf,
    /// Ed25519 PKCS#8 私钥（DER 或 PEM）
    #[arg(long = "signing-key")]
    signing_key: PathBuf,
    /// 输出 bundle JSON
    #[arg(short, long, default_value = "relay-bundle.json")]
    out: PathBuf,
    /// 覆盖 draft 里的 generated_at；缺省用当前 UTC
    #[arg(long = "generated-at")]
    generated_at: Option<String>,
}

#[derive(Args)]
struct VerifyArgs {
    /// 服务端配置；提供 relay 组、bundle_pubkey 与本地 ports.range
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 待验 bundle；缺省使用 [relay].bundle
    #[arg(long)]
    bundle: Option<PathBuf>,
}

#[derive(Args)]
struct ImportArgs {
    /// 服务端配置；目标路径取自 [relay].bundle
    #[arg(short, long, default_value = "server.toml")]
    config: PathBuf,
    /// 已签名的 bundle JSON
    #[arg(long)]
    bundle: PathBuf,
}

pub fn run(args: RelayArgs) -> anyhow::Result<()> {
    match args.command {
        RelayCommand::Bundle(bundle) => match bundle.command {
            RelayBundleCommand::Keygen(args) => keygen(args),
            RelayBundleCommand::Export(args) => export(args),
            RelayBundleCommand::Verify(args) => verify(args),
            RelayBundleCommand::Import(args) => import(args),
        },
    }
}

fn keygen(args: KeygenArgs) -> anyhow::Result<()> {
    if args.out.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to overwrite",
            args.out.display()
        );
    }
    let (signing, der) = RelaySigningKey::generate().context("generate Ed25519 signing key")?;
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(der)
    );
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create key directory {}", parent.display()))?;
    }
    fs::write(&args.out, pem).with_context(|| format!("write {}", args.out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&args.out, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", args.out.display()))?;
    }
    println!("private-key {}", args.out.display());
    println!("pubkey      {}", signing.public_key());
    println!();
    println!("把 pubkey 填进 [relay].bundle_pubkey；私钥只留在签名机。");
    Ok(())
}

fn export(args: ExportArgs) -> anyhow::Result<()> {
    let raw = fs::read_to_string(&args.input)
        .with_context(|| format!("read bundle draft {}", args.input.display()))?;
    let mut body: RelayBundleBody =
        serde_json::from_str(&raw).context("parse bundle draft JSON")?;
    if let Some(generated_at) = args.generated_at {
        body.generated_at = generated_at;
    }
    if body.generated_at.trim().is_empty() {
        body.generated_at = now_rfc3339();
    }

    let der = read_signing_key(&args.signing_key)?;
    let signing = RelaySigningKey::from_pkcs8(&der).context("load Ed25519 signing key")?;
    let bundle = signing.sign(&body).context("sign relay bundle")?;
    let public_key = signing.public_key();
    verify_bundle(&bundle, &public_key).context("self-check signed bundle")?;
    write_bundle(&args.out, &bundle)
        .with_context(|| format!("write bundle {}", args.out.display()))?;

    println!("bundle   {}", args.out.display());
    println!("group    {}", bundle.group);
    println!("epoch    {}", bundle.epoch);
    println!("hash     {}", bundle.config_hash);
    println!("relays   {}", bundle.relays.len());
    println!("pubkey   {public_key}");
    println!();
    println!("把上面的 pubkey 填进 [relay].bundle_pubkey；私钥不要下发到 relay 节点。");
    Ok(())
}

fn verify(args: VerifyArgs) -> anyhow::Result<()> {
    let config = load_config(&args.config)?;
    let relay = config
        .relay
        .as_ref()
        .context("server config has no [relay] section")?;
    let path = args.bundle.unwrap_or_else(|| relay.bundle.clone());
    let bundle = verify_path(&config, &path)?;
    print_bundle("OK", &path, &bundle);
    Ok(())
}

fn import(args: ImportArgs) -> anyhow::Result<()> {
    let config = load_config(&args.config)?;
    let relay = config
        .relay
        .as_ref()
        .context("server config has no [relay] section")?;
    let bundle = verify_path(&config, &args.bundle)?;
    let destination = relay.bundle.clone();
    let last = current_epoch(&destination)
        .with_context(|| format!("read current epoch {}", destination.display()))?;
    if bundle.epoch < last {
        bail!(
            "refuse relay bundle epoch rollback: {} < current {last}",
            bundle.epoch
        );
    }
    write_bundle(&destination, &bundle)
        .with_context(|| format!("install bundle {}", destination.display()))?;
    write_epoch_state(&destination, bundle.epoch)
        .with_context(|| format!("record epoch {}", destination.display()))?;

    let installed = load_bundle(&destination).context("reload installed bundle")?;
    verify_bundle(&installed, &relay.bundle_pubkey).context("verify installed bundle")?;
    verify_against_config(&config, &installed).context("verify installed bundle against config")?;
    print_bundle("imported", &destination, &installed);
    Ok(())
}

fn load_config(path: &Path) -> anyhow::Result<ServerConfig> {
    let config = load_server_config_with_format(path, ConfigFormat::Auto)
        .with_context(|| format!("load server config {}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("validate server config {}", path.display()))?;
    Ok(config)
}

fn verify_path(config: &ServerConfig, path: &Path) -> anyhow::Result<RelayBundle> {
    let relay = config
        .relay
        .as_ref()
        .context("server config has no [relay] section")?;
    let bundle =
        load_bundle(path).with_context(|| format!("load relay bundle {}", path.display()))?;
    // verify_for_config 使用配置里的 bundle 路径；命令行可显式指定另一份文件，
    // 因此这里拆开做同样的验签、配置绑定与 epoch 检查。
    verify_bundle(&bundle, &relay.bundle_pubkey).context("verify relay bundle signature")?;
    verify_against_config(config, &bundle).context("verify bundle against server config")?;
    if path == relay.bundle {
        check_epoch_rollback(path, bundle.epoch).context("check relay bundle epoch")?;
    }
    Ok(bundle)
}

fn print_bundle(status: &str, path: &Path, bundle: &RelayBundle) {
    println!("{status}     {}", path.display());
    println!("group    {}", bundle.group);
    println!("epoch    {}", bundle.epoch);
    println!("hash     {}", bundle.config_hash);
    for relay in &bundle.relays {
        println!(
            "relay    {} role={} control={} ports={}..={} domains={}",
            relay.id,
            relay.role.as_str(),
            relay.control_addr,
            relay.ports[0],
            relay.ports[1],
            if relay.http_domains.is_empty() {
                "-".to_string()
            } else {
                relay.http_domains.join(",")
            }
        );
    }
}

fn read_signing_key(path: &Path) -> anyhow::Result<Vec<u8>> {
    let raw = fs::read(path).with_context(|| format!("read signing key {}", path.display()))?;
    if !raw.starts_with(b"-----BEGIN") {
        return Ok(raw);
    }
    let text = String::from_utf8(raw).context("signing key PEM is not valid UTF-8")?;
    let mut encoded = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("-----") {
            continue;
        }
        encoded.push_str(line);
    }
    if encoded.is_empty() {
        bail!("signing key PEM has no base64 body");
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decode signing key PEM base64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::RelayRole;

    fn draft(epoch: u64) -> String {
        format!(
            r#"{{
  "version": 1,
  "group": "prod",
  "epoch": {epoch},
  "generated_at": "2026-11-23T00:00:00Z",
  "relays": [
    {{"id":"relay-a","role":"primary","control_addr":"relay-a.example.com:7000","http_domains":["*.example.com"],"ports":[20000,30000]}},
    {{"id":"relay-b","role":"secondary","control_addr":"relay-b.example.com:7000","http_domains":["*.example.com"],"ports":[30001,40000]}}
  ]
}}"#
        )
    }

    fn config_text(work: &Path, pubkey: &str) -> String {
        format!(
            r#"
[server]
bind_addr = "127.0.0.1"
control_port = 7000
tls_cert = "{}"
tls_key = "{}"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{}" }},
]

[ports]
range = [20000, 30000]

[relay]
id = "relay-a"
group = "prod"
role = "primary"
advertise_addr = "relay-a.example.com:7000"
bundle = "{}"
bundle_pubkey = "{pubkey}"
peer_secret_file = "{}"
"#,
            work.join("server.crt").display(),
            work.join("server.key").display(),
            "00".repeat(32),
            work.join("relay-bundle.json").display(),
            work.join("relay-peer.key").display()
        )
    }

    #[test]
    fn cli_export_verify_import_and_epoch_rollback() {
        let work = std::env::temp_dir().join(format!("meatus-cli-relay-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).expect("create work dir");

        let (signing, der) = RelaySigningKey::generate().expect("generate key");
        let key_path = work.join("relay-signing.der");
        fs::write(&key_path, &der).expect("write key");
        let bundle_path = work.join("relay-bundle.json");
        let draft_path = work.join("draft.json");
        fs::write(&draft_path, draft(3)).expect("write draft");

        export(ExportArgs {
            input: draft_path.clone(),
            signing_key: key_path,
            out: bundle_path.clone(),
            generated_at: None,
        })
        .expect("export");

        let config_path = work.join("server.toml");
        fs::write(&config_path, config_text(&work, &signing.public_key())).expect("write config");
        verify(VerifyArgs {
            config: config_path.clone(),
            bundle: None,
        })
        .expect("verify");
        import(ImportArgs {
            config: config_path.clone(),
            bundle: bundle_path.clone(),
        })
        .expect("import");
        assert_eq!(current_epoch(&bundle_path).expect("epoch"), 3);

        let raw = fs::read_to_string(&bundle_path).expect("read bundle");
        assert!(!raw.contains("token"));
        assert!(!raw.contains("PRIVATE KEY"));
        assert!(!raw.contains("secret"));

        let old_path = work.join("old-bundle.json");
        fs::write(&draft_path, draft(2)).expect("write old draft");
        export(ExportArgs {
            input: draft_path,
            signing_key: work.join("relay-signing.der"),
            out: old_path.clone(),
            generated_at: None,
        })
        .expect("export old");
        let err = import(ImportArgs {
            config: config_path,
            bundle: old_path,
        })
        .expect_err("rollback must fail");
        assert!(err.to_string().contains("epoch rollback"), "{err}");

        let raw_bundle: serde_json::Value = serde_json::from_str(&raw).expect("parse bundle");
        assert_eq!(raw_bundle["group"], "prod");
        assert_eq!(raw_bundle["relays"][1]["role"], "secondary");
        assert_eq!(
            RelayRole::Secondary.as_str(),
            raw_bundle["relays"][1]["role"]
                .as_str()
                .expect("secondary role")
        );

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn signing_key_pem_body_is_decoded() {
        let work = std::env::temp_dir().join(format!("meatus-cli-pem-{}", std::process::id()));
        fs::create_dir_all(&work).expect("create work dir");
        let (_, der) = RelaySigningKey::generate().expect("generate key");
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&der)
        );
        let path = work.join("key.pem");
        fs::write(&path, pem).expect("write pem");
        assert_eq!(read_signing_key(&path).expect("decode"), der);
        let _ = fs::remove_dir_all(&work);
    }
}
