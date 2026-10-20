//! One database connection on a dedicated worker. No SQLite runs on the GPUI thread.
mod database;
#[cfg(test)]
mod tests;

use crate::domain::*;
use database::Database;
use std::{fs::File, num::NonZeroU32, path::PathBuf};
use tokio::sync::{mpsc, oneshot};

pub const MAX_OUTPUT_BYTES: usize = 65_536;
pub const MAX_OUTPUT_BATCH: usize = 4_096;
const MAX_SNAPSHOT_BYTES: usize = 1_048_576;
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
    #[error("all configured execution slots are reserved")]
    Capacity,
    #[error("this issue already has a reserved execution")]
    DuplicateIssue,
    #[error("another child of this parent has a reserved execution")]
    ParentBusy,
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

    /// Capacity reservation only. The caller must first establish owner authorization and eligibility.
    pub(crate) async fn reserve(
        &self,
        execution: NewExecution,
        concurrency_limit: NonZeroU32,
    ) -> Result<Execution> {
        for issue in std::iter::once(&execution.issue).chain(execution.parent.iter()) {
            if issue.id.is_empty() || issue.number == 0 {
                return Err(Error::InvalidInput(
                    "issue identity and number are required",
                ));
            }
            if issue.description.len() + issue.title.len() + issue.id.len() > MAX_SNAPSHOT_BYTES {
                return Err(Error::InvalidInput("issue snapshot exceeds 1 MiB"));
            }
        }
        self.call(move |database| database.reserve(execution, concurrency_limit))
            .await
    }

    pub async fn execution(&self, id: i64) -> Result<Execution> {
        self.call(move |database| database.execution(id)).await
    }

    pub async fn change(
        &self,
        id: i64,
        change: ExecutionChange,
        reason: String,
    ) -> Result<Execution> {
        if reason.is_empty() || reason.len() > MAX_OUTPUT_BYTES {
            return Err(Error::InvalidInput(
                "state change reason must be 1..65536 bytes",
            ));
        }
        self.call(move |database| database.change(id, change, &reason))
            .await
    }

    pub async fn events(&self, id: i64, after_sequence: i64) -> Result<Vec<ExecutionEvent>> {
        self.call(move |database| database.events(id, after_sequence))
            .await
    }

    pub async fn append_output(&self, id: i64, source: String, payload: String) -> Result<i64> {
        if payload.len() > MAX_OUTPUT_BYTES
            || !matches!(source.as_str(), "setup" | "agent" | "stderr")
        {
            return Err(Error::InvalidInput(
                "output needs a known source and at most 65536 bytes",
            ));
        }
        self.call(move |database| database.append_output(id, &source, &payload))
            .await
    }

    /// Append several blocks in one transaction. Returns the last sequence, or the current
    /// last sequence for an empty batch. Every block is validated before any is written.
    pub async fn append_output_batch(&self, id: i64, blocks: Vec<(String, String)>) -> Result<i64> {
        if blocks.len() > MAX_OUTPUT_BATCH {
            return Err(Error::InvalidInput("output batch exceeds 4096 blocks"));
        }
        for (source, payload) in &blocks {
            if payload.len() > MAX_OUTPUT_BYTES
                || !matches!(source.as_str(), "setup" | "agent" | "stderr")
            {
                return Err(Error::InvalidInput(
                    "output needs a known source and at most 65536 bytes",
                ));
            }
        }
        self.call(move |database| database.append_output_batch(id, &blocks))
            .await
    }

    pub async fn output(&self, id: i64, after_sequence: i64) -> Result<Vec<StoredOutput>> {
        self.call(move |database| database.output(id, after_sequence))
            .await
    }
}
