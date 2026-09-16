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

fn issue(number: u32) -> IssueSnapshot {
    IssueSnapshot {
        id: format!("I_{number}"),
        number,
        title: format!("Issue {number}"),
        description: format!("Requirements for {number}"),
    }
}

fn execution(project_id: i64, number: u32, parent: Option<u32>) -> NewExecution {
    NewExecution {
        project_id,
        issue: issue(number),
        parent: parent.map(issue),
    }
}

fn limit(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

#[test]
fn worker_persists_snapshots_output_and_audit_across_reopen() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store
            .add_project(project("/fixture/checkout"))
            .await
            .unwrap();
        assert_eq!(project.setup_command.as_deref(), Some("echo setup"));
        let saved = store
            .reserve(execution(project.id, 1, Some(10)), limit(2))
            .await
            .unwrap();
        store
            .change(saved.id, ExecutionChange::Started, "Setup finished".into())
            .await
            .unwrap();
        store
            .append_output(saved.id, "agent".into(), "Saved agent output".into())
            .await
            .unwrap();
        store
            .change(
                saved.id,
                ExecutionChange::Interrupted,
                "Application quitting".into(),
            )
            .await
            .unwrap();
        store.close().await.unwrap();

        let reopened = Store::open(directory.path().into()).await.unwrap();
        assert_eq!(reopened.projects().await.unwrap(), vec![project]);
        let loaded = reopened.execution(saved.id).await.unwrap();
        assert_eq!(loaded.issue, saved.issue);
        assert_eq!(loaded.parent, saved.parent);
        assert_eq!(loaded.created_at, saved.created_at);
        assert!(loaded.created_at.ends_with('Z'));
        assert_eq!(loaded.status, ExecutionStatus::Interrupted);
        assert!(!loaded.termination_confirmed);
        assert_eq!(reopened.summary().await.unwrap().reserved_slots, 1);
        let events = reopened.events(saved.id, 0).await.unwrap();
        assert_eq!(
            events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(events[0].previous_status, None);
        assert_eq!(events[2].previous_status.as_deref(), Some("running"));
        assert_eq!(
            reopened.output(saved.id, 0).await.unwrap()[0].payload,
            "Saved agent output"
        );
        assert!(matches!(
            reopened
                .change(saved.id, ExecutionChange::Started, "No resume".into())
                .await,
            Err(Error::InvalidTransition)
        ));
        reopened.close().await.unwrap();
    });
}

#[test]
fn simultaneous_starts_respect_global_limit_across_projects() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let first = store.add_project(project("/fixture/first")).await.unwrap();
        let second = store.add_project(project("/fixture/second")).await.unwrap();
        let mut tasks = Vec::new();
        for number in 1..=12 {
            let store = store.clone();
            let project_id = if number % 2 == 0 { first.id } else { second.id };
            tasks.push(tokio::spawn(async move {
                store
                    .reserve(execution(project_id, number, None), limit(3))
                    .await
            }));
        }
        let mut accepted = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => accepted += 1,
                Err(Error::Capacity) => {}
                result => panic!("unexpected: {result:?}"),
            }
        }
        assert_eq!(accepted, 3);
        assert_eq!(store.summary().await.unwrap().executions, 3);
        store.close().await.unwrap();
    });
}

#[test]
fn duplicate_issue_is_rejected_across_checkouts() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let first = store.add_project(project("/fixture/first")).await.unwrap();
        let mut other = project("/fixture/second");
        other.github_host = "GITHUB.COM".into();
        let second = store.add_project(other).await.unwrap();
        store
            .reserve(execution(first.id, 1, None), limit(4))
            .await
            .unwrap();
        assert!(matches!(
            store.reserve(execution(second.id, 1, None), limit(4)).await,
            Err(Error::DuplicateIssue)
        ));
        assert_eq!(store.summary().await.unwrap().executions, 1);
        store.close().await.unwrap();
    });
}

#[test]
fn parent_slot_serializes_siblings_but_not_different_parents() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store
            .add_project(project("/fixture/checkout"))
            .await
            .unwrap();
        let first = store
            .reserve(execution(project.id, 1, Some(10)), limit(3))
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve(execution(project.id, 2, Some(10)), limit(3))
                .await,
            Err(Error::ParentBusy)
        ));
        store
            .reserve(execution(project.id, 3, Some(20)), limit(3))
            .await
            .unwrap();
        store
            .change(
                first.id,
                ExecutionChange::Interrupted,
                "Lost contact".into(),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve(execution(project.id, 2, Some(10)), limit(3))
                .await,
            Err(Error::ParentBusy)
        ));
        store
            .change(
                first.id,
                ExecutionChange::TerminationConfirmed,
                "Owned work stopped".into(),
            )
            .await
            .unwrap();
        // Capacity is available, but this is not evidence of eligibility to build on a predecessor.
        store
            .reserve(execution(project.id, 2, Some(10)), limit(3))
            .await
            .unwrap();
        assert_eq!(store.events(first.id, 0).await.unwrap().len(), 3);
        store.close().await.unwrap();
    });
}

