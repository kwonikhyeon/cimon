//! Running under the Windows service control manager (`cimon service`).

use std::path::PathBuf;
use std::sync::OnceLock;

static CONFIG: OnceLock<PathBuf> = OnceLock::new();

#[cfg(windows)]
mod imp {
    use super::CONFIG;
    use std::ffi::OsString;
    use std::time::Duration;
    use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run() {
            tracing::error!("service failed: {e:#}");
        }
    }

    fn run() -> anyhow::Result<()> {
        let token = tokio_util::sync::CancellationToken::new();
        let t2 = token.clone();
        let handle = service_control_handler::register(crate::ops::SERVICE_NAME, move |c| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                t2.cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let status = |state: ServiceState, accept: ServiceControlAccept, code: u32| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(30),
            process_id: None,
        };
        handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0))?;
        let cfg_path = CONFIG.get().cloned().unwrap_or_else(|| crate::config_path(&None));
        let res = (|| -> anyhow::Result<()> {
            let cfg = cimon_core::Config::load(&cfg_path)?;
            let _guard = crate::init_service_logging(&cimon_core::Layout::new(&cfg.paths.root).service_logs());
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cimon::serve::run(cfg, Some(token)))
        })();
        let code = if res.is_ok() { 0 } else { 1 };
        handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty(), code))?;
        res
    }

    pub fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(crate::ops::SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }
}

pub fn run(config: Option<PathBuf>) -> anyhow::Result<()> {
    if let Some(c) = config {
        let _ = CONFIG.set(c);
    }
    #[cfg(windows)]
    {
        imp::dispatch()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("the service mode is Windows-only — use `cimon serve`")
    }
}
