use super::*;
use std::{
    future::Future,
    sync::{Arc, Barrier},
};

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_time()
        .build()
        .unwrap()
        .block_on(test);
}

fn project(path: &str) -> NewProject {
    NewProject {
        checkout_path: path.into(),
        github_host: "github.com".into(),
        repository_id: "R_1".into(),
        owner: "owner".into(),
        name: "repo".into(),
        remote: "origin".into(),
        setup_command: Some("echo setup".into()),
    }
}

fn identity(number: u32) -> IssueIdentity {
    IssueIdentity {
        id: format!("I_{number}"),
        number,
    }
}

fn snapshot(number: u32) -> IssueSnapshot {
    IssueSnapshot {
        id: format!("I_{number}"),
        number,
        title: format!("Issue {number}"),
        description: format!("Requirements for {number}"),
    }
}

fn error(transition: Step, message: &str) -> StepError {
    StepError {
        transition,
        message: message.into(),
        detail: Some("diagnostic".into()),
        retryable: true,
    }
}

fn entered(entry: Entry) -> Execution {
    match entry {
        Entry::Entered(execution) => execution,
        Entry::WaitingForCapacity(execution) => panic!("expected capacity, got {execution:?}"),
    }
}

/// Database-level helper: check and enter working in one call.
fn start(
    db: &mut Database,
    project: i64,
    number: u32,
    parent: Option<u32>,
    limit: u32,
) -> Result<Entry> {
    let checking = db.begin_checking(project, &identity(number), "owner-login")?;
    db.enter_working(
        checking.id,
        &snapshot(number),
        parent.map(snapshot).as_ref(),
        limit,
    )
}

fn open(directory: &tempfile::TempDir) -> Database {
    Database::open(&directory.path().join("db.sqlite3")).unwrap()
}

#[test]
fn worker_persists_workflow_snapshots_output_and_audit_across_reopen() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store
            .add_project(project("/fixture/checkout"))
            .await
            .unwrap();
        assert_eq!(project.setup_command.as_deref(), Some("echo setup"));
        let checking = store
            .begin_checking(project.id, identity(1), "owner-login".into())
            .await
            .unwrap();
        assert_eq!(
            (checking.step, checking.status),
            (Step::Checking, Status::Running)
        );
        assert_eq!(checking.snapshot, None);
        let working = entered(
            store
                .enter_working(checking.id, snapshot(1), Some(snapshot(10)), Some(2))
                .await
                .unwrap(),
        );
        assert_eq!(working.step, Step::Working);
        assert!(working.holds_slot());
        store
            .append_output(working.id, OutputSource::Agent, "Saved agent output".into())
            .await
            .unwrap();
        store
            .end_working_attempt(
                working.id,
                AttemptEnd::Interrupted {
                    reason: "Application quitting".into(),
                },
                false,
            )
            .await
            .unwrap();
        store.close().await.unwrap();

        let store = Store::open(directory.path().into()).await.unwrap();
        let saved = store.execution(working.id).await.unwrap();
        assert_eq!(saved.snapshot, Some(snapshot(1)));
        assert_eq!(saved.parent, Some(snapshot(10)));
        assert_eq!(saved.status, Status::Interrupted);
        assert!(
            saved.holds_slot(),
            "interruption keeps capacity until stop is confirmed"
        );
        assert_eq!(
            store.output(saved.id, 0).await.unwrap()[0].payload,
            "Saved agent output"
        );
        let events = store.events(saved.id, 0).await.unwrap();
        assert_eq!(
            events.iter().map(|e| e.status.as_str()).collect::<Vec<_>>(),
            ["running", "running", "interrupted"]
        );
        assert_eq!(store.summary().await.unwrap().reserved_slots, 1);
        store.close().await.unwrap();
    });
}

