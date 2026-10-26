//! Running one command: environment from scratch, a Job Object around it, output to a
//! capped log with the first error line and `fail_on_output` hits picked out, timeout and
//! cancellation by terminating the whole job.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub log_path: PathBuf,
    /// Log cap in bytes (design §9.5: 200 MB, keeping the first and last quarter).
    pub log_cap: u64,
    pub error_patterns: Vec<Regex>,
    pub fail_on_output: Vec<String>,
    /// Lines starting with (after trimming) one of these are captured, e.g. "test result:".
    pub capture_prefixes: Vec<String>,
    /// Lines written before the command's output (pre-clean report, command line).
    pub preamble: Vec<String>,
    /// Values the command's environment carries that its output must not show (text
    /// uploads): replaced by `***` in the log, the tail and the matches.
    pub redact: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub spawn_error: Option<String>,
    pub first_error: Option<String>,
    pub fail_hits: Vec<String>,
    pub captured: Vec<String>,
    pub tail: Vec<String>,
    /// Processes still alive in the job after the command exited (terminated).
    pub leftover: u32,
    pub truncated: bool,
    pub seconds: f64,
}

/// Who the command runs as.
pub enum Account {
    /// The service process's own user (single-account development mode).
    Current,
    /// The build account, through the `__exec` helper (design §9.2).
    #[cfg(windows)]
    Build { logon: Arc<crate::win::Logon>, helper: PathBuf },
}

pub const DEFAULT_ERROR_PATTERNS: &[&str] = &[
    r"(?i)^\s*error(\[[A-Za-z0-9]+\])?\s*:",
    r"(?i)\bfatal error\b",
    r"panicked at",
    r"^FAILURE:",
    r"(?i)^\s*\S+\(\d+(,\d+)?\)\s*:\s*error\b",
    r"(?i)\berror TS\d+",
    r"(?i)^npm ERR!",
];

pub fn default_error_patterns() -> Vec<Regex> {
    DEFAULT_ERROR_PATTERNS.iter().map(|p| Regex::new(p).unwrap()).collect()
}

struct LogState {
    file: Option<std::fs::File>,
    path: PathBuf,
    /// Bytes written to the file before truncation started.
    written: u64,
    cap: u64,
    truncating: bool,
    /// The file keeps its first `head` bytes once truncation starts.
    head: u64,
    ring: VecDeque<u8>,
    /// Bytes that arrived after truncation started.
    appended: u64,
    tail: VecDeque<String>,
    first_error: Option<String>,
    fail_hits: Vec<String>,
    captured: Vec<String>,
    /// Longest first, so a value that contains another is replaced whole.
    redact: Vec<Vec<u8>>,
}

/// `raw` with every occurrence of each value replaced by `***`.
fn redact<'a>(raw: &'a [u8], values: &[Vec<u8>]) -> std::borrow::Cow<'a, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    for s in values {
        let cur: &[u8] = out.as_deref().unwrap_or(raw);
        if s.is_empty() || !cur.windows(s.len()).any(|w| w == s.as_slice()) {
            continue;
        }
        let mut v = Vec::with_capacity(cur.len());
        let mut i = 0;
        while i < cur.len() {
            if cur[i..].starts_with(s) {
                v.extend_from_slice(b"***");
                i += s.len();
            } else {
                v.push(cur[i]);
                i += 1;
            }
        }
        out = Some(v);
    }
    match out {
        Some(v) => std::borrow::Cow::Owned(v),
        None => std::borrow::Cow::Borrowed(raw),
    }
}

