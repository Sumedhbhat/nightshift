//! The run orchestrator: an actor that owns the active run, its refresh schedule, and every
//! execution task. All inputs arrive through one inbox, so no two refreshes ever overlap.
use crate::{
    application::{
        Coordinator,
        execution::{self, Services},
        start,
    },
    codex,
    domain::{Execution, IssueIdentity, Project, Status, Step, StepError, WorkingEvidence},
    github::eligibility,
    storage::{AttemptEnd, Entry, Error as StoreError},
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const FIRST_BACKOFF: Duration = Duration::from_secs(60);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum Command {
    /// Capture the active `gh` account as run owner and refresh immediately.
    StartRun,
    EndRun,
    /// Manual refresh; does not reset the automatic schedule.
    Refresh,
    RetryWorking(i64),
    RecheckPublication(i64),
    SetConcurrencyLimit(u32),
    Shutdown(oneshot::Sender<()>),
}

/// What the UI shows about the run. Sent whenever it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunStatus {
    pub owner: Option<String>,
    pub refreshing: bool,
    pub next_refresh: Option<Instant>,
    pub last_refresh: Option<String>,
    pub problems: Vec<String>,
    pub active_tasks: usize,
    pub concurrency_limit: u32,
}

#[derive(Clone, Debug)]
pub enum Update {
    Run(RunStatus),
    /// An execution changed; the UI reloads it from storage.
    Executions,
    Message(String),
}

enum Message {
    Command(Command),
    RefreshDue(u64),
    RefreshDone(Result<Refreshed, String>, bool),
    /// The refresh entered `working` for this execution; the actor must own it at once.
    Started(Box<(Execution, Project)>),
    TaskFinished(i64),
}

struct Refreshed {
    started: usize,
    problems: Vec<String>,
    transient: bool,
    summary: String,
}

#[derive(Clone)]
pub struct Handle {
    inbox: mpsc::Sender<Message>,
}

impl Handle {
    pub fn send(&self, command: Command) -> bool {
        self.inbox.try_send(Message::Command(command)).is_ok()
    }

