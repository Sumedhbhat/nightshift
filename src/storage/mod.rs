//! One database connection on a dedicated worker. No SQLite runs on the GPUI thread.
mod database;
#[cfg(test)]
mod tests;

use crate::domain::*;
use database::Database;
pub use database::{AttemptEnd, Entry};
use std::{fs::File, path::PathBuf};
use tokio::sync::{mpsc, oneshot};

pub const MAX_OUTPUT_BYTES: usize = 65_536;
pub const MAX_OUTPUT_BATCH: usize = 4_096;
const MAX_SNAPSHOT_BYTES: usize = 1_048_576;
const MAX_REASON_BYTES: usize = 65_536;
const QUEUE_CAPACITY: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Nightshift already has this data directory open")]
    AlreadyOpen,
    #[error("database worker stopped")]
    WorkerStopped,
    #[error("database schema {0} is newer than this app supports")]
    NewerSchema(i64),
    #[error("this issue is already being checked")]
    DuplicateIssue,
    #[error("this issue already has an execution; use its retry actions instead")]
    AlreadyStarted,
    #[error("the previous owned process has not been confirmed stopped")]
    ProcessNotStopped,
    #[error("recorded {0} cannot be replaced")]
    EvidenceConflict(&'static str),
    #[error("project or execution not found")]
    NotFound,
    #[error("invalid execution state change")]
    InvalidTransition,
    #[error("invalid input: {0}")]
    InvalidInput(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
type Job = Box<dyn FnOnce(&mut Database) + Send>;

enum Message {
    Call(Job),
    Close(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct Store {
    sender: mpsc::Sender<Message>,
}

fn check_text(value: &str, limit: usize, message: &'static str) -> Result<()> {
    if value.is_empty() || value.len() > limit {
        return Err(Error::InvalidInput(message));
    }
    Ok(())
}

fn check_error(error: &StepError) -> Result<()> {
    check_text(
        &error.message,
        MAX_REASON_BYTES,
        "error message must be 1..65536 bytes",
    )?;
    if error
        .detail
        .as_ref()
        .is_some_and(|d| d.len() > MAX_REASON_BYTES)
    {
        return Err(Error::InvalidInput("error detail exceeds 65536 bytes"));
    }
    Ok(())
}

fn check_snapshot(issue: &IssueSnapshot) -> Result<()> {
    if issue.id.is_empty() || issue.number == 0 {
        return Err(Error::InvalidInput(
            "issue identity and number are required",
        ));
    }
    if issue.description.len() + issue.title.len() + issue.id.len() > MAX_SNAPSHOT_BYTES {
        return Err(Error::InvalidInput("issue snapshot exceeds 1 MiB"));
    }
    Ok(())
}

impl Store {
    pub async fn open(directory: PathBuf) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Message>(QUEUE_CAPACITY);
        let (ready, opened) = oneshot::channel();
        std::thread::Builder::new()
            .name("nightshift-database".into())
            .spawn(move || {
                let opened_database = (|| {
                    std::fs::create_dir_all(&directory)?;
                    let lock = File::options()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(directory.join("nightshift.lock"))?;
                    lock.try_lock().map_err(|error| match error {
                        std::fs::TryLockError::WouldBlock => Error::AlreadyOpen,
                        std::fs::TryLockError::Error(error) => Error::Io(error),
                    })?;
                    let database = Database::open(&directory.join("nightshift.sqlite3"))?;
                    Ok::<_, Error>((database, lock))
                })();
                match opened_database {
                    Ok((mut database, _lock)) => {
                        if ready.send(Ok(())).is_err() {
                            return;
                        }
                        while let Some(message) = receiver.blocking_recv() {
                            match message {
                                Message::Call(job) => job(&mut database),
                                Message::Close(closed) => {
                                    drop(database);
                                    drop(_lock);
                                    let _ = closed.send(());
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })?;
        opened.await.map_err(|_| Error::WorkerStopped)??;
        Ok(Self { sender })
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Message::Call(Box::new(move |database| {
                let _ = sender.send(operation(database));
            })))
            .await
            .map_err(|_| Error::WorkerStopped)?;
        receiver.await.map_err(|_| Error::WorkerStopped)?
    }

    /// Drain preceding commands and close the database for all clones of this handle.
    pub async fn close(self) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Message::Close(sender))
            .await
            .map_err(|_| Error::WorkerStopped)?;
        receiver.await.map_err(|_| Error::WorkerStopped)
    }

    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.call(Database::projects).await
    }

    pub async fn summary(&self) -> Result<HistorySummary> {
        self.call(Database::summary).await
    }

    pub async fn add_project(&self, project: NewProject) -> Result<Project> {
        if !std::path::Path::new(&project.checkout_path).is_absolute()
            || project.github_host.is_empty()
            || project.repository_id.is_empty()
            || project.owner.is_empty()
            || project.name.is_empty()
            || project.remote.is_empty()
        {
            return Err(Error::InvalidInput(
                "project needs an absolute checkout path and verified GitHub identity",
            ));
        }
        self.call(move |database| database.add_project(project))
            .await
    }

    /// Optional setup command run in each new worktree before Codex starts.
    pub async fn set_setup_command(&self, project_id: i64, command: Option<String>) -> Result<()> {
        let command = command.filter(|c| !c.trim().is_empty());
        if command.as_ref().is_some_and(|c| c.len() > 4_096) {
            return Err(Error::InvalidInput("setup command exceeds 4096 bytes"));
        }
        self.call(move |database| database.set_setup_command(project_id, command))
            .await
    }

    pub async fn concurrency_limit(&self) -> Result<u32> {
        self.call(Database::concurrency_limit).await
    }

    pub async fn set_concurrency_limit(&self, limit: u32) -> Result<()> {
        self.call(move |database| database.set_concurrency_limit(limit))
            .await
    }

    pub async fn begin_checking(
        &self,
        project_id: i64,
        issue: IssueIdentity,
        run_owner: String,
    ) -> Result<Execution> {
        if issue.id.is_empty() || issue.number == 0 || run_owner.is_empty() {
            return Err(Error::InvalidInput(
                "issue identity, number, and run owner are required",
            ));
        }
        self.call(move |database| database.begin_checking(project_id, &issue, &run_owner))
            .await
    }

    /// Record a failed check or publication attempt at the current step.
    pub async fn fail_step(&self, id: i64, error: StepError) -> Result<Execution> {
        check_error(&error)?;
        self.call(move |database| database.fail_step(id, &error))
            .await
    }

    /// Orchestrator-only: enter `working` with capacity acquired atomically. Callers must first
    /// establish every start-eligibility gate.
    pub(crate) async fn enter_working(
        &self,
        id: i64,
        issue: IssueSnapshot,
        parent: Option<IssueSnapshot>,
        limit: Option<u32>,
    ) -> Result<Entry> {
        check_snapshot(&issue)?;
        if let Some(parent) = &parent {
            check_snapshot(parent)?;
        }
        self.call(move |database| {
            let limit = match limit {
                Some(limit) => limit,
                None => database.concurrency_limit()?,
            };
            database.enter_working(id, &issue, parent.as_ref(), limit)
        })
        .await
    }

    /// `None` uses the persisted concurrency limit, read in the same worker call.
    pub(crate) async fn retry_working(&self, id: i64, limit: Option<u32>) -> Result<Entry> {
        self.call(move |database| {
            let limit = match limit {
                Some(limit) => limit,
                None => database.concurrency_limit()?,
            };
            database.retry_working(id, limit)
        })
        .await
    }

    pub async fn record_working(&self, id: i64, evidence: WorkingEvidence) -> Result<Execution> {
        self.call(move |database| database.record_working(id, &evidence))
            .await
    }

    pub async fn record_process(&self, id: i64, process_id: u32) -> Result<Execution> {
        self.call(move |database| database.record_process(id, process_id))
            .await
    }

    pub async fn end_working_attempt(
        &self,
        id: i64,
        end: AttemptEnd,
        stopped: bool,
    ) -> Result<Execution> {
        match &end {
            AttemptEnd::Failed(error) => check_error(error)?,
            AttemptEnd::Finished { result } => check_text(
                result,
                MAX_REASON_BYTES,
                "work result must be 1..65536 bytes",
            )?,
            AttemptEnd::Interrupted { reason } => {
                check_text(reason, MAX_REASON_BYTES, "reason must be 1..65536 bytes")?
            }
        }
        self.call(move |database| database.end_working_attempt(id, &end, stopped))
            .await
    }

    pub async fn confirm_stopped(&self, id: i64, reason: String) -> Result<Execution> {
        check_text(&reason, MAX_REASON_BYTES, "reason must be 1..65536 bytes")?;
        self.call(move |database| database.confirm_stopped(id, &reason))
            .await
    }

    pub async fn begin_publication(&self, id: i64, reason: String) -> Result<Execution> {
        check_text(&reason, MAX_REASON_BYTES, "reason must be 1..65536 bytes")?;
        self.call(move |database| database.begin_publication(id, &reason))
            .await
    }

    pub async fn skip_publication(&self, id: i64, reason: String) -> Result<Execution> {
        check_text(&reason, MAX_REASON_BYTES, "reason must be 1..65536 bytes")?;
        self.call(move |database| database.skip_publication(id, &reason))
            .await
    }

    pub async fn record_branch_published(
        &self,
        id: i64,
        remote: String,
        remote_ref: String,
        commit: String,
    ) -> Result<Execution> {
        if remote.is_empty() || remote_ref.is_empty() || commit.is_empty() {
            return Err(Error::InvalidInput("branch evidence is incomplete"));
        }
        self.call(move |database| {
            database.record_branch_published(id, &remote, &remote_ref, &commit)
        })
        .await
    }

    pub async fn record_pull_request_published(
        &self,
        id: i64,
        number: u32,
        url: String,
        node_id: String,
    ) -> Result<Execution> {
        if number == 0 || url.is_empty() || node_id.is_empty() {
            return Err(Error::InvalidInput("pull request evidence is incomplete"));
        }
        self.call(move |database| {
            database.record_pull_request_published(id, number, &url, &node_id)
        })
        .await
    }

    pub async fn reconcile_after_restart(&self) -> Result<Vec<Execution>> {
        self.call(Database::reconcile_after_restart).await
    }

    pub async fn executions(&self) -> Result<Vec<Execution>> {
        self.call(Database::executions).await
    }

    pub async fn execution(&self, id: i64) -> Result<Execution> {
        self.call(move |database| database.execution(id)).await
    }

    pub async fn events(&self, id: i64, after_sequence: i64) -> Result<Vec<ExecutionEvent>> {
        self.call(move |database| database.events(id, after_sequence))
            .await
    }

    pub async fn append_output(
        &self,
        id: i64,
        source: OutputSource,
        payload: String,
    ) -> Result<i64> {
        if payload.len() > MAX_OUTPUT_BYTES {
            return Err(Error::InvalidInput(
                "output blocks are limited to 65536 bytes",
            ));
        }
        self.call(move |database| database.append_output(id, source, &payload))
            .await
    }

    /// Append several blocks in one transaction. Returns the last sequence, or the current
    /// last sequence for an empty batch. Every block is validated before any is written.
    pub async fn append_output_batch(
        &self,
        id: i64,
        blocks: Vec<(OutputSource, String)>,
    ) -> Result<i64> {
        if blocks.len() > MAX_OUTPUT_BATCH {
            return Err(Error::InvalidInput("output batch exceeds 4096 blocks"));
        }
        if blocks
            .iter()
            .any(|(_, payload)| payload.len() > MAX_OUTPUT_BYTES)
        {
            return Err(Error::InvalidInput(
                "output blocks are limited to 65536 bytes",
            ));
        }
        self.call(move |database| database.append_output_batch(id, &blocks))
            .await
    }

    pub async fn output(&self, id: i64, after_sequence: i64) -> Result<Vec<StoredOutput>> {
        self.call(move |database| database.output(id, after_sequence))
            .await
    }

    pub async fn output_count(&self, id: i64) -> Result<i64> {
        self.call(move |database| database.output_count(id)).await
    }
}