impl LogState {
    fn line(&mut self, raw: &[u8], patterns: &[Regex], fail: &[String], capture: &[String]) {
        let values = std::mem::take(&mut self.redact);
        let raw = redact(raw, &values).into_owned();
        self.redact = values;
        let raw = raw.as_slice();
        // matching looks at the first MATCH_BYTES of a line; the log keeps all of it
        let text = decode(&raw[..raw.len().min(MATCH_BYTES)]);
        let clean = strip_ansi(text.trim_end_matches(['\r', '\n']));
        if self.first_error.is_none() && patterns.iter().any(|p| p.is_match(&clean)) {
            self.first_error = Some(truncate(&clean, 500));
        }
        if self.fail_hits.len() < 20 {
            for f in fail {
                if clean.contains(f.as_str()) {
                    self.fail_hits.push(truncate(&clean, 300));
                    break;
                }
            }
        }
        let trimmed = clean.trim_start();
        if capture.iter().any(|c| trimmed.starts_with(c.as_str())) && self.captured.len() < 10_000 {
            self.captured.push(trimmed.to_string());
        }
        if self.tail.len() >= 200 {
            self.tail.pop_front();
        }
        self.tail.push_back(truncate(&clean, 2000));
        let mut bytes = raw.to_vec();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        self.write(&bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        let quarter = self.cap / 4;
        if !self.truncating && self.written + bytes.len() as u64 > self.cap {
            // Keep the file's first quarter and only the last quarter in memory, seeded
            // with what is on disk after the head so the tail is continuous.
            self.truncating = true;
            self.head = self.written.min(quarter);
            let seed_from = self.head.max(self.written.saturating_sub(quarter));
            if let Some(f) = self.file.as_mut() {
                let _ = f.flush();
                if seed_from < self.written {
                    if let Ok(mut r) = std::fs::File::open(&self.path) {
                        let _ = r.seek(SeekFrom::Start(seed_from));
                        let mut buf = vec![];
                        let _ = r.take(self.written - seed_from).read_to_end(&mut buf);
                        self.ring.extend(buf);
                    }
                }
            }
        }
        if self.truncating {
            self.appended += bytes.len() as u64;
            self.ring.extend(bytes);
            while self.ring.len() as u64 > quarter {
                self.ring.pop_front();
            }
        } else if let Some(f) = self.file.as_mut() {
            let _ = f.write_all(bytes);
            self.written += bytes.len() as u64;
        }
    }

    fn finish(&mut self) -> bool {
        let Some(mut f) = self.file.take() else { return false };
        if !self.truncating {
            let _ = f.flush();
            return false;
        }
        let _ = f.flush();
        let _ = f.set_len(self.head);
        let _ = f.seek(SeekFrom::End(0));
        let omitted = (self.written + self.appended).saturating_sub(self.head + self.ring.len() as u64);
        let _ = writeln!(f, "\n[cimon] … log exceeded {} MB — {} bytes from the middle were dropped …", self.cap / (1024 * 1024), omitted);
        let ring: Vec<u8> = self.ring.drain(..).collect();
        let _ = f.write_all(&ring);
        let _ = f.flush();
        true
    }
}

pub fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").unwrap());
    re.replace_all(s, "").into_owned()
}

/// How much of one line the matchers look at.
const MATCH_BYTES: usize = 64 * 1024;
/// A "line" longer than this (progress bars, binary) is handed on in pieces, so memory
/// stays bounded and nothing is dropped.
const LINE_LIMIT: usize = 1 << 20;

/// UTF-8, or the OEM code page when the bytes are not UTF-8 (CP949 on Korean systems).
fn decode(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        // only cut short at the end (a long line cut for matching): still UTF-8
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(raw).into_owned(),
        Err(_) => {
            #[cfg(windows)]
            if let Some(s) = crate::win::decode_oem(raw) {
                return s;
            }
            String::from_utf8_lossy(raw).into_owned()
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut cut = n;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

fn pump(mut reader: impl Read + Send + 'static, state: Arc<Mutex<LogState>>, patterns: Arc<Vec<Regex>>, fail: Arc<Vec<String>>, capture: Arc<Vec<String>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = vec![0u8; 64 * 1024];
        let mut cur: Vec<u8> = Vec::with_capacity(4096);
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut rest = &chunk[..n];
                    while let Some(i) = rest.iter().position(|b| *b == b'\n') {
                        cur.extend_from_slice(&rest[..=i]);
                        state.lock().unwrap().line(&cur, &patterns, &fail, &capture);
                        cur.clear();
                        rest = &rest[i + 1..];
                    }
                    cur.extend_from_slice(rest);
                    if cur.len() >= LINE_LIMIT {
                        state.lock().unwrap().line(&cur, &patterns, &fail, &capture);
                        cur.clear();
                    }
                }
            }
        }
        if !cur.is_empty() {
            state.lock().unwrap().line(&cur, &patterns, &fail, &capture);
        }
    })
}