#[test]
fn interrupted_capacity_is_retained_until_confirmed_and_late_events_are_rejected() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store
            .add_project(project("/fixture/checkout"))
            .await
            .unwrap();
        let first = store
            .reserve(execution(project.id, 1, None), limit(1))
            .await
            .unwrap();
        store
            .change(
                first.id,
                ExecutionChange::Interrupted,
                "Quit during preparation".into(),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .reserve(execution(project.id, 2, None), limit(1))
                .await,
            Err(Error::Capacity)
        ));
        store
            .change(
                first.id,
                ExecutionChange::TerminationConfirmed,
                "Preparation stopped".into(),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .change(first.id, ExecutionChange::Started, "Late response".into())
                .await,
            Err(Error::InvalidTransition)
        ));
        assert!(matches!(
            store
                .append_output(first.id, "agent".into(), "Late output".into())
                .await,
            Err(Error::InvalidTransition)
        ));
        store
            .reserve(execution(project.id, 2, None), limit(1))
            .await
            .unwrap();
        store.close().await.unwrap();
    });
}

#[test]
fn output_is_bounded_and_paged_without_losing_history() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let project = store
            .add_project(project("/fixture/checkout"))
            .await
            .unwrap();
        let saved = store
            .reserve(execution(project.id, 1, None), limit(1))
            .await
            .unwrap();
        for sequence in 1..=105 {
            assert_eq!(
                store
                    .append_output(saved.id, "setup".into(), format!("Line {sequence}"))
                    .await
                    .unwrap(),
                sequence
            );
        }
        assert!(matches!(
            store
                .append_output(saved.id, "setup".into(), "é".repeat(MAX_OUTPUT_BYTES))
                .await,
            Err(Error::InvalidInput(_))
        ));
        let first = store.output(saved.id, 0).await.unwrap();
        assert_eq!(first.len(), 100);
        let second = store
            .output(saved.id, first.last().unwrap().sequence)
            .await
            .unwrap();
        assert_eq!(second.len(), 5);
        assert_eq!(second[0].payload, "Line 101");
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
        Store::open(directory.path().into())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    });
}

#[test]
fn failed_audit_insert_rolls_back_reservation_and_status() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = Database::open(&directory.path().join("db.sqlite3")).unwrap();
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    db.connection.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON execution_events BEGIN SELECT RAISE(ABORT, 'injected disk write failure'); END;").unwrap();
    assert!(
        db.reserve(execution(project.id, 1, None), limit(1))
            .is_err()
    );
    assert_eq!(db.summary().unwrap().executions, 0);
    assert_eq!(db.summary().unwrap().reserved_slots, 0);
    db.connection
        .execute_batch("DROP TRIGGER fail_audit;")
        .unwrap();
    let saved = db
        .reserve(execution(project.id, 1, None), limit(1))
        .unwrap();
    db.connection.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON execution_events BEGIN SELECT RAISE(ABORT, 'injected disk write failure'); END;").unwrap();
    assert!(
        db.change(saved.id, ExecutionChange::Started, "Setup done")
            .is_err()
    );
    assert_eq!(
        db.execution(saved.id).unwrap().status,
        ExecutionStatus::Preparing
    );
    assert_eq!(db.events(saved.id, 0).unwrap().len(), 1);
}

#[test]
fn snapshots_cannot_be_updated_even_through_sql() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = Database::open(&directory.path().join("db.sqlite3")).unwrap();
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    let saved = db
        .reserve(execution(project.id, 1, Some(10)), limit(1))
        .unwrap();
    for column in ["description", "parent_description", "title"] {
        assert!(
            db.connection
                .execute(
                    &format!("UPDATE executions SET {column} = 'changed' WHERE id = ?1"),
                    [saved.id]
                )
                .is_err()
        );
    }
    assert_eq!(db.execution(saved.id).unwrap(), saved);
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
        first.reserve(execution(project.id, 1, None), limit(1))
    });
    let b = std::thread::spawn(move || {
        barrier.wait();
        second.reserve(execution(project.id, 2, None), limit(1))
    });
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Capacity)))
            .count(),
        1
    );
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

#[test]
fn failed_commit_does_not_leave_a_reserved_execution() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = Database::open(&directory.path().join("db.sqlite3")).unwrap();
    let project = db.add_project(project("/fixture/checkout")).unwrap();
    // A deferred foreign key fails at COMMIT, after the execution and event inserts succeeded.
    db.connection.execute_batch("CREATE TABLE commit_guard (project_id INTEGER REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER fail_commit AFTER INSERT ON execution_events BEGIN INSERT INTO commit_guard VALUES (-1); END;").unwrap();
    assert!(matches!(
        db.reserve(execution(project.id, 1, None), limit(1)),
        Err(Error::Sql(_))
    ));
    assert_eq!(db.summary().unwrap().executions, 0);
    assert_eq!(db.summary().unwrap().reserved_slots, 0);
    let events: i64 = db
        .connection
        .query_row("SELECT count(*) FROM execution_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 0);
    db.connection
        .execute_batch("DROP TRIGGER fail_commit;")
        .unwrap();
    db.reserve(execution(project.id, 1, None), limit(1))
        .unwrap();
}

#[test]
fn competing_siblings_reserve_only_one_parent_slot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db.sqlite3");
    let mut first = Database::open(&path).unwrap();
    let project = first.add_project(project("/fixture/checkout")).unwrap();
    let mut second = Database::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let a = std::thread::spawn(move || {
        other.wait();
        first.reserve(execution(project.id, 1, Some(10)), limit(3))
    });
    let b = std::thread::spawn(move || {
        barrier.wait();
        second.reserve(execution(project.id, 2, Some(10)), limit(3))
    });
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::ParentBusy)))
            .count(),
        1
    );
}
