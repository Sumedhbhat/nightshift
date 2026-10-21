use super::{Error, Result};
use crate::domain::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{path::Path, time::Duration};

pub(super) const SCHEMA_VERSION: i64 = 2;

pub(super) struct Database {
    pub(super) connection: Connection,
}

/// Result of asking the orchestrator to enter a slot-consuming step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Entered(Execution),
    /// No capacity. The execution stays at its completed step, pending.
    WaitingForCapacity(Execution),
}

const EXECUTION_COLUMNS: &str = "e.project_id, e.github_host, e.issue_id, e.issue_number, e.run_owner,
    e.title, e.description, e.parent_issue_id, e.parent_number, e.parent_title, e.parent_description,
    e.step, e.status, e.outcome, e.default_branch, e.base_commit, e.branch, e.worktree_path,
    e.codex_thread_id, e.codex_turn_id, e.work_finished_at, e.work_result, e.pushed_remote, e.pushed_ref,
    e.pushed_commit, e.pull_request_number, e.pull_request_url, e.pull_request_id, e.error_transition,
    e.error_message, e.error_detail, e.error_retryable, e.legacy, e.created_at, e.updated_at,
    a.number, a.step, a.process_id, a.started_at, a.ended_at, e.id";

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
            0 => {
                transaction.execute_batch(include_str!("001_initial.sql"))?;
                transaction.execute_batch(include_str!("002_workflow.sql"))?;
            }
            1 => transaction.execute_batch(include_str!("002_workflow.sql"))?,
            SCHEMA_VERSION => {}
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
             (SELECT count(*) FROM slot_attempts WHERE released_at IS NULL)",
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

    pub(super) fn set_setup_command(&mut self, id: i64, command: Option<String>) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE projects SET setup_command = ?1 WHERE id = ?2",
            params![command, id],
        )?;
        if changed == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    pub(super) fn concurrency_limit(&mut self) -> Result<u32> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM settings WHERE key = 'concurrency_limit'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value
            .and_then(|value| value.parse().ok())
            .filter(|value| (1..=MAX_CONCURRENCY_LIMIT).contains(value))
            .unwrap_or(DEFAULT_CONCURRENCY_LIMIT))
    }

    pub(super) fn set_concurrency_limit(&mut self, limit: u32) -> Result<()> {
        if !(1..=MAX_CONCURRENCY_LIMIT).contains(&limit) {
            return Err(Error::InvalidInput(
                "concurrency limit must be between 1 and 8",
            ));
        }
        self.connection.execute(
            "INSERT INTO settings (key, value) VALUES ('concurrency_limit', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [limit.to_string()],
        )?;
        Ok(())
    }

    /// Create the issue's execution at `checking`, or resume checking an existing record that has
    /// not advanced. One record per issue (decision D4) protects against duplicates across checkouts.
    pub(super) fn begin_checking(
        &mut self,
        project_id: i64,
        issue: &IssueIdentity,
        run_owner: &str,
    ) -> Result<Execution> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let host: String = tx
            .query_row(
                "SELECT github_host FROM projects WHERE id = ?1",
                [project_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let existing: Option<(i64, String, String)> = tx
            .query_row(
                "SELECT id, step, status FROM executions
                 WHERE github_host = ?1 AND issue_id = ?2 AND legacy = 0",
                params![host, issue.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let id = match existing {
            Some((id, step, status)) => {
                if step != Step::Checking.as_str() {
                    return Err(Error::AlreadyStarted);
                }
                if status == Status::Running.as_str() {
                    return Err(Error::DuplicateIssue);
                }
                tx.execute(
                    "UPDATE executions SET status = 'running', error_transition = NULL, error_message = NULL,
                     error_detail = NULL, error_retryable = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?1",
                    [id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO executions (project_id, github_host, issue_id, issue_number, run_owner, step, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'checking', 'running')",
                    params![project_id, host, issue.id, issue.number, run_owner],
                )?;
                tx.last_insert_rowid()
            }
        };
        record_event(
            &tx,
            id,
            Step::Checking,
            Status::Running,
            "Checking start eligibility",
            None,
        )?;
        tx.commit()?;
        read_execution(&self.connection, id)
    }

    /// Record why checking did not establish eligibility. The execution stays at `checking`.
    pub(super) fn fail_step(&mut self, id: i64, error: &StepError) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            // Working attempts end through `end_working_attempt`, which handles capacity.
            if execution.status != Status::Running
                || execution
                    .slot_attempt
                    .as_ref()
                    .is_some_and(|a| a.ended_at.is_none())
            {
                return Err(Error::InvalidTransition);
            }
            set_error(tx, id, error)?;
            record_event(
                tx,
                id,
                execution.step,
                Status::Error,
                &error.message,
                error.detail.as_deref(),
            )
        })
    }

    /// Enter `working`: atomically check capacity, save the immutable snapshots, and open a slot
    /// attempt. Without capacity the execution remains at `checking`, pending.
    pub(super) fn enter_working(
        &mut self,
        id: i64,
        issue: &IssueSnapshot,
        parent: Option<&IssueSnapshot>,
        limit: u32,
    ) -> Result<Entry> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let execution = read_execution(&tx, id)?;
        if execution.step != Step::Checking
            || !matches!(execution.status, Status::Running | Status::Pending)
            || execution.issue.id != issue.id
        {
            return Err(Error::InvalidTransition);
        }
        let target = Step::Working;
        if target.policy().consumes_execution_slot && active_slots(&tx)? >= i64::from(limit) {
            if execution.status != Status::Pending {
                tx.execute(
                    "UPDATE executions SET status = 'pending', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                    [id],
                )?;
                record_event(
                    &tx,
                    id,
                    Step::Checking,
                    Status::Pending,
                    "Eligible; waiting for an execution slot",
                    None,
                )?;
            }
            tx.commit()?;
            return Ok(Entry::WaitingForCapacity(read_execution(
                &self.connection,
                id,
            )?));
        }
        tx.execute(
            "UPDATE executions SET step = 'working', status = 'running', title = ?2, description = ?3,
             parent_issue_id = ?4, parent_number = ?5, parent_title = ?6, parent_description = ?7,
             snapshot_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), error_transition = NULL, error_message = NULL,
             error_detail = NULL, error_retryable = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            params![id, issue.title, issue.description, parent.map(|p| &p.id), parent.map(|p| p.number),
                parent.map(|p| &p.title), parent.map(|p| &p.description)],
        )?;
        if target.policy().consumes_execution_slot {
            open_attempt(&tx, id, target)?;
        }
        record_event(
            &tx,
            id,
            target,
            Status::Running,
            "Execution slot reserved; snapshots saved",
            None,
        )?;
        tx.commit()?;
        Ok(Entry::Entered(read_execution(&self.connection, id)?))
    }

    /// Explicit owner continuation of the working step after an error or interruption.
    pub(super) fn retry_working(&mut self, id: i64, limit: u32) -> Result<Entry> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let execution = read_execution(&tx, id)?;
        if execution.step != Step::Working
            || !matches!(execution.status, Status::Error | Status::Interrupted)
            || execution.working.work_finished_at.is_some()
            || execution.legacy
        {
            return Err(Error::InvalidTransition);
        }
        if execution.slot_attempt.is_some() {
            return Err(Error::ProcessNotStopped);
        }
        if active_slots(&tx)? >= i64::from(limit) {
            return Ok(Entry::WaitingForCapacity(execution));
        }
        tx.execute(
            "UPDATE executions SET status = 'running', error_transition = NULL, error_message = NULL,
             error_detail = NULL, error_retryable = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [id],
        )?;
        let number = open_attempt(&tx, id, Step::Working)?;
        record_event(
            &tx,
            id,
            Step::Working,
            Status::Running,
            &format!("Owner retry: working attempt {number}"),
            None,
        )?;
        tx.commit()?;
        Ok(Entry::Entered(read_execution(&self.connection, id)?))
    }

    /// Save working evidence as it is produced. Recorded values cannot be replaced.
    pub(super) fn record_working(
        &mut self,
        id: i64,
        evidence: &WorkingEvidence,
    ) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            if execution.step != Step::Working
                || execution.status != Status::Running
                || execution.slot_attempt.is_none()
            {
                return Err(Error::InvalidTransition);
            }
            let saved = &execution.working;
            let fields = [
                ("default_branch", &saved.default_branch, &evidence.default_branch),
                ("base_commit", &saved.base_commit, &evidence.base_commit),
                ("branch", &saved.branch, &evidence.branch),
                ("worktree_path", &saved.worktree_path, &evidence.worktree_path),
                ("codex_thread_id", &saved.codex_thread_id, &evidence.codex_thread_id),
                ("codex_turn_id", &saved.codex_turn_id, &evidence.codex_turn_id),
            ];
            let mut changed = vec![];
            for (column, old, new) in fields {
                match (old, new) {
                    (_, None) => {}
                    (Some(old), Some(new)) if old == new => {}
                    // A new Codex turn on retry replaces the turn ID; other evidence is fixed.
                    (Some(_), Some(new)) if column == "codex_turn_id" => {
                        tx.execute(&format!("UPDATE executions SET {column} = ?1 WHERE id = ?2"), params![new, id])?;
                        changed.push(format!("{column}={new}"));
                    }
                    (Some(_), Some(_)) => return Err(Error::EvidenceConflict(column)),
                    (None, Some(new)) => {
                        tx.execute(&format!("UPDATE executions SET {column} = ?1 WHERE id = ?2"), params![new, id])?;
                        changed.push(format!("{column}={new}"));
                    }
                }
            }
            if changed.is_empty() {
                return Ok(());
            }
            tx.execute("UPDATE executions SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1", [id])?;
            record_event(tx, id, Step::Working, Status::Running, "Working evidence saved", Some(&changed.join(", ")))
        })
    }

    pub(super) fn record_process(&mut self, id: i64, process_id: u32) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            let Some(attempt) = &execution.slot_attempt else {
                return Err(Error::InvalidTransition);
            };
            if execution.status != Status::Running || attempt.ended_at.is_some() {
                return Err(Error::InvalidTransition);
            }
            tx.execute(
                "UPDATE slot_attempts SET process_id = ?3 WHERE execution_id = ?1 AND number = ?2",
                params![id, attempt.number, process_id],
            )?;
            record_event(
                tx,
                id,
                execution.step,
                Status::Running,
                "Owned process started",
                Some(&format!("pid {process_id}")),
            )
        })
    }

    /// End the running slot attempt. `stopped` confirms the owned process has exited, which
    /// releases capacity; otherwise the slot stays held until `confirm_stopped`.
    pub(super) fn end_working_attempt(
        &mut self,
        id: i64,
        end: &AttemptEnd,
        stopped: bool,
    ) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            let Some(attempt) = &execution.slot_attempt else {
                return Err(Error::InvalidTransition);
            };
            if execution.step != Step::Working || execution.status != Status::Running || attempt.ended_at.is_some() {
                return Err(Error::InvalidTransition);
            }
            let (status, reason) = match end {
                AttemptEnd::Finished { result } => {
                    if !stopped {
                        return Err(Error::ProcessNotStopped);
                    }
                    tx.execute(
                        "UPDATE executions SET work_finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), work_result = ?2 WHERE id = ?1",
                        params![id, result],
                    )?;
                    (Status::Pending, format!("Local work finished: {result}"))
                }
                AttemptEnd::Failed(error) => {
                    set_error(tx, id, error)?;
                    (Status::Error, error.message.clone())
                }
                AttemptEnd::Interrupted { reason } => (Status::Interrupted, reason.clone()),
            };
            tx.execute(
                "UPDATE executions SET status = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                params![id, status.as_str()],
            )?;
            tx.execute(
                "UPDATE slot_attempts SET ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), end_reason = ?3,
                 released_at = CASE WHEN ?4 THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') END
                 WHERE execution_id = ?1 AND number = ?2",
                params![id, attempt.number, reason, stopped],
            )?;
            let detail = (!stopped).then_some("owned process not yet confirmed stopped; slot retained");
            record_event(tx, id, Step::Working, status, &reason, detail)
        })
    }

    /// Release capacity held by an ended attempt once its owned process is confirmed stopped.
    pub(super) fn confirm_stopped(&mut self, id: i64, reason: &str) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            let Some(attempt) = &execution.slot_attempt else {
                return Err(Error::InvalidTransition);
            };
            if attempt.ended_at.is_none() {
                return Err(Error::InvalidTransition);
            }
            tx.execute(
                "UPDATE slot_attempts SET released_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE execution_id = ?1 AND number = ?2",
                params![id, attempt.number],
            )?;
            record_event(
                tx,
                id,
                execution.step,
                execution.status,
                reason,
                Some("execution slot released"),
            )
        })
    }

    /// Start (or explicitly recheck) publication. Allowed after local work finished, or from a
    /// publication error at `working` or `branch_published`.
    pub(super) fn begin_publication(&mut self, id: i64, reason: &str) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            let allowed = match (execution.step, execution.status) {
                (Step::Working, Status::Pending | Status::Error) => {
                    execution.working.work_finished_at.is_some() && execution.slot_attempt.is_none()
                }
                (Step::BranchPublished, Status::Pending | Status::Error) => true,
                _ => false,
            };
            if !allowed {
                return Err(Error::InvalidTransition);
            }
            clear_error_and_set(tx, id, Status::Running)?;
            record_event(tx, id, execution.step, Status::Running, reason, None)
        })
    }

    /// The issue was closed when publication was attempted: keep the local result and end.
    pub(super) fn skip_publication(&mut self, id: i64, reason: &str) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            if execution.step != Step::Working
                || execution.status != Status::Running
                || execution.working.work_finished_at.is_none()
            {
                return Err(Error::InvalidTransition);
            }
            tx.execute(
                "UPDATE executions SET status = 'ended', outcome = 'publication_skipped',
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                [id],
            )?;
            record_event(
                tx,
                id,
                Step::Working,
                Status::Ended,
                reason,
                Some("publication skipped; local result retained"),
            )
        })
    }

    pub(super) fn record_branch_published(
        &mut self,
        id: i64,
        remote: &str,
        remote_ref: &str,
        commit: &str,
    ) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            if execution.step != Step::Working || execution.status != Status::Running || execution.working.work_finished_at.is_none() {
                return Err(Error::InvalidTransition);
            }
            tx.execute(
                "UPDATE executions SET step = 'branch_published', pushed_remote = ?2, pushed_ref = ?3, pushed_commit = ?4,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                params![id, remote, remote_ref, commit],
            )?;
            record_event(tx, id, Step::BranchPublished, Status::Running, "Branch published", Some(&format!("{remote} {remote_ref} {commit}")))
        })
    }

    pub(super) fn record_pull_request_published(
        &mut self,
        id: i64,
        number: u32,
        url: &str,
        node_id: &str,
    ) -> Result<Execution> {
        self.transition(id, |tx, execution| {
            if execution.step != Step::BranchPublished || execution.status != Status::Running {
                return Err(Error::InvalidTransition);
            }
            tx.execute(
                "UPDATE executions SET step = 'pull_request_published', status = 'ended', outcome = 'pull_request_published',
                 pull_request_number = ?2, pull_request_url = ?3, pull_request_id = ?4,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                params![id, number, url, node_id],
            )?;
            record_event(tx, id, Step::PullRequestPublished, Status::Ended, "Pull request published", Some(url))
        })
    }

    /// Startup after quit or crash: nothing is running. Unfinished slot attempts become
    /// interruptions that keep capacity until the owned process is confirmed stopped; other
    /// running transitions become retryable errors. Nothing resumes automatically.
    pub(super) fn reconcile_after_restart(&mut self) -> Result<Vec<Execution>> {
        let ids: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare("SELECT id FROM executions WHERE status = 'running' ORDER BY id")?;
            statement
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut changed = vec![];
        for id in ids {
            let execution = self.transition(id, |tx, execution| {
                match (execution.step, &execution.slot_attempt) {
                    (Step::Checking, _) => {
                        clear_error_and_set(tx, id, Status::Pending)?;
                        record_event(tx, id, Step::Checking, Status::Pending, "Nightshift restarted during checking", None)
                    }
                    (Step::Working, Some(attempt)) if attempt.ended_at.is_none() => {
                        tx.execute(
                            "UPDATE executions SET status = 'interrupted', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                            [id],
                        )?;
                        tx.execute(
                            "UPDATE slot_attempts SET ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                             end_reason = 'Nightshift stopped before this attempt finished'
                             WHERE execution_id = ?1 AND number = ?2",
                            params![id, attempt.number],
                        )?;
                        record_event(tx, id, Step::Working, Status::Interrupted, "Nightshift stopped before this attempt finished",
                            Some("slot retained until the owned process is confirmed stopped"))
                    }
                    (step, _) => {
                        let error = StepError {
                            transition: step.next().unwrap_or(step),
                            message: "Nightshift stopped during publication. Use Recheck publication.".into(),
                            detail: None,
                            retryable: true,
                        };
                        set_error(tx, id, &error)?;
                        record_event(tx, id, step, Status::Error, &error.message, None)
                    }
                }
            })?;
            changed.push(execution);
        }
        Ok(changed)
    }

    pub(super) fn executions(&mut self) -> Result<Vec<Execution>> {
        let ids: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare("SELECT id FROM executions ORDER BY id DESC LIMIT 500")?;
            statement
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.into_iter()
            .map(|id| read_execution(&self.connection, id))
            .collect()
    }

    pub(super) fn execution(&mut self, id: i64) -> Result<Execution> {
        read_execution(&self.connection, id)
    }

    pub(super) fn events(&mut self, id: i64, after: i64) -> Result<Vec<ExecutionEvent>> {
        let mut statement = self.connection.prepare("SELECT sequence, step, status, reason, detail, created_at
            FROM execution_events WHERE execution_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT 100")?;
        Ok(statement
            .query_map(params![id, after], |row| {
                Ok(ExecutionEvent {
                    sequence: row.get(0)?,
                    step: row.get(1)?,
                    status: row.get(2)?,
                    reason: row.get(3)?,
                    detail: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn append_output(
        &mut self,
        id: i64,
        source: OutputSource,
        payload: &str,
    ) -> Result<i64> {
        self.append_output_batch(id, &[(source, payload.to_owned())])
    }

    /// Process output requires a slot attempt whose process is not yet confirmed stopped.
    /// Nightshift's own system and Git messages may be added until the execution ends.
    pub(super) fn append_output_batch(
        &mut self,
        id: i64,
        blocks: &[(OutputSource, String)],
    ) -> Result<i64> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let execution = read_execution(&tx, id)?;
        if execution.status == Status::Ended {
            return Err(Error::InvalidTransition);
        }
        let process_output = blocks.iter().any(|(source, _)| {
            matches!(
                source,
                OutputSource::Setup | OutputSource::Agent | OutputSource::Stderr
            )
        });
        if process_output && execution.slot_attempt.is_none() {
            return Err(Error::InvalidTransition);
        }
        let attempt = execution.slot_attempt.as_ref().map(|a| a.number);
        let mut sequence: i64 = tx.query_row(
            "SELECT coalesce(max(sequence), 0) FROM execution_output WHERE execution_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO execution_output (execution_id, sequence, attempt, source, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (source, payload) in blocks {
                sequence += 1;
                insert.execute(params![id, sequence, attempt, source.as_str(), payload])?;
            }
        }
        tx.commit()?;
        Ok(sequence)
    }

    pub(super) fn output(&mut self, id: i64, after: i64) -> Result<Vec<StoredOutput>> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, attempt, source, payload, created_at FROM execution_output
            WHERE execution_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT 100",
        )?;
        Ok(statement
            .query_map(params![id, after], |row| {
                Ok(StoredOutput {
                    sequence: row.get(0)?,
                    attempt: row.get(1)?,
                    source: row.get(2)?,
                    payload: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(super) fn output_count(&mut self, id: i64) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT coalesce(max(sequence), 0) FROM execution_output WHERE execution_id = ?1",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Run one guarded state change and its audit event in a single immediate transaction.
    fn transition(
        &mut self,
        id: i64,
        change: impl FnOnce(&Transaction<'_>, &Execution) -> Result<()>,
    ) -> Result<Execution> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let execution = read_execution(&tx, id)?;
        if execution.status == Status::Ended || execution.legacy {
            return Err(Error::InvalidTransition);
        }
        change(&tx, &execution)?;
        tx.commit()?;
        read_execution(&self.connection, id)
    }
}

/// How a working slot attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptEnd {
    Finished { result: String },
    Failed(StepError),
    Interrupted { reason: String },
}

/// Count unreleased attempts in steps whose policy consumes a slot.
fn active_slots(tx: &Transaction<'_>) -> Result<i64> {
    let steps: Vec<&str> = Step::slot_consuming().map(Step::as_str).collect();
    let placeholders = vec!["?"; steps.len()].join(", ");
    Ok(tx.query_row(
        &format!("SELECT count(*) FROM slot_attempts WHERE released_at IS NULL AND step IN ({placeholders})"),
        rusqlite::params_from_iter(steps),
        |r| r.get(0),
    )?)
}

fn open_attempt(tx: &Transaction<'_>, id: i64, step: Step) -> Result<i64> {
    debug_assert!(step.policy().consumes_execution_slot);
    let number: i64 = tx.query_row(
        "SELECT coalesce(max(number), 0) + 1 FROM slot_attempts WHERE execution_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO slot_attempts (execution_id, number, step) VALUES (?1, ?2, ?3)",
        params![id, number, step.as_str()],
    )?;
    Ok(number)
}

fn set_error(tx: &Transaction<'_>, id: i64, error: &StepError) -> Result<()> {
    tx.execute(
        "UPDATE executions SET status = 'error', error_transition = ?2, error_message = ?3, error_detail = ?4,
         error_retryable = ?5, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
        params![id, error.transition.as_str(), error.message, error.detail, error.retryable],
    )?;
    Ok(())
}

fn clear_error_and_set(tx: &Transaction<'_>, id: i64, status: Status) -> Result<()> {
    tx.execute(
        "UPDATE executions SET status = ?2, error_transition = NULL, error_message = NULL, error_detail = NULL,
         error_retryable = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
        params![id, status.as_str()],
    )?;
    Ok(())
}

fn read_execution(connection: &Connection, id: i64) -> Result<Execution> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {EXECUTION_COLUMNS} FROM executions e
         LEFT JOIN slot_attempts a ON a.execution_id = e.id AND a.released_at IS NULL
         WHERE e.id = ?1"
    ))?;
    let mut rows = statement.query([id])?;
    let row = rows.next()?.ok_or(Error::NotFound)?;
    let unknown = |what: &'static str| Error::InvalidInput(what);
    let step: String = row.get(11)?;
    let status: String = row.get(12)?;
    let outcome: Option<String> = row.get(13)?;
    let snapshot = row
        .get::<_, Option<String>>(5)?
        .map(|title| {
            Ok::<_, rusqlite::Error>(IssueSnapshot {
                id: row.get(2)?,
                number: row.get(3)?,
                title,
                description: row.get(6)?,
            })
        })
        .transpose()?;
    let parent = row
        .get::<_, Option<String>>(7)?
        .map(|parent_id| {
            Ok::<_, rusqlite::Error>(IssueSnapshot {
                id: parent_id,
                number: row.get(8)?,
                title: row.get(9)?,
                description: row.get(10)?,
            })
        })
        .transpose()?;
    let error = row
        .get::<_, Option<String>>(29)?
        .map(|message| -> Result<StepError> {
            let transition: String = row.get(28)?;
            Ok(StepError {
                transition: Step::parse(&transition)
                    .ok_or(unknown("unknown stored error transition"))?,
                message,
                detail: row.get(30)?,
                retryable: row.get::<_, Option<bool>>(31)?.unwrap_or(false),
            })
        })
        .transpose()?;
    let slot_attempt = row
        .get::<_, Option<i64>>(35)?
        .map(|number| -> Result<SlotAttempt> {
            let step: String = row.get(36)?;
            Ok(SlotAttempt {
                number,
                step: Step::parse(&step).ok_or(unknown("unknown stored attempt step"))?,
                process_id: row.get(37)?,
                started_at: row.get(38)?,
                ended_at: row.get(39)?,
            })
        })
        .transpose()?;
    Ok(Execution {
        id,
        project_id: row.get(0)?,
        github_host: row.get(1)?,
        issue: IssueIdentity {
            id: row.get(2)?,
            number: row.get(3)?,
        },
        run_owner: row.get(4)?,
        snapshot,
        parent,
        step: Step::parse(&step).ok_or(unknown("unknown stored workflow step"))?,
        status: Status::parse(&status).ok_or(unknown("unknown stored execution status"))?,
        outcome: outcome
            .map(|value| Outcome::parse(&value).ok_or(unknown("unknown stored outcome")))
            .transpose()?,
        error,
        working: WorkingEvidence {
            default_branch: row.get(14)?,
            base_commit: row.get(15)?,
            branch: row.get(16)?,
            worktree_path: row.get(17)?,
            codex_thread_id: row.get(18)?,
            codex_turn_id: row.get(19)?,
            work_finished_at: row.get(20)?,
            work_result: row.get(21)?,
        },
        publication: PublicationEvidence {
            pushed_remote: row.get(22)?,
            pushed_ref: row.get(23)?,
            pushed_commit: row.get(24)?,
            pull_request_number: row.get(25)?,
            pull_request_url: row.get(26)?,
            pull_request_id: row.get(27)?,
        },
        slot_attempt,
        legacy: row.get(32)?,
        created_at: row.get(33)?,
        updated_at: row.get(34)?,
    })
}

fn record_event(
    tx: &Transaction<'_>,
    id: i64,
    step: Step,
    status: Status,
    reason: &str,
    detail: Option<&str>,
) -> Result<()> {
    tx.execute("INSERT INTO execution_events (execution_id, sequence, step, status, reason, detail)
        SELECT ?1, coalesce(max(sequence), 0) + 1, ?2, ?3, ?4, ?5 FROM execution_events WHERE execution_id = ?1",
        params![id, step.as_str(), status.as_str(), reason, detail])?;
    Ok(())
}