/// Waits for the output pumps. Once the command and its job are gone the pipes reach
/// end-of-file at once — unless a write end leaked into an unrelated process; then the
/// read is cancelled after a grace period instead of hanging the step.
fn join_pumps(pumps: Vec<std::thread::JoinHandle<()>>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pumps.iter().any(|p| !p.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // CancelSynchronousIo only hits a read in progress: keep cancelling until the
    // pumps are out (bounded), then join whatever finished
    #[cfg(windows)]
    {
        let until = Instant::now() + Duration::from_secs(5);
        while pumps.iter().any(|p| !p.is_finished()) && Instant::now() < until {
            for p in &pumps {
                if !p.is_finished() {
                    use std::os::windows::io::AsRawHandle;
                    crate::win::cancel_thread_io(p.as_raw_handle() as _);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    for p in pumps {
        if p.is_finished() {
            let _ = p.join();
        } else {
            // still blocked: leave the thread (it holds only its pipe) rather than hang
            tracing::warn!("an output reader did not stop — a pipe write end leaked into another process");
        }
    }
}

pub fn execute(spec: &ExecSpec, account: &Account, cancel: &AtomicBool) -> ExecResult {
    let started = Instant::now();
    let mut res = ExecResult::default();
    if let Some(p) = spec.log_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let file = std::fs::File::create(&spec.log_path).ok();
    let state = Arc::new(Mutex::new(LogState {
        file,
        path: spec.log_path.clone(),
        written: 0,
        cap: spec.log_cap.max(4096),
        truncating: false,
        head: 0,
        ring: VecDeque::new(),
        appended: 0,
        tail: VecDeque::new(),
        first_error: None,
        fail_hits: vec![],
        captured: vec![],
        redact: {
            let mut v: Vec<Vec<u8>> = spec.redact.iter().filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec()).collect();
            v.sort_by_key(|s| std::cmp::Reverse(s.len()));
            v
        },
    }));
    {
        let mut st = state.lock().unwrap();
        let mut head: Vec<String> = spec.preamble.iter().map(|l| format!("[cimon] {l}\n")).collect();
        head.push(format!("[cimon] $ {} {}\n[cimon] cwd {}\n", spec.program.display(), spec.args.join(" "), spec.cwd.display()));
        for l in head {
            let b = redact(l.as_bytes(), &st.redact).into_owned();
            st.write(&b);
        }
    }
    let patterns = Arc::new(spec.error_patterns.clone());
    let fail = Arc::new(spec.fail_on_output.clone());
    let capture = Arc::new(spec.capture_prefixes.clone());

    #[cfg(windows)]
    let outcome = run_windows(spec, account, cancel, &state, &patterns, &fail, &capture);
    #[cfg(not(windows))]
    let outcome = run_portable(spec, cancel, &state, &patterns, &fail, &capture);

    match outcome {
        Ok(o) => {
            res.exit_code = o.exit_code;
            res.timed_out = o.timed_out;
            res.cancelled = o.cancelled;
            res.leftover = o.leftover;
            // the helper could not start the program: that is not the program's exit code
            if let Some(e) = o.spawn_error {
                res.spawn_error = Some(e);
                res.exit_code = None;
            }
        }
        Err(e) => res.spawn_error = Some(e),
    }
    let mut st = state.lock().unwrap();
    if let Some(e) = &res.spawn_error {
        st.write(format!("[cimon] could not start: {e}\n").as_bytes());
    }
    if res.timed_out {
        st.write(format!("[cimon] time limit {} min exceeded — the job's processes were terminated\n", spec.timeout.as_secs() / 60).as_bytes());
    }
    if res.cancelled {
        st.write(b"[cimon] cancelled - the job's processes were terminated\n");
    }
    if res.leftover > 0 {
        st.write(format!("[cimon] {} process(es) were still running after the command exited and were terminated\n", res.leftover).as_bytes());
    }
    res.truncated = st.finish();
    res.first_error = st.first_error.clone();
    res.fail_hits = st.fail_hits.clone();
    res.captured = std::mem::take(&mut st.captured);
    res.tail = st.tail.iter().cloned().collect();
    res.seconds = started.elapsed().as_secs_f64();
    res
}

struct RunOutcome {
    exit_code: Option<i64>,
    timed_out: bool,
    cancelled: bool,
    leftover: u32,
    spawn_error: Option<String>,
}

#[cfg(windows)]
fn run_windows(
    spec: &ExecSpec,
    account: &Account,
    cancel: &AtomicBool,
    state: &Arc<Mutex<LogState>>,
    patterns: &Arc<Vec<Regex>>,
    fail: &Arc<Vec<String>>,
    capture: &Arc<Vec<String>>,
) -> Result<RunOutcome, String> {
    use crate::win;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    let job = win::Job::new().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + spec.timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    match account {
        Account::Current => {
            const CREATE_SUSPENDED: u32 = 0x0000_0004;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let mut child = std::process::Command::new(&spec.program)
                .args(&spec.args)
                .current_dir(&spec.cwd)
                .env_clear()
                .envs(&spec.env)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| format!("{}: {e}", spec.program.display()))?;
            if let Err(e) = job.assign(child.as_raw_handle() as _) {
                let _ = child.kill();
                return Err(e.to_string());
            }
            if let Err(e) = win::resume_process(child.id()) {
                let _ = child.kill();
                return Err(e.to_string());
            }
            let out = pump(child.stdout.take().unwrap(), state.clone(), patterns.clone(), fail.clone(), capture.clone());
            let err = pump(child.stderr.take().unwrap(), state.clone(), patterns.clone(), fail.clone(), capture.clone());
            let status = loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) => {}
                    Err(_) => break None,
                }
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    job.terminate();
                } else if Instant::now() >= deadline {
                    timed_out = true;
                    job.terminate();
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            // after a timeout or cancel the job is already being torn down
            let leftover = if timed_out || cancelled { 0 } else { job.active_processes() };
            job.terminate();
            join_pumps(vec![out, err]);
            Ok(RunOutcome { exit_code: status.and_then(|s| s.code()).map(|c| c as i64), timed_out, cancelled, leftover, spawn_error: None })
        }
        Account::Build { logon, helper } => {
            // one file per call: probes, fixtures and steps can run at the same time
            let spec_file = spec.env.get("TEMP").map(PathBuf::from).unwrap_or_else(|| spec.cwd.clone()).join(format!("cimon-exec-{}.json", cimon_core::random_id128()));
            let err_file = spec_file.with_extension("err");
            let child = ChildSpec { program: spec.program.clone(), args: spec.args.clone(), cwd: spec.cwd.clone(), env: spec.env.clone() };
            std::fs::write(&spec_file, serde_json::to_vec(&child).unwrap()).map_err(|e| format!("{}: {e}", spec_file.display()))?;
            let cmdline = format!("\"{}\" __exec \"{}\"", helper.display(), spec_file.display());
            let env: Vec<(String, String)> = spec.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let (proc_h, out_r, err_r) = match win::spawn_as_user(logon, &helper.to_string_lossy(), &cmdline, &env, &spec.cwd.to_string_lossy(), &job) {
                Ok(x) => x,
                Err(e) => {
                    let _ = std::fs::remove_file(&spec_file);
                    return Err(e.to_string());
                }
            };
            let out = pump(out_r, state.clone(), patterns.clone(), fail.clone(), capture.clone());
            let err = pump(err_r, state.clone(), patterns.clone(), fail.clone(), capture.clone());
            loop {
                if win::wait_process(proc_h.0, 100) {
                    break;
                }
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    job.terminate();
                } else if Instant::now() >= deadline {
                    timed_out = true;
                    job.terminate();
                }
            }
            let code = win::exit_code(proc_h.0);
            let leftover = if timed_out || cancelled { 0 } else { job.active_processes() };
            job.terminate();
            join_pumps(vec![out, err]);
            let spawn_error = std::fs::read_to_string(&err_file).ok().filter(|s| !s.trim().is_empty());
            let _ = std::fs::remove_file(&spec_file);
            let _ = std::fs::remove_file(&err_file);
            Ok(RunOutcome { exit_code: code, timed_out, cancelled, leftover, spawn_error })
        }
    }
}

