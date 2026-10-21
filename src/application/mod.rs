//! Application commands. The coordinator is the only caller allowed to acquire capacity.
pub mod start;

use crate::{
    domain::{IssueSnapshot, Step},
    storage::{Entry, Result, Store},
};

/// Owns capacity changes for slot-consuming steps. Step handlers never reserve slots directly.
#[derive(Clone)]
pub struct Coordinator {
    store: Store,
}

impl Coordinator {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Enter `working` after every start-eligibility gate has passed. The persisted concurrency
    /// limit is read inside the same database transaction that acquires the slot.
    pub async fn enter_working(
        &self,
        execution_id: i64,
        issue: IssueSnapshot,
        parent: Option<IssueSnapshot>,
    ) -> Result<Entry> {
        debug_assert!(Step::Working.policy().consumes_execution_slot);
        self.store
            .enter_working(execution_id, issue, parent, None)
            .await
    }

    /// Explicit owner **Retry working**: requires the previous process to be confirmed stopped.
    pub async fn retry_working(&self, execution_id: i64) -> Result<Entry> {
        self.store.retry_working(execution_id, None).await
    }
}

/// Saves preparation evidence and output on the execution's storage record.
pub struct ExecutionRecorder {
    pub store: Store,
    pub execution_id: i64,
}

impl crate::git::worktree::Recorder for ExecutionRecorder {
    fn evidence(
        &self,
        evidence: crate::domain::WorkingEvidence,
    ) -> crate::github::cli::BoxFuture<'_, std::result::Result<(), String>> {
        Box::pin(async move {
            self.store
                .record_working(self.execution_id, evidence)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn output(
        &self,
        source: crate::domain::OutputSource,
        text: String,
    ) -> crate::github::cli::BoxFuture<'_, std::result::Result<(), String>> {
        Box::pin(async move {
            self.store
                .append_output(self.execution_id, source, text)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}
