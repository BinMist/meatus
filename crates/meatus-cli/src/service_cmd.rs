//! Windows 客户端服务管理。
//!
//! 服务固定以本机 `LocalSystem` 账户运行，配置与日志放在 `%ProgramData%\Meatus`。
//! 非 Windows 平台只保留命令入口，执行时明确报错。

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

#[cfg_attr(not(windows), allow(dead_code))]
pub const SERVICE_NAME: &str = "meatus-client";
#[cfg_attr(not(windows), allow(dead_code))]
pub const SERVICE_DISPLAY_NAME: &str = "Meatus Client";

#[derive(Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// 安装客户端服务并复制配置到 ProgramData
    Install(InstallArgs),
    /// 卸载客户端服务
    Uninstall(UninstallArgs),
    /// 启动客户端服务
    Start,
    /// 停止客户端服务
    Stop,
    /// 查询客户端服务状态
    Status,
    /// 由 SCM 调用；也可用 --console 前台运行
    Run(RunArgs),
}

#[derive(Args)]
struct InstallArgs {
    /// 源客户端配置；缺省为当前目录的 client.toml
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// 安装后不启动
    #[arg(long)]
    no_start: bool,
}

#[derive(Args)]
struct UninstallArgs {
    /// 同时删除 %ProgramData%\Meatus 下的配置与日志
    #[arg(long)]
    purge: bool,
}

#[derive(Args)]
struct RunArgs {
    /// 配置文件；缺省为 %ProgramData%\Meatus\client.toml
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// 不注册 SCM，直接前台运行，便于排障
    #[arg(long)]
    console: bool,
}

pub fn run(args: ServiceArgs) -> Result<()> {
    #[cfg(windows)]
    {
        windows_impl::run(args)
    }

    #[cfg(not(windows))]
    {
        let _ = args;
        anyhow::bail!("meatus service 仅支持 Windows；当前平台没有 SCM")
    }
}