    /// Stop all executions and wait until they record their interruption.
    pub async fn shutdown(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .inbox
            .send(Message::Command(Command::Shutdown(reply)))
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

struct Task {
    stop: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

struct Orchestrator {
    services: Services,
    coordinator: Coordinator,
    inbox: mpsc::Sender<Message>,
    updates: mpsc::Sender<Update>,
    status: RunStatus,
    schedule: u64,
    backoff: Option<Duration>,
    refreshing: bool,
    refresh_again: bool,
    tasks: HashMap<i64, Task>,
    shutting_down: bool,
    /// Shared with refresh tasks so they stop entering `working` once shutdown begins.
    stopping: Arc<AtomicBool>,
    pending_shutdown: Option<oneshot::Sender<()>>,
}

/// Start the orchestrator on the current Tokio runtime.
pub fn spawn(services: Services, updates: mpsc::Sender<Update>) -> Handle {
    let (inbox, mut receiver) = mpsc::channel(256);
    let mut orchestrator = Orchestrator {
        coordinator: Coordinator::new(services.store.clone()),
        services,
        inbox: inbox.clone(),
        updates,
        status: RunStatus::default(),
        schedule: 0,
        backoff: None,
        refreshing: false,
        refresh_again: false,
        tasks: HashMap::new(),
        shutting_down: false,
        stopping: Arc::default(),
        pending_shutdown: None,
    };
    tokio::spawn(async move {
        orchestrator.confirm_stopped_after_restart().await;
        orchestrator.status.concurrency_limit = orchestrator
            .services
            .store
            .concurrency_limit()
            .await
            .unwrap_or(2);
        orchestrator.publish_status();
        while let Some(message) = receiver.recv().await {
            if orchestrator.handle(message).await {
                break;
            }
        }
    });
    Handle { inbox }
}

impl Orchestrator {
    fn publish_status(&mut self) {
        self.status.refreshing = self.refreshing;
        self.status.active_tasks = self.tasks.len();
        let _ = self.updates.try_send(Update::Run(self.status.clone()));
    }

    fn changed(&self) {
        let _ = self.updates.try_send(Update::Executions);
    }

    fn message(&self, text: String) {
        let _ = self.updates.try_send(Update::Message(text));
    }

    /// After a crash, interrupted attempts keep their slots. Stop any orphaned processes
    /// they owned and release the slot only when nothing remains. Nothing resumes.
    async fn confirm_stopped_after_restart(&mut self) {
        let Ok(executions) = self.services.store.executions().await else {
            return;
        };
        for execution in executions {
            let Some(attempt) = &execution.slot_attempt else {
                continue;
            };
            if attempt.ended_at.is_none() {
                continue;
            }
            let worktree = execution
                .working
                .worktree_path
                .as_deref()
                .map(std::path::Path::new);
            if codex::stop_orphan(attempt.process_id, worktree).await {
                let _ = self
                    .services
                    .store
                    .confirm_stopped(
                        execution.id,
                        "Confirmed no owned process remains after restart".into(),
                    )
                    .await;
            }
        }
        self.changed();
    }

    /// Returns true when the actor should exit.
    async fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Command(command) => return self.command(command).await,
            Message::RefreshDue(schedule) => {
                if schedule == self.schedule && self.status.owner.is_some() {
                    self.refresh(true);
                }
            }
            Message::RefreshDone(result, automatic) => {
                self.refreshing = false;
                match result {
                    Ok(refreshed) => {
                        self.status.problems = refreshed.problems;
                        self.status.last_refresh = Some(refreshed.summary);

                        if automatic {
                            self.backoff = if refreshed.transient {
                                Some(
                                    self.backoff
                                        .map_or(FIRST_BACKOFF, |b| (b * 2).min(MAX_BACKOFF)),
                                )
                            } else {
                                None
                            };
                        }
                    }
                    Err(error) => {
                        self.status.problems = vec![error];
                        if automatic {
                            self.backoff = Some(
                                self.backoff
                                    .map_or(FIRST_BACKOFF, |b| (b * 2).min(MAX_BACKOFF)),
                            );
                        }
                    }
                }
                if automatic {
                    self.schedule_next();
                }
                self.changed();
                if let Some(reply) = self.pending_shutdown.take() {
                    self.finish_shutdown(reply).await;
                    return true;
                }
                if std::mem::take(&mut self.refresh_again) && self.status.owner.is_some() {
                    self.refresh(false);
                }
                self.publish_status();
            }
            Message::Started(started) => {
                let (execution, project) = *started;
                if self.shutting_down {
                    // Nothing was launched yet, so nothing remains to stop.
                    let _ = self
                        .services
                        .store
                        .end_working_attempt(
                            execution.id,
                            AttemptEnd::Interrupted {
                                reason: "Nightshift is quitting".into(),
                            },
                            true,
                        )
                        .await;
                    self.changed();
                } else {
                    self.start_task(execution, project);
                }
            }
            Message::TaskFinished(id) => {
                self.tasks.remove(&id);
                self.changed();
                // Finishing frees capacity during an active run: refresh now (decision D2).
                if self.status.owner.is_some() && !self.shutting_down {
                    self.refresh(false);
                }
                self.publish_status();
            }
        }
        false
    }

    async fn command(&mut self, command: Command) -> bool {
        match command {
            Command::StartRun => {
                if self.shutting_down || self.status.owner.is_some() {
                    return false;
                }
                match self.services.github.gh.probe().await {
                    Ok(account) => {
                        self.status.owner = Some(account.login.clone());
                        self.status.problems.clear();
                        self.backoff = None;
                        self.message(format!("Run started as {}.", account.login));
                        self.refresh(true);
                    }
                    Err(error) => {
                        self.status.problems = vec![error.to_string()];
                        self.message(format!("Could not start a run: {error}"));
                    }
                }
            }
            Command::EndRun => {
                // Ending the run stops new starts; active local work continues.
                self.status.owner = None;
                self.status.next_refresh = None;
                self.schedule += 1;
                self.message(
                    "Run ended. Active executions continue; no new work will start.".into(),
                );
            }
            Command::Refresh => {
                if self.status.owner.is_some() {
                    self.refresh(false);
                } else {
                    self.message("Start a run to discover and start eligible issues.".into());
                }
            }
            Command::RetryWorking(id) => self.retry_working(id).await,
            Command::RecheckPublication(id) => self.recheck_publication(id).await,
            Command::SetConcurrencyLimit(limit) => {
                match self.services.store.set_concurrency_limit(limit).await {
                    Ok(()) => {
                        self.status.concurrency_limit = limit;
                        if self.status.owner.is_some() {
                            self.refresh(false);
                        }
                    }
                    Err(error) => self.message(error.to_string()),
                }
            }
            Command::Shutdown(reply) => {
                self.shutting_down = true;
                self.stopping.store(true, Ordering::SeqCst);
                self.status.owner = None;
                self.schedule += 1;
                for task in self.tasks.values() {
                    let _ = task.stop.send(true);
                }
                // A refresh in flight may still report started executions; take them first.
                if self.refreshing {
                    self.pending_shutdown = Some(reply);
                    return false;
                }
                self.finish_shutdown(reply).await;
                return true;
            }
        }
        self.publish_status();
        false
    }

    async fn finish_shutdown(&mut self, reply: oneshot::Sender<()>) {
        for task in self.tasks.values() {
            let _ = task.stop.send(true);
        }
        let handles: Vec<_> = self.tasks.drain().map(|(_, task)| task.handle).collect();
        let all = async {
            for handle in handles {
                let _ = handle.await;
            }
        };
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, all).await.is_err() {
            eprintln!("nightshift: executions did not stop within {SHUTDOWN_TIMEOUT:?}");
        }
        let _ = reply.send(());
    }