#[test]
fn simultaneous_starts_fill_the_limit_across_projects_and_wait_beyond_it() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        store.set_concurrency_limit(3).await.unwrap();
        let a = store.add_project(project("/fixture/a")).await.unwrap();
        let mut other = project("/fixture/b");
        other.repository_id = "R_2".into();
        let b = store.add_project(other).await.unwrap();
        let starts = (1..=5).map(|number| {
            let store = store.clone();
            let project = if number % 2 == 0 { a.id } else { b.id };
            tokio::spawn(async move {
                let checking = store
                    .begin_checking(project, identity(number), "owner-login".into())
                    .await
                    .unwrap();
                store
                    .enter_working(checking.id, snapshot(number), None, None)
                    .await
                    .unwrap()
            })
        });
        let mut results = vec![];
        for start in starts.collect::<Vec<_>>() {
            results.push(start.await.unwrap());
        }
        let entered = results
            .iter()
            .filter(|r| matches!(r, Entry::Entered(_)))
            .count();
        let waiting: Vec<_> = results
            .iter()
            .filter_map(|r| match r {
                Entry::WaitingForCapacity(e) => Some(e),
                Entry::Entered(_) => None,
            })
            .collect();
        assert_eq!(entered, 3);
        assert_eq!(waiting.len(), 2);
        for execution in waiting {
            assert_eq!(
                (execution.step, execution.status),
                (Step::Checking, Status::Pending)
            );
            assert_eq!(execution.snapshot, None, "no snapshot without capacity");
            assert!(!execution.holds_slot());
        }
        assert_eq!(store.summary().await.unwrap().reserved_slots, 3);
        store.close().await.unwrap();
    });
}

#[test]
fn independent_connections_cannot_overbook_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.sqlite3");
    let mut first = Database::open(&path).unwrap();
    let project = first.add_project(project("/fixture/checkout")).unwrap();
    let mut second = Database::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let a = std::thread::spawn(move || {
        other.wait();
        start(&mut first, project.id, 1, None, 1)
    });
    let b = std::thread::spawn(move || {
        barrier.wait();
        start(&mut second, project.id, 2, None, 1)
    });
    let results = [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Entry::Entered(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Entry::WaitingForCapacity(_)))
            .count(),
        1
    );
}

#[test]
fn duplicate_issue_is_rejected_across_checkouts_and_after_completion() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let first = db.add_project(project("/fixture/one")).unwrap();
    let second = db.add_project(project("/fixture/two")).unwrap();
    let checking = db
        .begin_checking(first.id, &identity(7), "owner-login")
        .unwrap();
    assert!(matches!(
        db.begin_checking(second.id, &identity(7), "owner-login"),
        Err(Error::DuplicateIssue)
    ));
    entered(
        db.enter_working(checking.id, &snapshot(7), None, 4)
            .unwrap(),
    );
    assert!(matches!(
        db.begin_checking(second.id, &identity(7), "owner-login"),
        Err(Error::AlreadyStarted)
    ));
    // Even after the slot is released, the issue is never started again (decision D4).
    db.end_working_attempt(
        checking.id,
        &AttemptEnd::Finished {
            result: "completed".into(),
        },
        true,
    )
    .unwrap();
    assert!(matches!(
        db.begin_checking(first.id, &identity(7), "owner-login"),
        Err(Error::AlreadyStarted)
    ));
    // Sibling issues of the same parent are independent: no parent-wide slot.
    entered(start(&mut db, first.id, 8, Some(100), 4).unwrap());
    entered(start(&mut db, second.id, 9, Some(100), 4).unwrap());
}

#[test]
fn failed_checks_stay_at_checking_and_can_be_rechecked() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let checking = db
        .begin_checking(project.id, &identity(3), "owner-login")
        .unwrap();
    let failed = db
        .fail_step(
            checking.id,
            &error(Step::Working, "Blocked by #2, which is open"),
        )
        .unwrap();
    assert_eq!(
        (failed.step, failed.status),
        (Step::Checking, Status::Error)
    );
    assert_eq!(
        failed.error.as_ref().unwrap().message,
        "Blocked by #2, which is open"
    );
    assert!(matches!(
        db.enter_working(checking.id, &snapshot(3), None, 1),
        Err(Error::InvalidTransition)
    ));
    let rechecked = db
        .begin_checking(project.id, &identity(3), "owner-login")
        .unwrap();
    assert_eq!(rechecked.id, checking.id);
    assert_eq!((rechecked.status, rechecked.error), (Status::Running, None));
    entered(
        db.enter_working(checking.id, &snapshot(3), None, 1)
            .unwrap(),
    );
}

