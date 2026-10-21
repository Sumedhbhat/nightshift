//! One execution's local work and publication. Capacity is acquired by the coordinator before
//! this runs; this module only ends attempts, which releases capacity once processes stop.
use crate::{
    application::ExecutionRecorder,
    codex::{self, CodexCommand, Session, TurnOutcome, TurnRequest, TurnSink},
    domain::{Execution, OutputSource, Project, Step, StepError, WorkingEvidence},
    git::{self, worktree},
    github::{GitHub, cli::BoxFuture},
    storage::{AttemptEnd, Store},
};
use std::{path::PathBuf, time::Duration};
use tokio::sync::watch;

/// Everything an execution needs from the outside world.
#[derive(Clone)]
pub struct Services {
    pub store: Store,
    pub github: GitHub,
    pub codex: CodexCommand,
    pub worktrees_root: PathBuf,
}

struct StoreSink {
    store: Store,
    id: i64,
}

impl TurnSink for StoreSink {
    fn thread(&self, thread_id: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.store
                .record_working(
                    self.id,
                    WorkingEvidence {
                        codex_thread_id: Some(thread_id),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn turn(&self, turn_id: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.store
                .record_working(
                    self.id,
                    WorkingEvidence {
                        codex_turn_id: Some(turn_id),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn output(&self, blocks: Vec<(OutputSource, String)>) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.store
                .append_output_batch(self.id, blocks)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

fn working_error(message: impl Into<String>, detail: Option<String>) -> AttemptEnd {
    AttemptEnd::Failed(StepError {
        transition: Step::Working,
        message: message.into(),
        detail,
        retryable: true,
    })
}

async fn note(store: &Store, id: i64, text: String) {
    let _ = store.append_output(id, OutputSource::System, text).await;
}

/// Run local work for an execution that holds a running slot attempt, then publish.
/// `stop` is set when the owner quits; work then ends as interrupted.
pub async fn run(
    services: Services,
    execution: Execution,
    project: Project,
    stop: watch::Receiver<bool>,
) {
    let store = services.store.clone();
    let id = execution.id;
    let end = local_work(&services, &execution, &project, stop).await;
    let (end, stopped) = end;
    let finished = matches!(end, AttemptEnd::Finished { .. });
    if let Err(error) = store.end_working_attempt(id, end, stopped).await {
        eprintln!("nightshift: could not record the end of execution {id}: {error}");
        return;
    }
    if finished {
        publish(
            &services,
            id,
            &project,
            "Local work finished; checking publication eligibility",
        )
        .await;
    }
}

/// Returns how the attempt ended and whether every owned process is confirmed stopped.
async fn local_work(
    services: &Services,
    execution: &Execution,
    project: &Project,
    stop: watch::Receiver<bool>,
) -> (AttemptEnd, bool) {
    let store = &services.store;
    let id = execution.id;
    let Some(issue) = execution.snapshot.clone() else {
        return (
            working_error("The execution has no saved issue snapshot.", None),
            true,
        );
    };
    let interrupted = || AttemptEnd::Interrupted {
        reason: "Nightshift is quitting".into(),
    };
    let request = worktree::Request {
        checkout: project.checkout_path.clone().into(),
        remote: project.remote.clone(),
        default_branch: execution.working.default_branch.clone().unwrap_or_default(),
        worktrees_root: services.worktrees_root.clone(),
        repository: format!("{}-{}", project.owner, project.name),
        issue_number: issue.number,
        issue_title: issue.title.clone(),
        execution_id: id,
        setup_command: project.setup_command.clone(),
        existing: execution.working.clone(),
    };
    let recorder = ExecutionRecorder {
        store: store.clone(),
        execution_id: id,
    };
    // Preparation is cancelled by dropping it, which kills a running setup command.
    let prepared = {
        let mut stopping = stop.clone();
        let preparing = std::pin::pin!(worktree::prepare(&request, &recorder));
        let stopped = std::pin::pin!(async move {
            while !*stopping.borrow() {
                if stopping.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        });
        match first(preparing, stopped).await {
            Either::Left(result) => result,
            Either::Right(()) => return (interrupted(), true),
        }
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => {
            return (
                working_error(
                    format!(
                        "Preparation failed at {:?}: {}",
                        failure.stage, failure.message
                    ),
                    None,
                ),
                true,
            );
        }
    };
    // Shutdown may have begun while setup finished; never launch Codex after that.
    if *stop.borrow() {
        return (interrupted(), true);
    }
    let mut session = match Session::spawn(&services.codex, &prepared.worktree) {
        Ok(session) => session,
        Err(error) => return (working_error(error.to_string(), None), true),
    };
    if let Some(pid) = session.process_id() {
        let _ = store.record_process(id, pid).await;
    }
    let repository = format!("{}/{}", project.owner, project.name);
    let turn = TurnRequest {
        cwd: prepared.worktree.clone(),
        prompt: codex::prompt(&repository, &issue, execution.parent.as_ref()),
        resume_thread: execution.working.codex_thread_id.clone(),
    };
    let sink = StoreSink {
        store: store.clone(),
        id,
    };
    let outcome = match session.handshake().await {
        Ok(()) => session.run_turn(&turn, &sink, stop.clone()).await,
        Err(error) => Err(error),
    };
    let stopped = session.shutdown(Some(&prepared.worktree)).await;
    if !stopped {
        note(
            store,
            id,
            "Codex processes did not stop; the slot stays reserved until they do.".into(),
        )
        .await;
    }
    let end = match outcome {
        Ok(TurnOutcome::Completed) => {
            match commit(&prepared.worktree, &issue.title, issue.number).await {
                Ok(Some(commit)) => AttemptEnd::Finished {
                    result: format!("completed; committed {commit}"),
                },
                Ok(None) => working_error("Codex finished without changing any files.", None),
                Err(error) => working_error("Could not commit Codex's changes.", Some(error)),
            }
        }
        Ok(TurnOutcome::Interrupted) if *stop.borrow() => interrupted(),
        Ok(TurnOutcome::Interrupted) => working_error("The Codex turn was interrupted.", None),
        Ok(TurnOutcome::Failed(message)) => {
            working_error(format!("The Codex turn failed: {message}"), None)
        }
        Err(error) if *stop.borrow() => AttemptEnd::Interrupted {
            reason: format!("Nightshift is quitting ({error})"),
        },
        Err(error) => working_error(error.to_string(), None),
    };
    (end, stopped)
}

/// Commit everything Codex changed in the worktree with the owner's Git identity.
/// Returns the new commit, or None when nothing changed.
async fn commit(
    worktree: &std::path::Path,
    title: &str,
    number: u32,
) -> Result<Option<String>, String> {
    let timeout = Duration::from_secs(60);
    git::run(worktree, &["add", "--all"], timeout).await?;
    let staged = git::command(worktree)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if staged.success() {
        return Ok(None);
    }
    let message = format!("{title} (#{number})\n\nImplemented by Codex through Nightshift.");
    git::run(
        worktree,
        &["commit", "--no-verify", "--no-gpg-sign", "-m", &message],
        timeout,
    )
    .await?;
    Ok(Some(
        git::run(worktree, &["rev-parse", "HEAD"], timeout)
            .await?
            .stdout,
    ))
}

/// Publication: verify the issue is open, push the branch, then create or recover the pull
/// request. Every failure stops and is stored for an explicit owner recheck; nothing retries.
pub async fn publish(services: &Services, id: i64, project: &Project, reason: &str) {
    let store = &services.store;
    let execution = match store.begin_publication(id, reason.into()).await {
        Ok(execution) => execution,
        Err(error) => {
            eprintln!("nightshift: publication of execution {id} cannot start: {error}");
            return;
        }
    };
    let Some(issue) = execution.snapshot.clone() else {
        return;
    };
    let fail = |transition: Step, message: String| StepError {
        transition,
        message,
        detail: None,
        retryable: true,
    };
    if execution.step == Step::Working {
        match services.github.issue_open(project, issue.number).await {
            Ok(true) => {}
            Ok(false) => {
                let _ = store
                    .skip_publication(
                        id,
                        format!(
                            "Issue #{} is closed; the branch was not published.",
                            issue.number
                        ),
                    )
                    .await;
                return;
            }
            Err(error) => {
                let message = format!(
                    "Could not verify that issue #{} is open: {error}",
                    issue.number
                );
                let _ = store
                    .fail_step(id, fail(Step::BranchPublished, message))
                    .await;
                return;
            }
        }
        let (Some(branch), Some(path)) =
            (&execution.working.branch, &execution.working.worktree_path)
        else {
            let _ = store
                .fail_step(
                    id,
                    fail(
                        Step::BranchPublished,
                        "No branch or worktree is recorded.".into(),
                    ),
                )
                .await;
            return;
        };
        let worktree = std::path::Path::new(path);
        let target = format!("refs/heads/{branch}");
        note(
            store,
            id,
            format!("Pushing {branch} to {}…", project.remote),
        )
        .await;
        let pushed = async {
            let commit = git::run(worktree, &["rev-parse", "HEAD"], Duration::from_secs(30))
                .await?
                .stdout;
            git::run(
                worktree,
                &[
                    "push",
                    "--porcelain",
                    "--no-verify",
                    "--",
                    &project.remote,
                    &format!("{commit}:{target}"),
                ],
                git::NETWORK_TIMEOUT,
            )
            .await?;
            // Evidence of the exact remote branch, read back from the remote.
            let remote = git::run(
                worktree,
                &["ls-remote", "--", &project.remote, &target],
                git::NETWORK_TIMEOUT,
            )
            .await?
            .stdout;
            if !remote.starts_with(&commit) {
                return Err(format!(
                    "{} reports {target} at `{remote}`, not {commit}.",
                    project.remote
                ));
            }
            Ok(commit)
        }
        .await;
        match pushed {
            Ok(commit) => {
                if let Err(error) = store
                    .record_branch_published(id, project.remote.clone(), target, commit)
                    .await
                {
                    eprintln!(
                        "nightshift: could not record the published branch for {id}: {error}"
                    );
                    return;
                }
            }
            Err(error) => {
                let _ = store
                    .fail_step(
                        id,
                        fail(Step::BranchPublished, format!("Push failed: {error}")),
                    )
                    .await;
                return;
            }
        }
    }
    let Ok(execution) = store.execution(id).await else {
        return;
    };
    let branch = execution.working.branch.clone().unwrap_or_default();
    let base = execution.working.default_branch.clone().unwrap_or_default();
    let existing = match services.github.pull_request_for(project, &branch).await {
        Ok(existing) => existing,
        Err(error) => {
            let message = format!("Could not check for an existing pull request: {error}");
            let _ = store
                .fail_step(id, fail(Step::PullRequestPublished, message))
                .await;
            return;
        }
    };
    let pull = match existing {
        Some(pull) => {
            note(
                store,
                id,
                format!("Recovered existing pull request #{}.", pull.number),
            )
            .await;
            Ok(pull)
        }
        None => {
            let parent = execution
                .parent
                .as_ref()
                .map(|p| format!("\n\nParent issue: #{} {}", p.number, p.title))
                .unwrap_or_default();
            let body = format!(
                "Refs #{number}.{parent}\n\nImplemented by Codex through Nightshift from `{base}` at `{commit}`. \
                 Review before merging; close #{number} after merge if GitHub does not.",
                number = issue.number,
                commit = execution
                    .working
                    .base_commit
                    .as_deref()
                    .unwrap_or("unknown"),
            );
            services
                .github
                .create_pull_request(
                    project,
                    &branch,
                    &base,
                    &format!("{} (#{})", issue.title, issue.number),
                    &body,
                )
                .await
        }
    };
    match pull {
        Ok(pull) => {
            let _ = store
                .record_pull_request_published(id, pull.number, pull.html_url, pull.node_id)
                .await;
        }
        Err(error) => {
            let message = format!("Pull request creation failed: {error}");
            let _ = store
                .fail_step(id, fail(Step::PullRequestPublished, message))
                .await;
        }
    }
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

/// Resolve with whichever future finishes first, dropping the other.
async fn first<L, R>(
    mut left: std::pin::Pin<&mut impl std::future::Future<Output = L>>,
    mut right: std::pin::Pin<&mut impl std::future::Future<Output = R>>,
) -> Either<L, R> {
    std::future::poll_fn(move |cx| {
        if let std::task::Poll::Ready(value) = left.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Left(value));
        }
        if let std::task::Poll::Ready(value) = right.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Right(value));
        }
        std::task::Poll::Pending
    })
    .await
}