    fn schedule_next(&mut self) {
        if self.status.owner.is_none() {
            self.status.next_refresh = None;
            return;
        }
        self.schedule += 1;
        let schedule = self.schedule;
        let delay = self.backoff.unwrap_or(REFRESH_INTERVAL);
        self.status.next_refresh = Some(Instant::now() + delay);
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = inbox.send(Message::RefreshDue(schedule)).await;
        });
    }

    /// Start a refresh unless one is running; a request during a refresh runs once afterwards.
    fn refresh(&mut self, automatic: bool) {
        if self.shutting_down {
            return;
        }
        if self.refreshing {
            self.refresh_again = true;
            return;
        }
        let Some(owner) = self.status.owner.clone() else {
            return;
        };
        self.refreshing = true;
        let services = self.services.clone();
        let coordinator = self.coordinator.clone();
        let inbox = self.inbox.clone();
        let running: Vec<i64> = self.tasks.keys().copied().collect();
        let stopping = self.stopping.clone();
        tokio::spawn(async move {
            let result =
                refresh(&services, &coordinator, &owner, &running, &stopping, &inbox).await;
            let _ = inbox.send(Message::RefreshDone(result, automatic)).await;
        });
        self.publish_status();
    }

    fn start_task(&mut self, execution: Execution, project: Project) {
        if self.shutting_down || self.tasks.contains_key(&execution.id) {
            return;
        }
        let (stop, stopped) = watch::channel(false);
        let id = execution.id;
        let services = self.services.clone();
        let inbox = self.inbox.clone();
        let handle = tokio::spawn(async move {
            execution::run(services, execution, project, stopped).await;
            let _ = inbox.send(Message::TaskFinished(id)).await;
        });
        self.tasks.insert(id, Task { stop, handle });
        self.changed();
    }

    async fn project(&self, id: i64) -> Option<Project> {
        self.services
            .store
            .projects()
            .await
            .ok()?
            .into_iter()
            .find(|p| p.id == id)
    }

    /// **Retry working**: confirm the previous process stopped, acquire capacity, continue.
    async fn retry_working(&mut self, id: i64) {
        let store = &self.services.store;
        let Ok(execution) = store.execution(id).await else {
            return;
        };
        if let Some(attempt) = &execution.slot_attempt {
            let worktree = execution
                .working
                .worktree_path
                .as_deref()
                .map(std::path::Path::new);
            if attempt.ended_at.is_none() || !codex::stop_orphan(attempt.process_id, worktree).await
            {
                self.message(format!(
                    "Execution {id}: the previous Codex process has not stopped; retry later."
                ));
                return;
            }
            let _ = store
                .confirm_stopped(
                    id,
                    "Owner retry confirmed the previous process stopped".into(),
                )
                .await;
        }
        let Some(project) = self.project(execution.project_id).await else {
            return;
        };
        match self.coordinator.retry_working(id).await {
            Ok(Entry::Entered(execution)) => self.start_task(execution, project),
            Ok(Entry::WaitingForCapacity(_)) => {
                self.message("All execution slots are in use. Retry when one is free.".into())
            }
            Err(error) => self.message(format!("Retry working failed: {error}")),
        }
        self.changed();
    }

    /// **Recheck publication**: verify the issue is open and resume incomplete publication.
    async fn recheck_publication(&mut self, id: i64) {
        let Ok(execution) = self.services.store.execution(id).await else {
            return;
        };
        let Some(project) = self.project(execution.project_id).await else {
            return;
        };
        let services = self.services.clone();
        let updates = self.updates.clone();
        tokio::spawn(async move {
            execution::publish(
                &services,
                id,
                &project,
                "Owner requested a publication recheck",
            )
            .await;
            let _ = updates.try_send(Update::Executions);
        });
    }
}