#[test]
fn capacity_follows_step_policies() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let id = db
        .begin_checking(project.id, &identity(1), "owner-login")
        .unwrap()
        .id;
    let expect = |db: &mut Database, step: Step| {
        let execution = db.execution(id).unwrap();
        assert_eq!(execution.step, step);
        assert_eq!(
            db.summary().unwrap().reserved_slots,
            i64::from(step.policy().consumes_execution_slot && execution.status == Status::Running),
            "slot accounting at {step:?} must follow its policy"
        );
    };
    expect(&mut db, Step::Checking);
    entered(db.enter_working(id, &snapshot(1), None, 1).unwrap());
    expect(&mut db, Step::Working);
    db.record_working(
        id,
        &WorkingEvidence {
            base_commit: Some("abc".into()),
            branch: Some("nightshift/1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    db.end_working_attempt(
        id,
        &AttemptEnd::Finished {
            result: "completed".into(),
        },
        true,
    )
    .unwrap();
    assert_eq!(
        db.summary().unwrap().reserved_slots,
        0,
        "finished local work frees the slot"
    );
    db.begin_publication(id, "Issue is open").unwrap();
    db.record_branch_published(id, "origin", "refs/heads/nightshift/1", "abc")
        .unwrap();
    expect(&mut db, Step::BranchPublished);
    db.record_pull_request_published(id, 12, "https://github.com/o/r/pull/12", "PR_1")
        .unwrap();
    expect(&mut db, Step::PullRequestPublished);
    let done = db.execution(id).unwrap();
    assert_eq!(
        (done.status, done.outcome),
        (Status::Ended, Some(Outcome::PullRequestPublished))
    );
    assert!(matches!(
        db.begin_publication(id, "again"),
        Err(Error::InvalidTransition)
    ));
}

#[test]
fn interrupted_attempt_holds_capacity_until_stop_is_confirmed_then_retry_reacquires() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let id = match start(&mut db, project.id, 1, None, 1).unwrap() {
        Entry::Entered(e) => e.id,
        other => panic!("{other:?}"),
    };
    db.record_process(id, 4242).unwrap();
    db.end_working_attempt(
        id,
        &AttemptEnd::Interrupted {
            reason: "Quit".into(),
        },
        false,
    )
    .unwrap();
    assert!(matches!(
        start(&mut db, project.id, 2, None, 1).unwrap(),
        Entry::WaitingForCapacity(_)
    ));
    assert!(matches!(
        db.retry_working(id, 1),
        Err(Error::ProcessNotStopped)
    ));
    // Late process output after interruption is still accepted until stop is confirmed.
    db.append_output(id, OutputSource::Stderr, "draining")
        .unwrap();
    db.confirm_stopped(id, "Process 4242 exited").unwrap();
    assert!(matches!(
        db.append_output(id, OutputSource::Agent, "late"),
        Err(Error::InvalidTransition)
    ));
    assert_eq!(db.summary().unwrap().reserved_slots, 0);
    let retried = entered(db.retry_working(id, 1).unwrap());
    assert_eq!(retried.slot_attempt.as_ref().unwrap().number, 2);
    assert_eq!(
        retried.snapshot,
        Some(snapshot(1)),
        "retry keeps the saved snapshot"
    );
    // A failed attempt with a confirmed stop is retryable and frees capacity immediately.
    db.end_working_attempt(
        id,
        &AttemptEnd::Failed(error(Step::Working, "Setup failed")),
        true,
    )
    .unwrap();
    assert_eq!(db.summary().unwrap().reserved_slots, 0);
    let failed = db.execution(id).unwrap();
    assert_eq!((failed.step, failed.status), (Step::Working, Status::Error));
}

