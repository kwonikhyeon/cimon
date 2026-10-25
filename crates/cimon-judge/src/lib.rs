//! Judges (design §9.6, recipe-spec §12) and artifact inspections. Nothing here counts
//! a step as passed unless it saw evidence: zero tests, missing reports, a broken judge
//! script all come out as `fail` or `inconclusive`.

pub mod inspect;

use cimon_core::{FailureClass, Outcome, Verdict};
use cimon_runner::ExecResult;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepJudgement {
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<serde_json::Value>,
}

impl StepJudgement {
    fn of(outcome: Outcome) -> Self {
        StepJudgement { outcome, tests: None, metrics: None }
    }
}

/// What every judge checks first: the command ran, finished in time, printed none of
/// the `fail_on_output` strings.
pub fn precheck(exec: &ExecResult, timeout_min: u32) -> Option<Outcome> {
    if let Some(e) = &exec.spawn_error {
        return Some(Outcome::inconclusive(FailureClass::Infra, format!("could not start the command: {e}")));
    }
    if exec.timed_out {
        return Some(Outcome::fail(FailureClass::Code, format!("time limit {timeout_min} min exceeded")));
    }
    if let Some(hit) = exec.fail_hits.first() {
        return Some(Outcome::fail(FailureClass::Code, format!("forbidden output (fail_on_output): {hit}")));
    }
    None
}

pub fn judge_exit(exec: &ExecResult) -> StepJudgement {
    match exec.exit_code {
        Some(0) => StepJudgement::of(Outcome::pass("exit code 0")),
        Some(c) => StepJudgement::of(Outcome::fail(FailureClass::Code, format!("exit code {c}{}", first_error(exec)))),
        None => StepJudgement::of(Outcome::inconclusive(FailureClass::Infra, "no exit code")),
    }
}

fn first_error(exec: &ExecResult) -> String {
    exec.first_error.as_ref().map(|e| format!(" — {e}")).unwrap_or_default()
}

/// Sums `test result: … N passed; M failed; K ignored` lines.
pub fn cargo_counts(lines: &[String]) -> Option<TestCounts> {
    let re = Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored").unwrap();
    let mut t = TestCounts::default();
    let mut any = false;
    for l in lines {
        if let Some(c) = re.captures(l) {
            any = true;
            t.passed += c[1].parse::<u64>().unwrap_or(0);
            t.failed += c[2].parse::<u64>().unwrap_or(0);
            t.skipped += c[3].parse::<u64>().unwrap_or(0);
        }
    }
    any.then_some(t)
}

pub fn judge_cargo(exec: &ExecResult, min_tests: u32) -> StepJudgement {
    let counts = cargo_counts(&exec.captured);
    let mut j = match exec.exit_code {
        Some(0) => match &counts {
            None => StepJudgement::of(Outcome::fail(FailureClass::Code, "no `test result:` line — no tests ran")),
            Some(t) if t.failed > 0 => StepJudgement::of(Outcome::fail(FailureClass::Code, format!("{} test(s) failed", t.failed))),
            Some(t) if t.passed < min_tests as u64 => StepJudgement::of(Outcome::fail(
                FailureClass::Code,
                format!("{} test(s) passed — fewer than min_tests = {min_tests}", t.passed),
            )),
            Some(t) => StepJudgement::of(Outcome::pass(format!("{} passed, {} ignored", t.passed, t.skipped))),
        },
        Some(c) => StepJudgement::of(Outcome::fail(
            FailureClass::Code,
            format!("exit code {c}{}{}", counts.as_ref().map(|t| format!(" ({} failed)", t.failed)).unwrap_or_default(), first_error(exec)),
        )),
        None => StepJudgement::of(Outcome::inconclusive(FailureClass::Infra, "no exit code")),
    };
    j.tests = counts;
    j
}

