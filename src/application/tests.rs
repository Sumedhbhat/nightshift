//! End-to-end workflow tests: orchestrator, storage, worktrees, a fake `gh`, and a scripted
//! Codex app server, across two projects backed by local bare remotes.
use super::{
    execution::Services,
    run::{self, Command, Update},
};
use crate::{
    codex::CodexCommand,
    domain::{Execution, NewProject, Outcome, Status, Step},
    git::testing::{git, repository},
    github::{
        GitHub,
        fake::{self, FakeGh},
    },
    storage::{Error as StoreError, Store},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

const ME: &str = "octo";

fn issue(repo: &str, number: u32) -> Value {
    json!({
        "node_id": format!("I_{repo}_{number}"), "number": number, "title": format!("Task {number} in {repo}"),
        "body": format!("Do task {number}"), "state": "open", "state_reason": null,
        "labels": [{"name": "ready-for-agent"}], "assignees": [{"login": ME}],
        "parent_issue_url": null,
        "issue_dependencies_summary": {"blocked_by": 0, "total_blocked_by": 0},
        "sub_issues_summary": {"total": 0},
        "repository_url": format!("https://api.github.com/repos/o/{repo}"),
        "html_url": format!("https://github.com/o/{repo}/issues/{number}")
    })
}

/// A Codex stand-in: answers the handshake, thread, and turn, edits the worktree, then waits
/// for `release` before completing the turn.
fn codex_script(directory: &Path, release: &Path) -> CodexCommand {
    let script = directory.join("fake-codex.sh");
    std::fs::write(
        &script,
        format!(
            r#"read l; echo '{{"id":1,"result":{{}}}}'
read l
read l; echo '{{"id":2,"result":{{"thread":{{"id":"thr"}}}}}}'
read l; echo '{{"id":3,"result":{{"turn":{{"id":"turn","status":"inProgress","items":[]}}}}}}'
echo "change by $$" > "nightshift-change.txt"
echo '{{"method":"item/completed","params":{{"threadId":"thr","turnId":"turn","completedAtMs":1,"item":{{"type":"agentMessage","id":"m","text":"Edited the file."}}}}}}'
while [ ! -e '{release}' ]; do sleep 0.05; done
echo '{{"method":"turn/completed","params":{{"threadId":"thr","turn":{{"id":"turn","status":"completed","items":[]}}}}}}'
while read l; do :; done
"#,
            release = release.display()
        ),
    )
    .unwrap();
    CodexCommand {
        program: "/bin/sh".into(),
        args: vec![script.to_str().unwrap().into()],
    }
}

struct World {
    _root: tempfile::TempDir,
    _repos: Vec<tempfile::TempDir>,
    remotes: Vec<PathBuf>,
    release: PathBuf,
    store: Store,
    fake: Arc<FakeGh>,
    handle: run::Handle,
    updates: mpsc::Receiver<Update>,
}

/// Projects `a` (issues 1, 2) and `b` (issue 3), all eligible, with the given limit.
async fn world(limit: u32) -> World {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("data")).await.unwrap();
    store.set_concurrency_limit(limit).await.unwrap();
    let fake = FakeGh::signed_in(ME);
    let mut repos = vec![];
    let mut remotes = vec![];
    for (name, issues) in [("a", vec![1, 2]), ("b", vec![3])] {
        let (dir, remote, checkout) = repository();
        git(&checkout, &["config", "user.name", "Fixture"]);
        git(&checkout, &["config", "user.email", "fixture@example.com"]);
        store
            .add_project(NewProject {
                checkout_path: checkout.to_str().unwrap().into(),
                github_host: "github.com".into(),
                repository_id: format!("R_{name}"),
                owner: "o".into(),
                name: name.into(),
                remote: "origin".into(),
                setup_command: Some("echo setup ran".into()),
            })
            .await
            .unwrap();
        fake.get(
            &format!("repos/o/{name}"),
            json!({"node_id": format!("R_{name}"), "name": name, "owner": {"login": "o"},
                   "default_branch": "main", "permissions": {"push": true}}),
        );
        fake.get_all(
            &format!("repos/o/{name}/issues?state=open&assignee={ME}&labels=ready-for-agent&per_page=100&sort=created&direction=asc"),
            json!([issues.iter().map(|n| issue(name, *n)).collect::<Vec<_>>()]),
        );
        for number in issues {
            fake.get(
                &format!("repos/o/{name}/issues/{number}"),
                issue(name, number),
            );
            fake.get_all(
                &format!("repos/o/{name}/issues/{number}/dependencies/blocked_by?per_page=100"),
                json!([[]]),
            );
        }
        fake.respond_prefix(
            &format!("api --hostname github.com --method GET repos/o/{name}/pulls?head="),
            fake::ok(json!([])),
        );
        fake.respond_prefix(
            &format!("api --hostname github.com --method POST repos/o/{name}/pulls --input -"),
            fake::ok(json!({"number": 40, "html_url": format!("https://github.com/o/{name}/pull/40"), "node_id": "PR_x", "head": {"ref": "x"}})),
        );
        repos.push(dir);
        remotes.push(remote);
    }
    let release = root.path().join("release");
    let services = Services {
        store: store.clone(),
        github: GitHub::new(fake.gh()),
        codex: codex_script(root.path(), &release),
        worktrees_root: root.path().join("worktrees"),
    };
    let (sender, updates) = mpsc::channel(1_024);
    let handle = run::spawn(services, sender);
    World {
        _root: root,
        _repos: repos,
        remotes,
        release,
        store,
        fake,
        handle,
        updates,
    }
}

