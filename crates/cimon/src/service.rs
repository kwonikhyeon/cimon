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
        // the controls are honored only while the DACL keeps them from logged-on users —
        // read again at each control and every half minute until it is fixed, so an admin's
        // `cimon setup --service-acl` takes effect without a restart
        let dacl_problems = current_dacl_problems();
        let controls_ok = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(dacl_problems.is_empty()));
        if !dacl_problems.is_empty() {
            let ok = controls_ok.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(30));
                if recheck(&ok) {
                    return;
                }
            });
        }
        let ok2 = controls_ok.clone();
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
            // stop when idle / cancel (docs/tray.md §4.2) — only while the DACL keeps them
            // to operators and admins; otherwise anyone logged on could stop the service
            ServiceControl::UserEvent(code) if matches!(code.to_raw(), cimon_ctl::STOP_WHEN_IDLE | cimon_ctl::CANCEL_STOP_WHEN_IDLE) => {
                if !ok2.load(std::sync::atomic::Ordering::SeqCst) && !recheck(&ok2) {
                    tracing::warn!("refused control {} — the service DACL lets logged-on users send it (cimon setup --service-acl)", code.to_raw());
                    return ServiceControlHandlerResult::NotImplemented;
                }
                match cimon::serve::app() {
                    Some(app) => {
                        cimon_engine::stop_when_idle(&app, code.to_raw() == cimon_ctl::STOP_WHEN_IDLE);
                        ServiceControlHandlerResult::NoError
                    }
                    None => ServiceControlHandlerResult::NotImplemented,
                }
            }
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
            let mut notices = vec![];
            if !dacl_problems.is_empty() {
                let w = format!("{DACL_NOTICE} {} — 「작업 끝나면 중지」 제어를 받지 않는다. 관리자가 cimon setup --service-acl 을 실행한다", dacl_problems.join("; "));
                tracing::warn!("{w}");
                notices.push(w);
            }
            let r = rt.block_on(cimon::serve::run_with(cfg, Some(token), notices));
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

    fn current_dacl_problems() -> Vec<String> {
        cimon_ctl::dacl::read().map(|s| cimon_ctl::dacl::problems(&s)).unwrap_or_else(|e| vec![format!("could not read it: {e}")])
    }

    /// Reads the DACL again; once it is right, controls are taken and the start-up
    /// warning leaves `/healthz`.
    fn recheck(ok: &std::sync::atomic::AtomicBool) -> bool {
        if !current_dacl_problems().is_empty() {
            return false;
        }
        ok.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(app) = cimon::serve::app() {
            app.engine.notices.lock().unwrap_or_else(|p| p.into_inner()).retain(|n| !n.starts_with(DACL_NOTICE));
        }
        tracing::info!("the service DACL is right now — stop-when-idle controls are taken");
        true
    }

    const DACL_NOTICE: &str = "서비스 DACL:";

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