#[test]
fn working_evidence_cannot_be_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let id = entered(start(&mut db, project.id, 1, None, 1).unwrap()).id;
    let evidence = WorkingEvidence {
        branch: Some("nightshift/1-a".into()),
        worktree_path: Some("/tmp/worktree".into()),
        ..Default::default()
    };
    db.record_working(id, &evidence).unwrap();
    db.record_working(id, &evidence).unwrap();
    assert!(matches!(
        db.record_working(
            id,
            &WorkingEvidence {
                branch: Some("other".into()),
                ..Default::default()
            }
        ),
        Err(Error::EvidenceConflict("branch"))
    ));
    assert_eq!(
        db.execution(id).unwrap().working.branch.as_deref(),
        Some("nightshift/1-a")
    );
}

#[test]
fn closed_issue_skips_publication_and_publication_errors_need_explicit_recheck() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    for (number, closed) in [(1, true), (2, false)] {
        let id = entered(start(&mut db, project.id, number, None, 2).unwrap()).id;
        db.end_working_attempt(
            id,
            &AttemptEnd::Finished {
                result: "completed".into(),
            },
            true,
        )
        .unwrap();
        db.begin_publication(id, "Publishing").unwrap();
        if closed {
            let ended = db.skip_publication(id, "Issue closed").unwrap();
            assert_eq!(ended.outcome, Some(Outcome::PublicationSkipped));
            assert!(matches!(
                db.begin_publication(id, "recheck"),
                Err(Error::InvalidTransition)
            ));
        } else {
            let failed = db
                .fail_step(id, &error(Step::BranchPublished, "Push rejected"))
                .unwrap();
            assert_eq!((failed.step, failed.status), (Step::Working, Status::Error));
            assert!(
                matches!(
                    db.record_branch_published(id, "origin", "refs/heads/x", "abc"),
                    Err(Error::InvalidTransition)
                ),
                "publication cannot continue without an explicit recheck"
            );
            db.begin_publication(id, "Owner rechecked publication")
                .unwrap();
            let published = db
                .record_branch_published(id, "origin", "refs/heads/x", "abc")
                .unwrap();
            assert_eq!(published.error, None);
        }
    }
}

#[test]
fn restart_reconciliation_interrupts_running_work_without_resuming() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let checking = db
        .begin_checking(project.id, &identity(1), "owner-login")
        .unwrap()
        .id;
    let working = entered(start(&mut db, project.id, 2, None, 3).unwrap()).id;
    let publishing = entered(start(&mut db, project.id, 3, None, 3).unwrap()).id;
    db.end_working_attempt(
        publishing,
        &AttemptEnd::Finished {
            result: "completed".into(),
        },
        true,
    )
    .unwrap();
    db.begin_publication(publishing, "Publishing").unwrap();
    drop(db);
    let mut db = open(&directory);
    let changed = db.reconcile_after_restart().unwrap();
    assert_eq!(changed.len(), 3);
    assert_eq!(db.execution(checking).unwrap().status, Status::Pending);
    let interrupted = db.execution(working).unwrap();
    assert_eq!(interrupted.status, Status::Interrupted);
    assert!(
        interrupted.holds_slot(),
        "unknown process state keeps the slot"
    );
    let errored = db.execution(publishing).unwrap();
    assert_eq!(errored.status, Status::Error);
    assert!(errored.error.unwrap().retryable);
    assert!(db.reconcile_after_restart().unwrap().is_empty());
}

#[test]
fn snapshots_identity_and_step_order_cannot_be_changed_through_sql() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let saved = entered(start(&mut db, project.id, 1, Some(10), 1).unwrap());
    for column in [
        "description",
        "parent_description",
        "title",
        "issue_id",
        "run_owner",
    ] {
        assert!(
            db.connection
                .execute(
                    &format!("UPDATE executions SET {column} = 'changed' WHERE id = ?1"),
                    [saved.id]
                )
                .is_err(),
            "{column} must be immutable"
        );
    }
    assert!(
        db.connection
            .execute(
                "UPDATE executions SET step = 'checking' WHERE id = ?1",
                [saved.id]
            )
            .is_err()
    );
    assert_eq!(db.execution(saved.id).unwrap(), saved);
}