impl World {
    async fn executions(&self) -> Vec<Execution> {
        self.store.executions().await.unwrap()
    }

    /// Poll storage until `done` holds, draining UI updates meanwhile.
    async fn until(&mut self, what: &str, done: impl Fn(&[Execution]) -> bool) -> Vec<Execution> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            while self.updates.try_recv().is_ok() {}
            let executions = self.executions().await;
            if done(&executions) {
                return executions;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {executions:#?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn candidate_listings(&self) -> usize {
        self.fake
            .calls()
            .iter()
            .filter(|c| c.contains("labels=ready-for-agent"))
            .count()
    }
}

fn running(executions: &[Execution]) -> usize {
    executions
        .iter()
        .filter(|e| e.step == Step::Working && e.status == Status::Running)
        .count()
}

fn published(executions: &[Execution]) -> usize {
    executions
        .iter()
        .filter(|e| e.outcome == Some(Outcome::PullRequestPublished))
        .count()
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn run_fills_the_limit_across_projects_blocks_extra_and_duplicate_starts_then_publishes() {
    block_on(async {
        let mut world = world(2).await;
        assert!(world.handle.send(Command::StartRun));
        let executions = world.until("two running", |e| running(e) == 2).await;
        // The third candidate waits: with every slot taken, it is not even checked.
        assert_eq!(executions.len(), 2);
        assert_eq!(world.store.summary().await.unwrap().reserved_slots, 2);
        // A manual refresh while full starts nothing more.
        world.handle.send(Command::Refresh);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(running(&world.executions().await), 2);
        assert_eq!(world.store.summary().await.unwrap().reserved_slots, 2);
        // Duplicate starts of a running issue are rejected, even from another checkout.
        let started = executions.iter().find(|e| e.step == Step::Working).unwrap();
        let other_project = if started.project_id == 1 { 2 } else { 1 };
        assert!(matches!(
            world
                .store
                .begin_checking(other_project, started.issue.clone(), ME.into())
                .await,
            Err(StoreError::AlreadyStarted)
        ));
        // Release Codex: the two finish, publish, and free capacity for the third.
        std::fs::write(&world.release, "go").unwrap();
        let done = world
            .until("all three published", |e| published(e) == 3)
            .await;
        for execution in &done {
            assert_eq!(execution.step, Step::PullRequestPublished);
            assert!(!execution.holds_slot());
            let branch = execution.working.branch.as_deref().unwrap();
            let remote = &world.remotes[(execution.project_id - 1) as usize];
            let pushed = git(remote, &["rev-parse", &format!("refs/heads/{branch}")]);
            assert_eq!(
                Some(pushed.as_str()),
                execution.publication.pushed_commit.as_deref()
            );
            let files = git(remote, &["show", "--name-only", "--format=", &pushed]);
            assert_eq!(
                files, "nightshift-change.txt",
                "the commit contains Codex's change"
            );
            let base = execution.working.base_commit.as_deref().unwrap();
            assert_eq!(
                git(remote, &["rev-parse", &format!("{pushed}^")]),
                base,
                "branch starts at the fetched default branch"
            );
            let output = world.store.output(execution.id, 0).await.unwrap();
            assert!(output.iter().any(|o| o.payload == "setup ran"));
            assert!(output.iter().any(|o| o.payload == "Edited the file."));
        }
        let posts = world
            .fake
            .calls()
            .iter()
            .filter(|c| c.contains("--method POST"))
            .count();
        assert_eq!(posts, 3, "one pull request per execution");
        // Further refreshes never restart issues that already ran (decision D4).
        world.handle.send(Command::Refresh);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(world.executions().await.len(), 3);
        world.handle.shutdown().await;
        world.store.clone().close().await.unwrap();
    });
}

#[test]
fn completion_without_an_active_run_triggers_no_search_or_start() {
    block_on(async {
        let mut world = world(1).await;
        world.handle.send(Command::StartRun);
        world.until("one running", |e| running(e) == 1).await;
        world.handle.send(Command::EndRun);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let listings = world.candidate_listings();
        std::fs::write(&world.release, "go").unwrap();
        world.until("first published", |e| published(e) == 1).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(
            world.candidate_listings(),
            listings,
            "no refresh after the run ended"
        );
        let executions = world.executions().await;
        assert_eq!(running(&executions), 0);
        assert_eq!(
            executions
                .iter()
                .filter(|e| e.step == Step::Working)
                .count(),
            0
        );
        world.handle.shutdown().await;
        world.store.clone().close().await.unwrap();
    });
}

#[test]
fn quitting_interrupts_active_work_and_releases_slots_after_processes_stop() {
    block_on(async {
        let mut world = world(2).await;
        world.handle.send(Command::StartRun);
        let executions = world
            .until("two Codex turns running", |e| {
                e.iter()
                    .filter(|e| e.working.codex_turn_id.is_some())
                    .count()
                    == 2
            })
            .await;
        let worktrees: Vec<PathBuf> = executions
            .iter()
            .filter_map(|e| e.working.worktree_path.clone().map(PathBuf::from))
            .collect();
        world.handle.shutdown().await;
        let executions = world.executions().await;
        let interrupted: Vec<_> = executions
            .iter()
            .filter(|e| e.status == Status::Interrupted)
            .collect();
        assert_eq!(interrupted.len(), 2);
        for execution in interrupted {
            assert!(
                !execution.holds_slot(),
                "processes stopped, so slots are released"
            );
            assert!(execution.snapshot.is_some());
        }
        for path in worktrees {
            assert!(
                path.join("nightshift-change.txt").exists(),
                "worktrees are preserved"
            );
        }
        // Nothing is left running from the fake Codex.
        let ps = std::process::Command::new("ps")
            .args(["-A", "-o", "args="])
            .output()
            .unwrap();
        let script = world._root.path().join("fake-codex.sh");
        assert!(!String::from_utf8_lossy(&ps.stdout).contains(script.to_str().unwrap()));
        world.store.clone().close().await.unwrap();
    });
}

#[test]
fn retry_working_continues_an_interrupted_execution_and_recheck_publication_recovers() {
    block_on(async {
        let mut world = world(1).await;
        world.handle.send(Command::StartRun);
        // Interrupt only after Codex is running, so the worktree and thread are recorded.
        let first = world
            .until("Codex running", |e| {
                e.iter().any(|e| e.working.codex_turn_id.is_some())
            })
            .await;
        let id = first.iter().find(|e| e.step == Step::Working).unwrap().id;
        world.handle.send(Command::EndRun);
        // Interrupt by restarting the orchestrator: shut down, then start a new one.
        world.handle.shutdown().await;
        assert_eq!(
            world.store.execution(id).await.unwrap().status,
            Status::Interrupted
        );
        let services = Services {
            store: world.store.clone(),
            github: GitHub::new(world.fake.gh()),
            codex: codex_script(world._root.path(), &world.release),
            worktrees_root: world._root.path().join("worktrees"),
        };
        let (sender, updates) = mpsc::channel(1_024);
        world.handle = run::spawn(services, sender);
        world.updates = updates;
        // Make the push fail once, then succeed after the owner rechecks.
        let remote = world.remotes[0].clone();
        let hidden = remote.with_extension("hidden");
        std::fs::rename(&remote, &hidden).unwrap();
        std::fs::write(&world.release, "go").unwrap();
        world.handle.send(Command::RetryWorking(id));
        let failed = world
            .until("publication error", |e| {
                e.iter().any(|e| e.id == id && e.status == Status::Error)
            })
            .await;
        let execution = failed.iter().find(|e| e.id == id).unwrap();
        assert_eq!(execution.step, Step::Working);
        assert!(
            execution
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("Push failed"),
            "{:?}",
            execution.error
        );
        assert!(execution.working.work_finished_at.is_some());
        // No automatic retry happens.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            world.store.execution(id).await.unwrap().status,
            Status::Error
        );
        std::fs::rename(&hidden, &remote).unwrap();
        world.handle.send(Command::RecheckPublication(id));
        let done = world
            .until("published after recheck", |e| published(e) == 1)
            .await;
        assert_eq!(done.iter().find(|e| e.id == id).unwrap().error, None);
        let events = world.store.events(id, 0).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.reason.starts_with("Owner retry: working attempt 2"))
        );
        world.handle.shutdown().await;
        world.store.clone().close().await.unwrap();
    });
}

#[test]
fn account_switch_blocks_new_starts_without_touching_active_work() {
    block_on(async {
        let mut world = world(1).await;
        world.handle.send(Command::StartRun);
        world.until("one running", |e| running(e) == 1).await;
        world.fake.set(
            "auth status --hostname github.com --json hosts",
            fake::ok(fake::auth_status("intruder")),
        );
        std::fs::write(&world.release, "go").unwrap();
        // The active execution still finishes and publishes; publication checks only open state.
        world.until("first published", |e| published(e) == 1).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let executions = world.executions().await;
        assert_eq!(
            executions
                .iter()
                .filter(|e| e.step >= Step::Working)
                .count(),
            1,
            "no new start as another account"
        );
        world.handle.shutdown().await;
        world.store.clone().close().await.unwrap();
    });
}