/// Counts JUnit `<testcase>` elements in the given files.
pub fn junit_counts(files: &[std::path::PathBuf]) -> Result<TestCounts, String> {
    use quick_xml::events::Event;
    let mut t = TestCounts::default();
    for f in files {
        let mut reader = quick_xml::Reader::from_file(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut buf = vec![];
        let mut in_case = false;
        let mut case_failed = false;
        let mut case_skipped = false;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.local_name().as_ref() {
                    b"testcase" => {
                        in_case = true;
                        case_failed = false;
                        case_skipped = false;
                    }
                    b"failure" | b"error" if in_case => case_failed = true,
                    b"skipped" if in_case => case_skipped = true,
                    _ => {}
                },
                Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                    b"testcase" => t.passed += 1,
                    b"failure" | b"error" if in_case => case_failed = true,
                    b"skipped" if in_case => case_skipped = true,
                    _ => {}
                },
                Ok(Event::End(e)) if e.local_name().as_ref() == b"testcase" => {
                    in_case = false;
                    if case_failed {
                        t.failed += 1;
                    } else if case_skipped {
                        t.skipped += 1;
                    } else {
                        t.passed += 1;
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("{}: {e}", f.display())),
                _ => {}
            }
            buf.clear();
        }
    }
    Ok(t)
}

/// Report files under `ws` matching `globs`.
pub fn find_files(ws: &Path, globs: &[String]) -> Vec<std::path::PathBuf> {
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = globset::GlobBuilder::new(&g.replace('\\', "/")).literal_separator(true).build() {
            b.add(glob);
        }
    }
    let Ok(set) = b.build() else { return vec![] };
    let mut out = vec![];
    for e in walkdir::WalkDir::new(ws).into_iter().filter_entry(|e| e.file_name() != ".git").flatten() {
        if e.file_type().is_file() {
            if let Ok(rel) = e.path().strip_prefix(ws) {
                if set.is_match(rel.to_string_lossy().replace('\\', "/")) {
                    out.push(e.path().to_path_buf());
                }
            }
        }
    }
    out.sort();
    out
}

pub fn judge_junit(exec: &ExecResult, ws: &Path, reports: &[String], min_tests: u32) -> StepJudgement {
    let files = find_files(ws, reports);
    let counts = if files.is_empty() { None } else { junit_counts(&files).ok() };
    let mut j = match exec.exit_code {
        Some(0) => {
            if files.is_empty() {
                StepJudgement::of(Outcome::fail(
                    FailureClass::Code,
                    format!("no JUnit report matched {} — no tests ran (or reports goes somewhere else)", reports.join(", ")),
                ))
            } else {
                match junit_counts(&files) {
                    Err(e) => StepJudgement::of(Outcome::inconclusive(FailureClass::Recipe, format!("unreadable JUnit report: {e}"))),
                    Ok(t) if t.failed > 0 => StepJudgement::of(Outcome::fail(FailureClass::Code, format!("{} test(s) failed", t.failed))),
                    Ok(t) if t.passed < min_tests as u64 => StepJudgement::of(Outcome::fail(
                        FailureClass::Code,
                        format!("{} test(s) passed in {} report(s) — fewer than min_tests = {min_tests}", t.passed, files.len()),
                    )),
                    Ok(t) => StepJudgement::of(Outcome::pass(format!("{} passed, {} skipped in {} report(s)", t.passed, t.skipped, files.len()))),
                }
            }
        }
        Some(c) => StepJudgement::of(Outcome::fail(
            FailureClass::Code,
            format!("exit code {c}{}{}", counts.as_ref().map(|t| format!(" ({} failed)", t.failed)).unwrap_or_default(), first_error(exec)),
        )),
        None => StepJudgement::of(Outcome::inconclusive(FailureClass::Infra, "no exit code")),
    };
    j.tests = counts;
    j
}

/// Output contract of a judge script (recipe-spec §12).
#[derive(Debug, Deserialize)]
struct ScriptVerdict {
    verdict: String,
    #[serde(default)]
    failure_class: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    metrics: Option<serde_json::Value>,
}

