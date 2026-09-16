//! Local records. Eligibility and successful completion are separate, unfinished policies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: i64,
    pub checkout_path: String,
    pub github_host: String,
    pub repository_id: String,
    pub owner: String,
    pub name: String,
    pub remote: String,
    pub setup_command: Option<String>,
}

/// Repository identity must be verified by the Git/GitHub adapter before insertion.
#[derive(Clone, Debug)]
pub struct NewProject {
    pub checkout_path: String,
    pub github_host: String,
    pub repository_id: String,
    pub owner: String,
    pub name: String,
    pub remote: String,
    pub setup_command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueSnapshot {
    pub id: String,
    pub number: u32,
    pub title: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct NewExecution {
    pub project_id: i64,
    pub issue: IssueSnapshot,
    pub parent: Option<IssueSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionStatus {
    Preparing,
    Running,
    Interrupted,
    Failed,
}

impl ExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

/// Terminal outcomes are distinct from confirmation that owned work stopped.
#[derive(Clone, Copy, Debug)]
pub enum ExecutionChange {
    Started,
    Interrupted,
    FailedAndStopped,
    TerminationConfirmed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub id: i64,
    pub project_id: i64,
    pub issue: IssueSnapshot,
    pub parent: Option<IssueSnapshot>,
    pub status: ExecutionStatus,
    pub termination_confirmed: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEvent {
    pub sequence: i64,
    pub previous_status: Option<String>,
    pub status: String,
    pub termination_confirmed: bool,
    pub reason: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOutput {
    pub sequence: i64,
    pub source: String,
    pub payload: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HistorySummary {
    pub projects: i64,
    pub executions: i64,
    pub reserved_slots: i64,
}
