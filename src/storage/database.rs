use super::{Error, Result};
use crate::domain::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{num::NonZeroU32, path::Path, time::Duration};

pub(super) struct Database {
    pub(super) connection: Connection,
}

impl Database {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => transaction.execute_batch(include_str!("001_initial.sql"))?,
            1 => {}
            version => return Err(Error::NewerSchema(version)),
        }
        transaction.commit()?;
        Ok(Self { connection })
    }

    pub(super) fn projects(&mut self) -> Result<Vec<Project>> {
        let mut statement = self.connection.prepare("SELECT id, checkout_path, github_host, repository_id, owner, name, remote, setup_command FROM projects ORDER BY id")?;
        Ok(statement
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    checkout_path: row.get(1)?,
                    github_host: row.get(2)?,
                    repository_id: row.get(3)?,
                    owner: row.get(4)?,
                    name: row.get(5)?,
                    remote: row.get(6)?,
                    setup_command: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn summary(&mut self) -> Result<HistorySummary> {
        Ok(self.connection.query_row(
            "SELECT (SELECT count(*) FROM projects), (SELECT count(*) FROM executions),
             (SELECT count(*) FROM executions WHERE termination_confirmed = 0)",
            [],
            |row| {
                Ok(HistorySummary {
                    projects: row.get(0)?,
                    executions: row.get(1)?,
                    reserved_slots: row.get(2)?,
                })
            },
        )?)
    }

    pub(super) fn add_project(&mut self, project: NewProject) -> Result<Project> {
        self.connection.execute(
            "INSERT INTO projects (checkout_path, github_host, repository_id, owner, name, remote, setup_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![project.checkout_path, project.github_host, project.repository_id, project.owner,
                project.name, project.remote, project.setup_command])?;
        Ok(Project {
            id: self.connection.last_insert_rowid(),
            checkout_path: project.checkout_path,
            github_host: project.github_host,
            repository_id: project.repository_id,
            owner: project.owner,
            name: project.name,
            remote: project.remote,
            setup_command: project.setup_command,
        })
    }

    pub(super) fn reserve(&mut self, new: NewExecution, limit: NonZeroU32) -> Result<Execution> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let host: String = tx
            .query_row(
                "SELECT github_host FROM projects WHERE id = ?1",
                [new.project_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM executions WHERE github_host = ?1 AND issue_id = ?2 AND termination_confirmed = 0)",
            params![host, new.issue.id], |r| r.get(0))?;
        if duplicate {
            return Err(Error::DuplicateIssue);
        }
        if let Some(parent) = &new.parent {
            let busy: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM executions WHERE github_host = ?1 AND parent_issue_id = ?2 AND termination_confirmed = 0)",
                params![host, parent.id], |r| r.get(0))?;
            if busy {
                return Err(Error::ParentBusy);
            }
        }
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM executions WHERE termination_confirmed = 0",
            [],
            |r| r.get(0),
        )?;
        if active >= i64::from(limit.get()) {
            return Err(Error::Capacity);
        }
        let parent = new.parent.as_ref();
        tx.execute(
            "INSERT INTO executions (project_id, github_host, issue_id, issue_number, title, description,
             parent_issue_id, parent_number, parent_title, parent_description, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'preparing')",
            params![new.project_id, host, new.issue.id, new.issue.number, new.issue.title, new.issue.description,
                parent.map(|p| &p.id), parent.map(|p| p.number), parent.map(|p| &p.title), parent.map(|p| &p.description)])?;
        let id = tx.last_insert_rowid();
        record_event(
            &tx,
            id,
            None,
            ExecutionStatus::Preparing,
            false,
            "Execution reserved",
        )?;
        tx.commit()?;
        self.execution(id)
    }

    pub(super) fn execution(&mut self, id: i64) -> Result<Execution> {
        read_execution(&self.connection, id)
    }

    pub(super) fn change(
        &mut self,
        id: i64,
        change: ExecutionChange,
        reason: &str,
    ) -> Result<Execution> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = read_execution(&tx, id)?;
        let (status, stopped) = match (previous.status, previous.termination_confirmed, change) {
            (ExecutionStatus::Preparing, false, ExecutionChange::Started) => {
                (ExecutionStatus::Running, false)
            }
            (
                ExecutionStatus::Preparing | ExecutionStatus::Running,
                false,
                ExecutionChange::Interrupted,
            ) => (ExecutionStatus::Interrupted, false),
            (
                ExecutionStatus::Preparing | ExecutionStatus::Running,
                false,
                ExecutionChange::FailedAndStopped,
            ) => (ExecutionStatus::Failed, true),
            (ExecutionStatus::Interrupted, false, ExecutionChange::TerminationConfirmed) => {
                (ExecutionStatus::Interrupted, true)
            }
            _ => return Err(Error::InvalidTransition),
        };
        tx.execute(
            "UPDATE executions SET status = ?1, termination_confirmed = ?2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?3",
            params![status.as_str(), stopped, id],
        )?;
        record_event(
            &tx,
            id,
            Some(previous.status.as_str()),
            status,
            stopped,
            reason,
        )?;
        tx.commit()?;
        self.execution(id)
    }

    pub(super) fn events(&mut self, id: i64, after: i64) -> Result<Vec<ExecutionEvent>> {
        let mut statement = self.connection.prepare("SELECT sequence, previous_status, status, termination_confirmed,
            reason, created_at FROM execution_events WHERE execution_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT 100")?;
        Ok(statement
            .query_map(params![id, after], |row| {
                Ok(ExecutionEvent {
                    sequence: row.get(0)?,
                    previous_status: row.get(1)?,
                    status: row.get(2)?,
                    termination_confirmed: row.get(3)?,
                    reason: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn append_output(&mut self, id: i64, source: &str, payload: &str) -> Result<i64> {
        self.append_output_batch(id, &[(source.to_owned(), payload.to_owned())])
    }

    pub(super) fn append_output_batch(
        &mut self,
        id: i64,
        blocks: &[(String, String)],
    ) -> Result<i64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stopped: bool = tx
            .query_row(
                "SELECT termination_confirmed FROM executions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if stopped {
            return Err(Error::InvalidTransition);
        }
        let mut sequence: i64 = tx.query_row(
            "SELECT coalesce(max(sequence), 0) FROM execution_output WHERE execution_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO execution_output (execution_id, sequence, source, payload) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (source, payload) in blocks {
                sequence += 1;
                insert.execute(params![id, sequence, source, payload])?;
            }
        }
        tx.commit()?;
        Ok(sequence)
    }

    pub(super) fn output(&mut self, id: i64, after: i64) -> Result<Vec<StoredOutput>> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, source, payload, created_at FROM execution_output
            WHERE execution_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT 100",
        )?;
        Ok(statement
            .query_map(params![id, after], |row| {
                Ok(StoredOutput {
                    sequence: row.get(0)?,
                    source: row.get(1)?,
                    payload: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn read_execution(connection: &Connection, id: i64) -> Result<Execution> {
    let mut execution = connection.prepare("SELECT project_id, issue_id, issue_number, title, description,
        parent_issue_id, parent_number, parent_title, parent_description, status, termination_confirmed,
        created_at, updated_at FROM executions WHERE id = ?1")?;
    let mut rows = execution.query([id])?;
    let row = rows.next()?.ok_or(Error::NotFound)?;
    let status: String = row.get(9)?;
    let status = match status.as_str() {
        "preparing" => ExecutionStatus::Preparing,
        "running" => ExecutionStatus::Running,
        "interrupted" => ExecutionStatus::Interrupted,
        "failed" => ExecutionStatus::Failed,
        _ => return Err(Error::InvalidInput("unknown stored execution status")),
    };
    let parent_id: Option<String> = row.get(5)?;
    Ok(Execution {
        id,
        project_id: row.get(0)?,
        issue: IssueSnapshot {
            id: row.get(1)?,
            number: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
        },
        parent: parent_id
            .map(|id| {
                Ok::<_, rusqlite::Error>(IssueSnapshot {
                    id,
                    number: row.get(6)?,
                    title: row.get(7)?,
                    description: row.get(8)?,
                })
            })
            .transpose()?,
        status,
        termination_confirmed: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn record_event(
    tx: &Transaction<'_>,
    id: i64,
    previous: Option<&str>,
    status: ExecutionStatus,
    stopped: bool,
    reason: &str,
) -> Result<()> {
    tx.execute("INSERT INTO execution_events (execution_id, sequence, previous_status, status, termination_confirmed, reason)
        SELECT ?1, coalesce(max(sequence), 0) + 1, ?2, ?3, ?4, ?5 FROM execution_events WHERE execution_id = ?1",
        params![id, previous, status.as_str(), stopped, reason])?;
    Ok(())
}