#[test]
fn failed_audit_insert_rolls_back_every_transition() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let fail = "CREATE TRIGGER fail_audit BEFORE INSERT ON execution_events BEGIN SELECT RAISE(ABORT, 'injected disk write failure'); END;";
    db.connection.execute_batch(fail).unwrap();
    assert!(
        db.begin_checking(project.id, &identity(1), "owner-login")
            .is_err()
    );
    assert_eq!(db.summary().unwrap().executions, 0);
    db.connection
        .execute_batch("DROP TRIGGER fail_audit;")
        .unwrap();
    let id = db
        .begin_checking(project.id, &identity(1), "owner-login")
        .unwrap()
        .id;
    db.connection.execute_batch(fail).unwrap();
    assert!(db.enter_working(id, &snapshot(1), None, 1).is_err());
    let unchanged = db.execution(id).unwrap();
    assert_eq!(
        (unchanged.step, unchanged.snapshot.clone()),
        (Step::Checking, None)
    );
    assert_eq!(
        db.summary().unwrap().reserved_slots,
        0,
        "no slot without the audit event"
    );
    db.connection
        .execute_batch("DROP TRIGGER fail_audit;")
        .unwrap();
    entered(db.enter_working(id, &snapshot(1), None, 1).unwrap());
    db.connection.execute_batch(fail).unwrap();
    assert!(
        db.end_working_attempt(
            id,
            &AttemptEnd::Finished {
                result: "completed".into()
            },
            true
        )
        .is_err()
    );
    assert!(db.execution(id).unwrap().holds_slot());
    assert_eq!(db.events(id, 0).unwrap().len(), 2);
}

#[test]
fn failed_commit_does_not_leave_a_reserved_execution() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let id = db
        .begin_checking(project.id, &identity(1), "owner-login")
        .unwrap()
        .id;
    // A deferred foreign key fails at COMMIT, after every statement in the transition succeeded.
    db.connection.execute_batch("CREATE TABLE commit_guard (project_id INTEGER REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER fail_commit AFTER INSERT ON slot_attempts BEGIN INSERT INTO commit_guard VALUES (-1); END;").unwrap();
    assert!(matches!(
        db.enter_working(id, &snapshot(1), None, 1),
        Err(Error::Sql(_))
    ));
    assert_eq!(db.summary().unwrap().reserved_slots, 0);
    assert_eq!(db.execution(id).unwrap().step, Step::Checking);
    assert_eq!(db.events(id, 0).unwrap().len(), 1);
    db.connection
        .execute_batch("DROP TRIGGER fail_commit;")
        .unwrap();
    entered(db.enter_working(id, &snapshot(1), None, 1).unwrap());
}

