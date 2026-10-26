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

    fn status(state: ServiceState, accept: ServiceControlAccept, code: u32, checkpoint: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint,
            wait_hint: Duration::from_secs(30),
            process_id: None,
        }
    }

    fn run() -> anyhow::Result<()> {
        let token = tokio_util::sync::CancellationToken::new();
        let t2 = token.clone();
        // the handler reports StopPending at once, and a reporter keeps the checkpoint
        // moving while the job's processes are terminated and the workers finish; the
        // mutex keeps a late StopPending from overwriting the final Stopped
        let cell: std::sync::Arc<std::sync::OnceLock<service_control_handler::ServiceStatusHandle>> = Default::default();
        let stopped: std::sync::Arc<std::sync::Mutex<bool>> = Default::default();
        let (cell2, stopped2) = (cell.clone(), stopped.clone());
        let handle = service_control_handler::register(crate::ops::SERVICE_NAME, move |c| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(h) = cell2.get().copied() {
                    let _ = h.set_service_status(status(ServiceState::StopPending, ServiceControlAccept::empty(), 0, 1));
                    let stopped = stopped2.clone();
                    std::thread::spawn(move || {
                        for cp in 2..=12 {
                            std::thread::sleep(Duration::from_secs(5));
                            let done = stopped.lock().unwrap_or_else(|p| p.into_inner());
                            if *done {
                                break;
                            }
                            let _ = h.set_service_status(status(ServiceState::StopPending, ServiceControlAccept::empty(), 0, cp));
                        }
                    });
                }
                t2.cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let _ = cell.set(handle);
        let status = |state: ServiceState, accept: ServiceControlAccept, code: u32| status(state, accept, code, 0);
        handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0))?;
        let cfg_path = CONFIG.get().cloned().unwrap_or_else(|| crate::config_path(&None));
        let res = (|| -> anyhow::Result<()> {
            let cfg = cimon_core::Config::load(&cfg_path)?;
            let _guard = crate::init_service_logging(&cimon_core::Layout::new(&cfg.paths.root).service_logs());
            let rt = tokio::runtime::Runtime::new()?;
            let r = rt.block_on(cimon::serve::run(cfg, Some(token)));
            // while the log writer is open — `_guard` closes it when this closure
            // returns, and service_main's line would then go nowhere
            if let Err(e) = &r {
                tracing::error!("service failed: {e:#}");
            }
            // blocking workers (a job being torn down, an intake fetch) get a bounded wait
            rt.shutdown_timeout(Duration::from_secs(25));
            r
        })();
        let code = if res.is_ok() { 0 } else { 1 };
        let mut done = stopped.lock().unwrap_or_else(|p| p.into_inner());
        *done = true;
        handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty(), code))?;
        drop(done);
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