#[cfg(not(windows))]
fn run_portable(
    spec: &ExecSpec,
    cancel: &AtomicBool,
    state: &Arc<Mutex<LogState>>,
    patterns: &Arc<Vec<Regex>>,
    fail: &Arc<Vec<String>>,
    capture: &Arc<Vec<String>>,
) -> Result<RunOutcome, String> {
    let mut child = std::process::Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let out = pump(child.stdout.take().unwrap(), state.clone(), patterns.clone(), fail.clone(), capture.clone());
    let err = pump(child.stderr.take().unwrap(), state.clone(), patterns.clone(), fail.clone(), capture.clone());
    let deadline = Instant::now() + spec.timeout;
    let (mut timed_out, mut cancelled) = (false, false);
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = child.kill();
        } else if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    join_pumps(vec![out, err]);
    Ok(RunOutcome { exit_code: status.and_then(|s| s.code()).map(|c| c as i64), timed_out, cancelled, leftover: 0, spawn_error: None })
}

/// What the `__exec` helper runs (as the build account, inside the job).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// `cimon __exec <spec>` — runs the command with inherited stdio and exits with its
/// code. std's Command handles `.bat`/`.cmd` argument escaping (design §9.3). When the
/// command cannot be started, the reason goes to `<spec>.err` so the runner reports a
/// start failure instead of an exit code.
pub fn child_exec_main(spec_path: &Path) -> i32 {
    let fail = |msg: String, code: i32| {
        eprintln!("[cimon] {msg}");
        let _ = std::fs::write(spec_path.with_extension("err"), &msg);
        code
    };
    let spec: ChildSpec = match std::fs::read(spec_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(s) => s,
        None => return fail(format!("__exec: cannot read {}", spec_path.display()), 125),
    };
    let status = std::process::Command::new(&spec.program).args(&spec.args).current_dir(&spec.cwd).env_clear().envs(&spec.env).status();
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => fail(format!("could not start {}: {e}", spec.program.display()), 126),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        for k in ["SystemRoot", "windir", "ComSpec", "PATHEXT"] {
            if let Ok(v) = std::env::var(k) {
                env.insert(k.to_string(), v);
            }
        }
        env.insert("PATH".into(), format!("{}\\System32", std::env::var("SystemRoot").unwrap_or_default()));
        env
    }

    fn spec(dir: &Path, args: &[&str], timeout: u64) -> ExecSpec {
        ExecSpec {
            program: PathBuf::from(std::env::var("ComSpec").unwrap()),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: dir.to_path_buf(),
            env: sys_env(),
            timeout: Duration::from_secs(timeout),
            log_path: dir.join("log.txt"),
            log_cap: 200 * 1024 * 1024,
            error_patterns: default_error_patterns(),
            fail_on_output: vec!["SKIP (no sibling".into()],
            capture_prefixes: vec!["test result:".into()],
            preamble: vec![],
            redact: vec![],
        }
    }

    /// A value the environment carries never reaches the log, the tail or the error
    /// line — whatever the command prints (design §11.7).
    #[test]
    fn redacted_values_never_reach_the_log() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spec(d.path(), &["/c", "echo error: login failed for s3cr3t-pass-xyz and s3cr3t&& echo pw=%PW%"], 30);
        s.env.insert("PW".into(), "s3cr3t-pass-xyz".into());
        s.redact = vec!["s3cr3t".into(), "s3cr3t-pass-xyz".into()];
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert_eq!(r.exit_code, Some(0), "{r:?}");
        let log = std::fs::read_to_string(d.path().join("log.txt")).unwrap();
        assert!(!log.contains("s3cr3t"), "{log}");
        assert!(log.contains("login failed for *** and ***"), "longest first, then the shorter: {log}");
        assert!(log.contains("pw=***"), "{log}");
        assert_eq!(r.first_error.as_deref(), Some("error: login failed for *** and ***"));
        assert!(r.tail.iter().all(|l| !l.contains("s3cr3t")), "{:?}", r.tail);
    }

    #[test]
    fn exit_code_errors_and_captures() {
        let d = tempfile::tempdir().unwrap();
        let s = spec(d.path(), &["/c", "echo test result: ok. 3 passed; 0 failed&& echo error: boom&& echo SKIP (no sibling x&& exit 3"], 30);
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert_eq!(r.exit_code, Some(3), "{r:?}");
        assert_eq!(r.first_error.as_deref(), Some("error: boom"));
        assert_eq!(r.fail_hits.len(), 1);
        assert_eq!(r.captured, vec!["test result: ok. 3 passed; 0 failed"]);
        let log = std::fs::read_to_string(d.path().join("log.txt")).unwrap();
        assert!(log.contains("error: boom"));
    }

    #[test]
    fn timeout_kills_tree() {
        let d = tempfile::tempdir().unwrap();
        // ping holds the console ~10s; a grandchild keeps running too
        let s = spec(d.path(), &["/c", "start /b ping -n 30 127.0.0.1 >NUL & ping -n 30 127.0.0.1 >NUL"], 2);
        let t = Instant::now();
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert!(r.timed_out, "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn cancel_flag() {
        let d = tempfile::tempdir().unwrap();
        let s = spec(d.path(), &["/c", "ping -n 30 127.0.0.1 >NUL"], 60);
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            f2.store(true, Ordering::SeqCst);
        });
        let r = execute(&s, &Account::Current, &flag);
        assert!(r.cancelled);
    }

    #[test]
    fn leftover_processes_terminated() {
        let d = tempfile::tempdir().unwrap();
        let s = spec(d.path(), &["/c", "start /b ping -n 30 127.0.0.1 >NUL & exit 0"], 60);
        let t = Instant::now();
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert_eq!(r.exit_code, Some(0));
        assert!(r.leftover >= 1, "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn log_cap_keeps_head_and_tail() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spec(d.path(), &["/c", "for /L %i in (1,1,3000) do @echo line-%i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"], 60);
        s.log_cap = 40_000;
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert!(r.truncated);
        let log = std::fs::read_to_string(d.path().join("log.txt")).unwrap();
        assert!(log.len() < 25_000, "{}", log.len());
        assert!(log.contains("line-1-"));
        assert!(log.contains("line-3000-"));
        assert!(log.contains("bytes from the middle were dropped"));
    }

    #[test]
    fn log_cap_with_a_huge_first_line_and_no_newlines() {
        let d = tempfile::tempdir().unwrap();
        let mut st = LogState {
            file: std::fs::File::create(d.path().join("l.txt")).ok(),
            path: d.path().join("l.txt"),
            written: 0,
            cap: 4000,
            truncating: false,
            head: 0,
            ring: VecDeque::new(),
            appended: 0,
            tail: VecDeque::new(),
            first_error: None,
            fail_hits: vec![],
            captured: vec![],
            redact: vec![],
        };
        st.write(b"start\n");
        st.write(&vec![b'x'; 10_000]);
        st.write(b"\nend\n");
        assert!(st.finish());
        let log = std::fs::read(d.path().join("l.txt")).unwrap();
        assert!(!log.contains(&0u8), "no NUL padding");
        assert!(log.starts_with(b"start\n"));
        assert!(log.ends_with(b"end\n"));
        let text = String::from_utf8_lossy(&log);
        // 6 + 10000 + 5 bytes in, 6 kept at the head, 1000 in the tail
        assert!(text.contains(" 9005 bytes from the middle"), "{text}");
    }

    #[test]
    fn output_without_newlines_is_kept() {
        let d = tempfile::tempdir().unwrap();
        // ~1.2 MB of dots without a newline, then a marker
        let s = spec(d.path(), &["/c", "powershell -NoProfile -Command \"[Console]::Out.Write('.' * 1200000); [Console]::Out.Write('MARK')\""], 60);
        let mut s = s;
        s.env.insert("PATH".into(), format!("{}\\System32;{}\\System32\\WindowsPowerShell\\v1.0", std::env::var("SystemRoot").unwrap(), std::env::var("SystemRoot").unwrap()));
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert_eq!(r.exit_code, Some(0), "{r:?}");
        let log = std::fs::read_to_string(d.path().join("log.txt")).unwrap();
        assert!(log.contains("MARK"), "tail of a newline-less stream is not dropped");
    }

    #[test]
    fn missing_program_is_spawn_error() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spec(d.path(), &[], 5);
        s.program = d.path().join("nope.exe");
        let r = execute(&s, &Account::Current, &AtomicBool::new(false));
        assert!(r.spawn_error.is_some());
        assert_eq!(r.exit_code, None);
    }
}
