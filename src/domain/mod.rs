//! Local records and the typed execution workflow. No GPUI, GitHub, or Codex types live here.
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

/// Identity of a candidate issue, known before any snapshot is saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueIdentity {
    pub id: String,
    pub number: u32,
}

/// Fixed orchestration metadata for a workflow step. Users and the database cannot change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepPolicy {
    pub consumes_execution_slot: bool,
}

/// One persisted position in the monotonic execution lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    Checking,
    Working,
    BranchPublished,
    PullRequestPublished,
}

impl Step {
    pub const ALL: [Step; 4] = [
        Step::Checking,
        Step::Working,
        Step::BranchPublished,
        Step::PullRequestPublished,
    ];

    /// Every step must declare its policy here; the orchestrator reads nothing else.
    pub const fn policy(self) -> StepPolicy {
        match self {
            Step::Checking => StepPolicy {
                consumes_execution_slot: false,
            },
            Step::Working => StepPolicy {
                consumes_execution_slot: true,
            },
            Step::BranchPublished => StepPolicy {
                consumes_execution_slot: false,
            },
            Step::PullRequestPublished => StepPolicy {
                consumes_execution_slot: false,
            },
        }
    }

    pub const fn next(self) -> Option<Step> {
        match self {
            Step::Checking => Some(Step::Working),
            Step::Working => Some(Step::BranchPublished),
            Step::BranchPublished => Some(Step::PullRequestPublished),
            Step::PullRequestPublished => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Step::Checking => "checking",
            Step::Working => "working",
            Step::BranchPublished => "branch_published",
            Step::PullRequestPublished => "pull_request_published",
        }
    }

    pub fn parse(value: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|step| step.as_str() == value)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Step::Checking => "Checking",
            Step::Working => "Working",
            Step::BranchPublished => "Branch published",
            Step::PullRequestPublished => "Pull request published",
        }
    }

    pub fn slot_consuming() -> impl Iterator<Item = Step> {
        Step::ALL
            .into_iter()
            .filter(|step| step.policy().consumes_execution_slot)
    }
}

/// Activity at the current step. `Ended` is terminal and always carries an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for the orchestrator: capacity, the next transition, or publication.
    Pending,
    /// A transition or the step's local work is in progress.
    Running,
    /// The last attempt failed; the error is stored on the execution.
    Error,
    /// Local work was cut short by quitting or lost contact.
    Interrupted,
    Ended,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Running => "running",
            Status::Error => "error",
            Status::Interrupted => "interrupted",
            Status::Ended => "ended",
        }
    }

    pub fn parse(value: &str) -> Option<Status> {
        [
            Status::Pending,
            Status::Running,
            Status::Error,
            Status::Interrupted,
            Status::Ended,
        ]
        .into_iter()
        .find(|status| status.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    PullRequestPublished,
    /// The issue was closed when local work finished. The local result is retained.
    PublicationSkipped,
    /// Schema 1 history that finished before the workflow existed.
    Legacy,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::PullRequestPublished => "pull_request_published",
            Outcome::PublicationSkipped => "publication_skipped",
            Outcome::Legacy => "legacy",
        }
    }

    pub fn parse(value: &str) -> Option<Outcome> {
        [
            Outcome::PullRequestPublished,
            Outcome::PublicationSkipped,
            Outcome::Legacy,
        ]
        .into_iter()
        .find(|outcome| outcome.as_str() == value)
    }
}

/// A recoverable failure attached to the current step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepError {
    /// The step the failed transition was trying to reach or complete.
    pub transition: Step,
    pub message: String,
    pub detail: Option<String>,
    pub retryable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingEvidence {
    pub default_branch: Option<String>,
    pub base_commit: Option<String>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub codex_thread_id: Option<String>,
    pub codex_turn_id: Option<String>,
    pub work_finished_at: Option<String>,
    pub work_result: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublicationEvidence {
    pub pushed_remote: Option<String>,
    pub pushed_ref: Option<String>,
    pub pushed_commit: Option<String>,
    pub pull_request_number: Option<u32>,
    pub pull_request_url: Option<String>,
    pub pull_request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub id: i64,
    pub project_id: i64,
    pub github_host: String,
    pub issue: IssueIdentity,
    pub run_owner: String,
    /// Saved once on entry to `working`.
    pub snapshot: Option<IssueSnapshot>,
    pub parent: Option<IssueSnapshot>,
    pub step: Step,
    pub status: Status,
    pub outcome: Option<Outcome>,
    pub error: Option<StepError>,
    pub working: WorkingEvidence,
    pub publication: PublicationEvidence,
    /// The unreleased slot attempt, if this execution currently holds capacity.
    pub slot_attempt: Option<SlotAttempt>,
    pub legacy: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Execution {
    pub fn holds_slot(&self) -> bool {
        self.slot_attempt.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotAttempt {
    pub number: i64,
    pub step: Step,
    pub process_id: Option<u32>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEvent {
    pub sequence: i64,
    pub step: String,
    pub status: String,
    pub reason: String,
    pub detail: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputSource {
    System,
    Git,
    Setup,
    Agent,
    Stderr,
}

impl OutputSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutputSource::System => "system",
            OutputSource::Git => "git",
            OutputSource::Setup => "setup",
            OutputSource::Agent => "agent",
            OutputSource::Stderr => "stderr",
        }
    }

    pub fn parse(value: &str) -> Option<OutputSource> {
        [
            OutputSource::System,
            OutputSource::Git,
            OutputSource::Setup,
            OutputSource::Agent,
            OutputSource::Stderr,
        ]
        .into_iter()
        .find(|source| source.as_str() == value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOutput {
    pub sequence: i64,
    pub attempt: Option<i64>,
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

/// Bounds for the persisted concurrency limit (decision D5).
pub const DEFAULT_CONCURRENCY_LIMIT: u32 = 2;
pub const MAX_CONCURRENCY_LIMIT: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_ordered_and_round_trip() {
        let mut step = Step::Checking;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            assert!(next > step);
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, Step::ALL);
        for step in Step::ALL {
            assert_eq!(Step::parse(step.as_str()), Some(step));
        }
        assert_eq!(Step::parse("failed"), None);
    }

    #[test]
    fn only_working_consumes_a_slot() {
        assert_eq!(Step::slot_consuming().collect::<Vec<_>>(), [Step::Working]);
    }
}
