//! Controlling the cimon Windows service from outside it — shared by the CLI
//! (`cimon ctl`, `cimon setup`) and the tray app (docs/tray.md): the service name, the
//! user-defined control codes the service answers, the service DACL cimon writes and
//! the checks on it, and argument quoting for elevated launches.

pub mod dacl;

/// The service `cimon setup` registers.
pub const SERVICE: &str = "cimon";

/// User-defined control: refuse new jobs and stop once nothing runs (docs/tray.md §4.2).
pub const STOP_WHEN_IDLE: u32 = 128;
/// User-defined control: take jobs again, cancelling [`STOP_WHEN_IDLE`].
pub const CANCEL_STOP_WHEN_IDLE: u32 = 129;

/// The service as the service control manager sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Stopped,
    StartPending,
    StopPending,
    Running,
    /// paused or pausing — cimon does not pause, but someone could
    Other,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::NotInstalled => "not installed",
            State::Stopped => "stopped",
            State::StartPending => "starting",
            State::StopPending => "stopping",
            State::Running => "running",
            State::Other => "other",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    /// The caller may not do this — elevate, or join `cimon-operators` (docs/tray.md §4.1).
    #[error("access denied")]
    AccessDenied,
    #[error("the {SERVICE} service is not installed")]
    NotInstalled,
    #[error("{0}")]
    Other(String),
}

/// MSVCRT quoting of one argument for a command line (what `CommandLineToArgvW` and the
/// C runtime split back): quotes only when needed, doubling backslashes before a quote.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_service::service::{ServiceAccess, ServiceState, UserEventCode};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

    fn map(e: windows_service::Error) -> CtlError {
        match &e {
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(ERROR_ACCESS_DENIED) => CtlError::AccessDenied,
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) => CtlError::NotInstalled,
            _ => CtlError::Other(e.to_string()),
        }
    }

    fn open(access: ServiceAccess) -> Result<windows_service::service::Service, CtlError> {
        let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(map)?;
        m.open_service(SERVICE, access).map_err(map)
    }

    pub fn query() -> Result<State, CtlError> {
        let s = match open(ServiceAccess::QUERY_STATUS) {
            Err(CtlError::NotInstalled) => return Ok(State::NotInstalled),
            r => r?,
        };
        Ok(match s.query_status().map_err(map)?.current_state {
            ServiceState::Stopped => State::Stopped,
            ServiceState::StartPending => State::StartPending,
            ServiceState::StopPending => State::StopPending,
            ServiceState::Running => State::Running,
            _ => State::Other,
        })
    }

    pub fn start() -> Result<(), CtlError> {
        open(ServiceAccess::START)?.start::<&str>(&[]).map_err(map)
    }

    pub fn stop() -> Result<(), CtlError> {
        open(ServiceAccess::STOP)?.stop().map(|_| ()).map_err(map)
    }

    pub fn notify(code: u32) -> Result<(), CtlError> {
        let code = UserEventCode::from_raw(code).map_err(|e| CtlError::Other(format!("{e:?}")))?;
        open(ServiceAccess::USER_DEFINED_CONTROL)?.notify(code).map(|_| ()).map_err(map)
    }
}

#[cfg(windows)]
pub use win::{notify, query, start, stop};

#[cfg(not(windows))]
mod other {
    use super::*;
    pub fn query() -> Result<State, CtlError> {
        Ok(State::NotInstalled)
    }
    pub fn start() -> Result<(), CtlError> {
        Err(CtlError::NotInstalled)
    }
    pub fn stop() -> Result<(), CtlError> {
        Err(CtlError::NotInstalled)
    }
    pub fn notify(_code: u32) -> Result<(), CtlError> {
        Err(CtlError::NotInstalled)
    }
}

#[cfg(not(windows))]
pub use other::{notify, query, start, stop};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_the_c_runtime() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(r"C:\Program Files\cimon\cimon.exe"), r#""C:\Program Files\cimon\cimon.exe""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(""), r#""""#);
    }
}