#[test]
fn schema_1_history_migrates_as_legacy_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("001_initial.sql"))
        .unwrap();
    connection.execute_batch("
        INSERT INTO projects (checkout_path, github_host, repository_id, owner, name, remote) VALUES ('/a', 'github.com', 'R', 'o', 'r', 'origin');
        INSERT INTO executions (project_id, github_host, issue_id, issue_number, title, description, status, termination_confirmed)
            VALUES (1, 'github.com', 'I_1', 1, 'One', 'Body one', 'interrupted', 0),
                   (1, 'github.com', 'I_1', 1, 'One', 'Body one', 'failed', 1);
        INSERT INTO execution_events (execution_id, sequence, previous_status, status, termination_confirmed, reason)
            VALUES (1, 1, NULL, 'preparing', 0, 'Execution reserved');
        INSERT INTO execution_output (execution_id, sequence, source, payload) VALUES (1, 1, 'agent', 'old output');
    ").unwrap();
    drop(connection);
    let mut db = Database::open(&path).unwrap();
    let interrupted = db.execution(1).unwrap();
    assert!(interrupted.legacy);
    assert_eq!(
        (interrupted.step, interrupted.status),
        (Step::Working, Status::Interrupted)
    );
    assert!(
        interrupted.holds_slot(),
        "unconfirmed schema 1 work keeps its slot"
    );
    assert_eq!(interrupted.snapshot.unwrap().description, "Body one");
    let failed = db.execution(2).unwrap();
    assert_eq!(
        (failed.status, failed.outcome, failed.holds_slot()),
        (Status::Ended, Some(Outcome::Legacy), false)
    );
    assert_eq!(db.output(1, 0).unwrap()[0].payload, "old output");
    assert_eq!(db.events(1, 0).unwrap()[0].reason, "Execution reserved");
    // Legacy records do not block a new execution of the same issue.
    let checking = db.begin_checking(1, &identity(1), "owner-login").unwrap();
    assert!(!checking.legacy);
    let version: i64 = db
        .connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[test]
fn concurrency_limit_setting_has_a_default_and_range() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open(&directory);
    assert_eq!(db.concurrency_limit().unwrap(), DEFAULT_CONCURRENCY_LIMIT);
    db.set_concurrency_limit(5).unwrap();
    assert_eq!(db.concurrency_limit().unwrap(), 5);
    assert!(db.set_concurrency_limit(0).is_err());
    assert!(db.set_concurrency_limit(MAX_CONCURRENCY_LIMIT + 1).is_err());
    assert_eq!(db.concurrency_limit().unwrap(), 5);
}

#[test]
fn output_is_bounded_batched_and_paged() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store.add_project(project("/fixture/batch")).await.unwrap();
        let checking = store
            .begin_checking(project.id, identity(1), "owner-login".into())
            .await
            .unwrap();
        let id = entered(
            store
                .enter_working(checking.id, snapshot(1), None, Some(1))
                .await
                .unwrap(),
        )
        .id;
        let blocks = |range: std::ops::Range<i32>| {
            range
                .map(|i| (OutputSource::Agent, format!("block {i}")))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            store.append_output_batch(id, blocks(0..150)).await.unwrap(),
            150
        );
        assert_eq!(store.append_output_batch(id, vec![]).await.unwrap(), 150);
        let mut invalid = blocks(0..3);
        invalid[2].1 = "x".repeat(MAX_OUTPUT_BYTES + 1);
        assert!(matches!(
            store.append_output_batch(id, invalid).await,
            Err(Error::InvalidInput(_))
        ));
        let page = store.output(id, 140).await.unwrap();
        assert_eq!(page.len(), 10);
        assert_eq!(page[0].sequence, 141);
        assert_eq!(page[9].payload, "block 149");
        assert_eq!(page[0].attempt, Some(1));
        assert_eq!(
            store.output(id, 0).await.unwrap().len(),
            100,
            "pages hold at most 100 blocks"
        );
        assert_eq!(store.output_count(id).await.unwrap(), 150);
        store.close().await.unwrap();
    });
}

#[test]
fn data_directory_lock_is_released_after_worker_closes() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        assert!(matches!(
            Store::open(directory.path().into()).await,
            Err(Error::AlreadyOpen)
        ));
        let clone = store.clone();
        store.close().await.unwrap();
        assert!(matches!(clone.summary().await, Err(Error::WorkerStopped)));
        // Other tests spawn subprocesses. A child forked while the lock was open holds the
        // inherited descriptor until it calls exec, so allow that brief window to pass.
        let mut reopened = Store::open(directory.path().into()).await;
        for _ in 0..50 {
            if !matches!(reopened, Err(Error::AlreadyOpen)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            reopened = Store::open(directory.path().into()).await;
        }
        reopened.unwrap().close().await.unwrap();
    });
}

#[test]
fn unknown_schema_is_rejected_without_changing_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    assert!(matches!(Database::open(&path), Err(Error::NewerSchema(99))));
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        99
    );
}