/// `%ProgramData%\Meatus`；Windows 服务与安装命令共用。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Meatus")
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn default_config_path() -> PathBuf {
    default_data_dir().join("client.toml")
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn default_log_dir() -> PathBuf {
    default_data_dir().join("logs")
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, anyhow, bail};
    use meatus_client::{ClientState, run_until};
    use meatus_core::{ConfigFormat, LogFormat, load_client_config_with_format};
    use tokio_util::sync::CancellationToken;
    use tracing_appender::non_blocking::WorkerGuard;
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    use windows_service::{
        define_windows_service,
        service::{
            ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl,
            ServiceExitCode, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    use super::{
        InstallArgs, RunArgs, SERVICE_DISPLAY_NAME, SERVICE_NAME, ServiceArgs, ServiceCommand,
        UninstallArgs, default_config_path, default_data_dir, default_log_dir,
    };

    pub fn run(args: ServiceArgs) -> Result<()> {
        match args.command {
            ServiceCommand::Install(args) => install(args),
            ServiceCommand::Uninstall(args) => uninstall(args),
            ServiceCommand::Start => start(),
            ServiceCommand::Stop => stop(),
            ServiceCommand::Status => status(),
            ServiceCommand::Run(args) => run_command(args),
        }
    }

    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access)
            .map_err(|error| anyhow!("open service manager: {error}"))
    }

    fn install(args: InstallArgs) -> Result<()> {
        let source = args.config.unwrap_or_else(|| PathBuf::from("client.toml"));
        if !source.is_file() {
            bail!("config file not found: {}", source.display());
        }

        // 先校验源配置，再碰已有服务与 ProgramData，避免半装或覆盖现有配置。
        let source_config = load_client_config_with_format(&source, ConfigFormat::Auto)
            .with_context(|| format!("load {}", source.display()))?;
        ClientState::new(source_config).with_context(|| "validate client config")?;

        let service_manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        if service_manager
            .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
            .is_ok()
        {
            bail!("service {SERVICE_NAME} already exists; run `meatus service uninstall` first");
        }

        let data_dir = default_data_dir();
        fs::create_dir_all(&data_dir).with_context(|| format!("create {}", data_dir.display()))?;
        fs::create_dir_all(default_log_dir())
            .with_context(|| format!("create {}", default_log_dir().display()))?;

        let destination = default_config_path();
        if source != destination {
            fs::copy(&source, &destination).with_context(|| {
                format!("copy {} to {}", source.display(), destination.display())
            })?;
        }

        let config = load_client_config_with_format(&destination, ConfigFormat::Auto)
            .with_context(|| format!("load {}", destination.display()))?;
        ClientState::new(config).with_context(|| "validate copied client config")?;

        let executable = std::env::current_exe().context("resolve current executable")?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: executable,
            launch_arguments: vec![OsString::from("service"), OsString::from("run")],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let service = service_manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .map_err(|error| anyhow!("create service: {error}"))?;
        service
            .set_description("Meatus 内网穿透客户端；配置与日志位于 %ProgramData%\\Meatus")
            .map_err(|error| anyhow!("set service description: {error}"))?;

        println!("installed {SERVICE_NAME}");
        println!("config  {}", destination.display());
        println!("logs    {}", default_log_dir().display());

        if !args.no_start {
            service
                .start::<OsString>(&[])
                .map_err(|error| anyhow!("start service: {error}"))?;
            wait_for_state(&service, ServiceState::Running, Duration::from_secs(15))?;
            println!("state   running");
        }
        Ok(())
    }

    fn uninstall(args: UninstallArgs) -> Result<()> {
        let service_manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = service_manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
            )
            .map_err(|error| anyhow!("open service {SERVICE_NAME}: {error}"))?;

        let current = service
            .query_status()
            .map_err(|error| anyhow!("query service: {error}"))?;
        if current.current_state != ServiceState::Stopped {
            service
                .stop()
                .map_err(|error| anyhow!("stop service: {error}"))?;
            wait_for_state(&service, ServiceState::Stopped, Duration::from_secs(30))?;
        }
        service
            .delete()
            .map_err(|error| anyhow!("delete service: {error}"))?;
        drop(service);

        println!("uninstalled {SERVICE_NAME}");
        if args.purge {
            let data_dir = default_data_dir();
            match fs::remove_dir_all(&data_dir) {
                Ok(()) => println!("purged  {}", data_dir.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("remove {}", data_dir.display()));
                }
            }
        } else {
            println!(
                "kept    {} (use --purge to remove)",
                default_data_dir().display()
            );
        }
        Ok(())
    }

    fn start() -> Result<()> {
        let service_manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = service_manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::START,
            )
            .map_err(|error| anyhow!("open service {SERVICE_NAME}: {error}"))?;
        let current = service
            .query_status()
            .map_err(|error| anyhow!("query service: {error}"))?;
        match current.current_state {
            ServiceState::Running => {
                println!("state   running");
                return Ok(());
            }
            ServiceState::StartPending => {
                let status =
                    wait_for_state(&service, ServiceState::Running, Duration::from_secs(15))?;
                print_status(&status);
                return Ok(());
            }
            ServiceState::StopPending => {
                wait_for_state(&service, ServiceState::Stopped, Duration::from_secs(30))?;
            }
            _ => {}
        }
        service
            .start::<OsString>(&[])
            .map_err(|error| anyhow!("start service: {error}"))?;
        let status = wait_for_state(&service, ServiceState::Running, Duration::from_secs(15))?;
        print_status(&status);
        Ok(())
    }

    fn stop() -> Result<()> {
        let service_manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = service_manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP,
            )
            .map_err(|error| anyhow!("open service {SERVICE_NAME}: {error}"))?;
        let current = service
            .query_status()
            .map_err(|error| anyhow!("query service: {error}"))?;
        match current.current_state {
            ServiceState::Stopped => {
                println!("state   stopped");
                return Ok(());
            }
            ServiceState::StopPending => {
                wait_for_state(&service, ServiceState::Stopped, Duration::from_secs(30))?;
            }
            _ => {
                service
                    .stop()
                    .map_err(|error| anyhow!("stop service: {error}"))?;
                wait_for_state(&service, ServiceState::Stopped, Duration::from_secs(30))?;
            }
        }
        println!("state   stopped");
        Ok(())
    }

    fn status() -> Result<()> {
        let service_manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = service_manager
            .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
            .map_err(|error| anyhow!("open service {SERVICE_NAME}: {error}"))?;
        let status = service
            .query_status()
            .map_err(|error| anyhow!("query service: {error}"))?;
        print_status(&status);
        Ok(())
    }

    fn run_command(args: RunArgs) -> Result<()> {
        let config_path = args.config.unwrap_or_else(default_config_path);
        if args.console {
            return run_console(&config_path);
        }
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|error| anyhow!("start service dispatcher: {error}"))
    }

    fn run_console(config_path: &Path) -> Result<()> {
        let config = load_client_config_with_format(config_path, ConfigFormat::Auto)
            .with_context(|| format!("load {}", config_path.display()))?;
        let _guard = init_file_logging(&config.log.level, config.log.format)?;
        let state = ClientState::new(config)?.with_config_path(config_path);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        runtime.block_on(meatus_client::run(state))
    }

    define_windows_service!(ffi_service_main, my_service_main);

    fn my_service_main(_arguments: Vec<OsString>) {
        if let Err(error) = run_service() {
            record_service_error(&error);
        }
    }

    fn run_service() -> Result<()> {
        // SCM 默认工作目录是 System32；切到 ProgramData，让配置里的相对路径
        // （证书、CA 等）与安装布局一致。
        let data_dir = default_data_dir();
        fs::create_dir_all(&data_dir).with_context(|| format!("create {}", data_dir.display()))?;
        std::env::set_current_dir(&data_dir)
            .with_context(|| format!("set working directory {}", data_dir.display()))?;

        let config_path = default_config_path();
        let config = load_client_config_with_format(&config_path, ConfigFormat::Auto)
            .with_context(|| format!("load {}", config_path.display()))?;
        let _guard = init_file_logging(&config.log.level, config.log.format)?;

        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
        let event_handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = shutdown_tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
            .map_err(|error| anyhow!("register service control handler: {error}"))?;

        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::StartPending,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::NO_ERROR,
                checkpoint: 1,
                wait_hint: Duration::from_secs(3),
                process_id: None,
            })
            .map_err(|error| anyhow!("report start pending: {error}"))?;

        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let worker = thread::Builder::new()
            .name("meatus-client-service".to_string())
            .spawn(move || -> Result<()> {
                let state = ClientState::new(config)?.with_config_path(&config_path);
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("build tokio runtime")?;
                runtime.block_on(run_until(state, worker_cancel))
            })
            .context("spawn service worker")?;

        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Running,
                controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                exit_code: ServiceExitCode::NO_ERROR,
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .map_err(|error| anyhow!("report running: {error}"))?;

        loop {
            match shutdown_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if worker.is_finished() {
                        break;
                    }
                }
            }
        }

        cancel.cancel();
        let worker_result = worker
            .join()
            .map_err(|_| anyhow!("service worker panicked"))?;

        let exit_code = if worker_result.is_ok() {
            ServiceExitCode::NO_ERROR
        } else {
            ServiceExitCode::ServiceSpecific(1)
        };
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code,
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .map_err(|error| anyhow!("report stopped: {error}"))?;

        worker_result
    }

    fn init_file_logging(level: &str, format: LogFormat) -> Result<WorkerGuard> {
        let log_dir = default_log_dir();
        fs::create_dir_all(&log_dir).with_context(|| format!("create {}", log_dir.display()))?;
        let appender = tracing_appender::rolling::daily(&log_dir, "client.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

        match format {
            LogFormat::Json => tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().json().with_ansi(false).with_writer(writer))
                .try_init()
                .map_err(|error| anyhow!("init json logging: {error}"))?,
            LogFormat::Text => tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().compact().with_ansi(false).with_writer(writer))
                .try_init()
                .map_err(|error| anyhow!("init text logging: {error}"))?,
        }
        Ok(guard)
    }

    fn record_service_error(error: &anyhow::Error) {
        let dir = default_log_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("service-error.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{error:#}");
        }
    }

    fn wait_for_state(
        service: &windows_service::service::Service,
        desired: ServiceState,
        timeout: Duration,
    ) -> Result<ServiceStatus> {
        let started = Instant::now();
        loop {
            let status = service
                .query_status()
                .map_err(|error| anyhow!("query service: {error}"))?;
            if status.current_state == desired {
                return Ok(status);
            }
            if desired != ServiceState::Stopped && status.current_state == ServiceState::Stopped {
                bail!(
                    "service stopped before reaching {:?}; check {}",
                    desired,
                    default_log_dir().join("service-error.log").display()
                );
            }
            if started.elapsed() >= timeout {
                bail!(
                    "service did not reach {:?} within {}s (current {:?})",
                    desired,
                    timeout.as_secs(),
                    status.current_state
                );
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn print_status(status: &ServiceStatus) {
        println!("state   {:?}", status.current_state);
        if let Some(pid) = status.process_id {
            println!("pid     {pid}");
        }
    }
}