/// One refresh: list candidates for the run owner, check each one's start gates, and enter
/// `working` for eligible issues while capacity lasts. Read-only toward publication.
async fn refresh(
    services: &Services,
    coordinator: &Coordinator,
    owner: &str,
    running: &[i64],
    stopping: &AtomicBool,
    inbox: &mpsc::Sender<Message>,
) -> Result<Refreshed, String> {
    let store = &services.store;
    let github = &services.github;
    github
        .gh
        .verify_owner(owner)
        .await
        .map_err(|e| e.to_string())?;
    let projects = store.projects().await.map_err(|e| e.to_string())?;
    let search = eligibility::candidates(&github.gh, &projects, owner).await;
    let mut refreshed = Refreshed {
        started: 0,
        problems: vec![],
        transient: search.errors.iter().any(|(_, e)| e.is_transient()),
        summary: String::new(),
    };
    let name = |id: i64| {
        projects.iter().find(|p| p.id == id).map_or_else(
            || format!("project {id}"),
            |p| format!("{}/{}", p.owner, p.name),
        )
    };
    for (project, error) in &search.errors {
        refreshed
            .problems
            .push(format!("{}: {error}", name(*project)));
    }
    let limit = store.concurrency_limit().await.map_err(|e| e.to_string())?;
    let mut blocked_projects: Vec<i64> = vec![];
    let mut considered = 0;
    let mut waiting = 0;
    for candidate in &search.candidates {
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        if blocked_projects.contains(&candidate.project_id) {
            continue;
        }
        let Some(project) = projects.iter().find(|p| p.id == candidate.project_id) else {
            continue;
        };
        // Spend no GitHub calls on gates when every slot is already taken.
        let reserved = store
            .summary()
            .await
            .map_err(|e| e.to_string())?
            .reserved_slots;
        if reserved >= i64::from(limit) {
            waiting += 1;
            continue;
        }
        let identity = IssueIdentity {
            id: candidate.issue_id.clone(),
            number: candidate.number,
        };
        let checking = match store
            .begin_checking(project.id, identity, owner.into())
            .await
        {
            Ok(checking) => checking,
            // Started before, or being checked: never duplicate (decision D4).
            Err(StoreError::AlreadyStarted | StoreError::DuplicateIssue) => continue,
            Err(error) => {
                refreshed.problems.push(format!(
                    "{}#{}: {error}",
                    name(project.id),
                    candidate.number
                ));
                continue;
            }
        };
        if running.contains(&checking.id) {
            continue;
        }
        considered += 1;
        let check = start::check(github, project, candidate.number, owner).await;
        if !check.eligible() {
            let message = if check.gates.is_empty() {
                "Start eligibility could not be established.".into()
            } else {
                check.summary()
            };
            if check.gates.iter().any(start::Gate::blocks_project) {
                blocked_projects.push(project.id);
                refreshed.problems.push(format!(
                    "{}: {message} New work for this project is blocked.",
                    name(project.id)
                ));
            }
            refreshed.transient |= check.gates.iter().any(start::Gate::is_transient);
            let _ = store
                .fail_step(
                    checking.id,
                    StepError {
                        transition: Step::Working,
                        message,
                        detail: None,
                        retryable: true,
                    },
                )
                .await;
            continue;
        }
        let unverified = |message: String| StepError {
            transition: Step::Working,
            message,
            detail: None,
            retryable: true,
        };
        let (Some(issue), Some(default_branch)) =
            (check.issue.clone(), check.default_branch.clone())
        else {
            let _ = store
                .fail_step(
                    checking.id,
                    unverified("GitHub did not report the issue or default branch.".into()),
                )
                .await;
            continue;
        };
        if stopping.load(Ordering::SeqCst) {
            let _ = store
                .fail_step(
                    checking.id,
                    unverified("Nightshift quit before this issue started.".into()),
                )
                .await;
            break;
        }
        match coordinator
            .enter_working(checking.id, issue, check.parent.clone())
            .await
        {
            Ok(Entry::Entered(execution)) => {
                let execution = store
                    .record_working(
                        execution.id,
                        WorkingEvidence {
                            default_branch: Some(default_branch),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap_or(execution);
                refreshed.started += 1;
                // Hand the execution to the actor immediately so shutdown can always reach it.
                let _ = inbox
                    .send(Message::Started(Box::new((execution, project.clone()))))
                    .await;
            }
            Ok(Entry::WaitingForCapacity(_)) => waiting += 1,
            Err(error) => {
                refreshed.problems.push(format!(
                    "{}#{}: {error}",
                    name(project.id),
                    candidate.number
                ));
                let _ = store
                    .fail_step(
                        checking.id,
                        unverified(format!("Could not reserve a slot: {error}")),
                    )
                    .await;
            }
        }
    }
    // Candidates left pending at checking are re-checked on the next refresh.
    let pending = store
        .executions()
        .await
        .map(|all| {
            all.iter()
                .filter(|e| e.step == Step::Checking && e.status == Status::Pending)
                .count()
        })
        .unwrap_or(0);
    refreshed.summary = format!(
        "{} candidates, {considered} checked, {} started, {} waiting for a slot",
        search.candidates.len(),
        refreshed.started,
        waiting.max(pending)
    );
    Ok(refreshed)
}
