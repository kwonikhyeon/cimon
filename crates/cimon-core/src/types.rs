use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "inconclusive",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Verdict::Pass),
            "fail" => Some(Verdict::Fail),
            "inconclusive" => Some(Verdict::Inconclusive),
            _ => None,
        }
    }
    /// Severity used to fold step verdicts into a job verdict: fail > inconclusive > pass.
    pub fn severity(&self) -> u8 {
        match self {
            Verdict::Pass => 0,
            Verdict::Inconclusive => 1,
            Verdict::Fail => 2,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whose problem a non-pass verdict is (design §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Code,
    Recipe,
    Request,
    Environment,
    Infra,
    Unobserved,
}

impl FailureClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureClass::Code => "code",
            FailureClass::Recipe => "recipe",
            FailureClass::Request => "request",
            FailureClass::Environment => "environment",
            FailureClass::Infra => "infra",
            FailureClass::Unobserved => "unobserved",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "code" => Some(FailureClass::Code),
            "recipe" => Some(FailureClass::Recipe),
            "request" => Some(FailureClass::Request),
            "environment" => Some(FailureClass::Environment),
            "infra" => Some(FailureClass::Infra),
            "unobserved" => Some(FailureClass::Unobserved),
            _ => None,
        }
    }
    /// Allowed (verdict, class) pairs — design §8.2.
    pub fn allowed_with(&self, verdict: Verdict) -> bool {
        match (verdict, self) {
            (Verdict::Fail, FailureClass::Code | FailureClass::Recipe | FailureClass::Request) => true,
            (
                Verdict::Inconclusive,
                FailureClass::Recipe | FailureClass::Environment | FailureClass::Infra | FailureClass::Unobserved,
            ) => true,
            _ => false,
        }
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A verdict with its failure class. `Pass` carries no class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub verdict: Verdict,
    pub class: Option<FailureClass>,
    pub reason: String,
}

impl Outcome {
    pub fn pass(reason: impl Into<String>) -> Self {
        Outcome { verdict: Verdict::Pass, class: None, reason: reason.into() }
    }
    pub fn fail(class: FailureClass, reason: impl Into<String>) -> Self {
        debug_assert!(class.allowed_with(Verdict::Fail));
        Outcome { verdict: Verdict::Fail, class: Some(class), reason: reason.into() }
    }
    pub fn inconclusive(class: FailureClass, reason: impl Into<String>) -> Self {
        debug_assert!(class.allowed_with(Verdict::Inconclusive));
        Outcome { verdict: Verdict::Inconclusive, class: Some(class), reason: reason.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Intake,
    Provisioning,
    Ready,
    Running,
    Collecting,
    Cancelling,
    Done,
    Merged,
    Cancelled,
    QueueTimeout,
    Interrupted,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Intake => "intake",
            JobState::Provisioning => "provisioning",
            JobState::Ready => "ready",
            JobState::Running => "running",
            JobState::Collecting => "collecting",
            JobState::Cancelling => "cancelling",
            JobState::Done => "done",
            JobState::Merged => "merged",
            JobState::Cancelled => "cancelled",
            JobState::QueueTimeout => "queue_timeout",
            JobState::Interrupted => "interrupted",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "intake" => JobState::Intake,
            "provisioning" => JobState::Provisioning,
            "ready" => JobState::Ready,
            "running" => JobState::Running,
            "collecting" => JobState::Collecting,
            "cancelling" => JobState::Cancelling,
            "done" => JobState::Done,
            "merged" => JobState::Merged,
            "cancelled" => JobState::Cancelled,
            "queue_timeout" => JobState::QueueTimeout,
            "interrupted" => JobState::Interrupted,
            _ => return None,
        })
    }
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Done | JobState::Merged | JobState::Cancelled | JobState::QueueTimeout | JobState::Interrupted)
    }
    /// Before the runner picks it up.
    pub fn is_pre_start(&self) -> bool {
        matches!(self, JobState::Intake | JobState::Provisioning | JobState::Ready)
    }
}

/// Which MCP tool created a job; decides which step kinds `steps`/`tags` may pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Build,
    Test,
    Run,
}

impl Tool {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tool::Build => "build",
            Tool::Test => "test",
            Tool::Run => "run",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "build" => Some(Tool::Build),
            "test" => Some(Tool::Test),
            "run" => Some(Tool::Run),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Member,
    Admin,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// An authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub name: String,
    pub role: Role,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

/// A non-fatal note attached to a job result (design §7.4 `warnings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub source: String,
    pub message: String,
}

impl Warning {
    pub fn new(source: impl Into<String>, message: impl Into<String>) -> Self {
        Warning { source: source.into(), message: message.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_pairs() {
        assert!(FailureClass::Code.allowed_with(Verdict::Fail));
        assert!(!FailureClass::Code.allowed_with(Verdict::Inconclusive));
        assert!(FailureClass::Recipe.allowed_with(Verdict::Fail));
        assert!(FailureClass::Recipe.allowed_with(Verdict::Inconclusive));
        assert!(!FailureClass::Infra.allowed_with(Verdict::Fail));
        assert!(!FailureClass::Request.allowed_with(Verdict::Inconclusive));
    }
}