/// Interprets a judge script run. Anything off-contract is `inconclusive`/`recipe`.
pub fn judge_script_output(script: &ExecResult, stdout_lines: &[String]) -> StepJudgement {
    let broken = |why: String| StepJudgement::of(Outcome::inconclusive(FailureClass::Recipe, format!("judge script: {why}")));
    if let Some(e) = &script.spawn_error {
        return broken(format!("could not start: {e}"));
    }
    if script.timed_out {
        return broken("timed out".into());
    }
    if script.exit_code != Some(0) {
        return broken(format!("exited with {:?}{}", script.exit_code, first_error(script)));
    }
    let parsed = stdout_lines.iter().rev().find_map(|l| serde_json::from_str::<ScriptVerdict>(l.trim()).ok());
    let Some(v) = parsed else {
        return broken("no JSON verdict on stdout".into());
    };
    let Some(verdict) = Verdict::parse(&v.verdict) else {
        return broken(format!("unknown verdict {:?}", v.verdict));
    };
    let reason = v.reason.unwrap_or_default();
    let outcome = match verdict {
        Verdict::Pass => Outcome::pass(if reason.is_empty() { "judge script: pass".into() } else { reason }),
        _ => {
            let Some(class) = v.failure_class.as_deref().and_then(FailureClass::parse) else {
                return broken(format!("verdict {} needs a valid failure_class", verdict.as_str()));
            };
            if !class.allowed_with(verdict) || class == FailureClass::Request {
                return broken(format!("({}, {}) is not an allowed pair", verdict.as_str(), class.as_str()));
            }
            Outcome { verdict, class: Some(class), reason }
        }
    };
    StepJudgement { outcome, tests: None, metrics: v.metrics }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(code: i64, captured: &[&str]) -> ExecResult {
        ExecResult { exit_code: Some(code), captured: captured.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn cargo_zero_tests_fails() {
        let j = judge_cargo(&exec(0, &["test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured"]), 1);
        assert_eq!(j.outcome.verdict, Verdict::Fail);
        let j = judge_cargo(&exec(0, &[]), 1);
        assert_eq!(j.outcome.verdict, Verdict::Fail);
        let j = judge_cargo(&exec(0, &["test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured", "test result: ok. 2 passed; 0 failed; 0 ignored"]), 1);
        assert_eq!(j.outcome.verdict, Verdict::Pass);
        assert_eq!(j.tests.unwrap().passed, 5);
    }

    #[test]
    fn junit_counts_cases() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("r")).unwrap();
        std::fs::write(
            d.path().join("r/TEST-a.xml"),
            r#"<testsuites><testsuite name="a" tests="3"><testcase name="x"/><testcase name="y"><failure message="no"/></testcase><testcase name="z"><skipped/></testcase></testsuite></testsuites>"#,
        )
        .unwrap();
        let j = judge_junit(&exec(0, &[]), d.path(), &["r/*.xml".into()], 1);
        assert_eq!(j.outcome.verdict, Verdict::Fail);
        assert_eq!(j.tests.unwrap(), TestCounts { passed: 1, failed: 1, skipped: 1 });
        let j = judge_junit(&exec(0, &[]), d.path(), &["nothing/*.xml".into()], 1);
        assert_eq!(j.outcome.verdict, Verdict::Fail);
        assert!(j.outcome.reason.contains("no JUnit report"));
    }

    #[test]
    fn script_contract() {
        let ok = exec(0, &[]);
        let j = judge_script_output(&ok, &[r#"{"verdict":"fail","failure_class":"code","reason":"0.004 m","metrics":{"travel":0.004}}"#.into()]);
        assert_eq!(j.outcome.verdict, Verdict::Fail);
        assert_eq!(j.metrics.unwrap()["travel"], 0.004);
        let j = judge_script_output(&ok, &[r#"{"verdict":"fail","failure_class":"infra"}"#.into()]);
        assert_eq!((j.outcome.verdict, j.outcome.class), (Verdict::Inconclusive, Some(FailureClass::Recipe)));
        let j = judge_script_output(&ok, &["not json".into()]);
        assert_eq!(j.outcome.class, Some(FailureClass::Recipe));
        let j = judge_script_output(&exec(2, &[]), &[r#"{"verdict":"pass"}"#.into()]);
        assert_eq!(j.outcome.verdict, Verdict::Inconclusive);
        let j = judge_script_output(&ok, &[r#"{"verdict":"pass","reason":"ok"}"#.into()]);
        assert_eq!(j.outcome.verdict, Verdict::Pass);
    }

    #[test]
    fn precheck_order() {
        let mut e = exec(0, &[]);
        e.fail_hits = vec!["SKIP (no sibling".into()];
        assert_eq!(precheck(&e, 10).unwrap().verdict, Verdict::Fail);
        e.timed_out = true;
        assert!(precheck(&e, 10).unwrap().reason.contains("time limit"));
    }
}
