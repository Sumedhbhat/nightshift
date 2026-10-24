use super::{
    cli::{GhError, Subprocess},
    eligibility::{self, Blocker, Problem},
    fake::{self, FakeGh},
    *,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn run<T>(test: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test)
}

fn project() -> Project {
    Project {
        id: 1,
        checkout_path: "/tmp/fixture".into(),
        github_host: "github.com".into(),
        repository_id: "R_1".into(),
        owner: "owner".into(),
        name: "repo".into(),
        remote: "origin".into(),
        setup_command: None,
    }
}

const ME: &str = "octo";

fn issue(number: u32) -> Value {
    json!({
        "node_id": format!("I_{number}"), "number": number, "title": format!("Issue {number}"),
        "body": format!("Body {number}"), "state": "open", "state_reason": null,
        "labels": [{"name": "ready-for-agent"}], "assignees": [{"login": ME}],
        "parent_issue_url": null,
        "issue_dependencies_summary": {"blocked_by": 0, "total_blocked_by": 0},
        "sub_issues_summary": {"total": 0},
        "repository_url": "https://api.github.com/repos/owner/repo",
        "html_url": format!("https://github.com/owner/repo/issues/{number}")
    })
}

fn with(mut value: Value, changes: Value) -> Value {
    for (key, change) in changes.as_object().unwrap() {
        value[key] = change.clone();
    }
    value
}

fn closed(number: u32, reason: Option<&str>) -> Value {
    with(
        issue(number),
        json!({"state": "closed", "state_reason": reason}),
    )
}

/// Replace (not queue) the issue's "blocked by" list.
fn blocked_by(fake: &FakeGh, number: u32, blockers: Vec<Value>) {
    fake.set(
        &format!(
            "api --hostname github.com --method GET --paginate --slurp repos/owner/repo/issues/{number}/dependencies/blocked_by?per_page=100"
        ),
        fake::ok(json!([blockers])),
    );
}

fn summary(total: u32) -> Value {
    json!({"issue_dependencies_summary": {"blocked_by": total, "total_blocked_by": total}})
}

/// An eligible standalone issue #5, with an optional change to its fields.
fn standalone(fake: &FakeGh, changes: Value) {
    fake.get("repos/owner/repo/issues/5", with(issue(5), changes));
    blocked_by(fake, 5, vec![]);
}

fn check(fake: &Arc<FakeGh>) -> eligibility::Eligibility {
    run(eligibility::check(&fake.gh(), &project(), 5, ME))
}

#[test]
fn github_remote_formats_and_invalid_hosts() {
    for url in [
        "git@github.com:owner/repo.git",
        "https://github.com/owner/repo",
        "https://github.com/owner/repo.git/",
        "ssh://git@github.com/owner/repo.git",
        "ssh://git@github.com:22/owner/repo.git",
        "https://user@github.com/owner/repo",
    ] {
        assert_eq!(
            parse_remote(url),
            Some(("owner".into(), "repo".into())),
            "{url}"
        );
    }
    for url in [
        "https://github.com.evil.test/owner/repo",
        "https://gitlab.com/owner/repo",
        "https://github.com/owner/repo/issues",
        "https://github.com/owner/repo?x=1",
        "git@github.com:owner/../repo",
        "file:///tmp/repo",
        "http://github.com/owner/repo",
    ] {
        assert_eq!(parse_remote(url), None, "{url}");
    }
}

#[test]
fn probe_reports_missing_incompatible_and_signed_out_cli() {
    let missing = Gh::new(Arc::new(Subprocess::new("/nonexistent/nightshift-gh")));
    assert_eq!(run(missing.probe()), Err(GhError::NotInstalled));

    let old = FakeGh::signed_in(ME);
    old.set(
        "api --help",
        fake::text("Flags: --method --paginate --input --hostname"),
    );
    assert!(
        matches!(run(old.gh().probe()), Err(GhError::Incompatible(m)) if m.contains("--slurp"))
    );

    let no_json = FakeGh::signed_in(ME);
    no_json.set(
        "auth status --hostname github.com --json hosts",
        fake::fail("unknown flag: --json"),
    );
    assert!(matches!(
        run(no_json.gh().probe()),
        Err(GhError::Incompatible(_))
    ));

    let signed_out = FakeGh::signed_in(ME);
    signed_out.set(
        "auth status --hostname github.com --json hosts",
        fake::ok(
            json!({"hosts": {"github.com": [{"state": "error", "active": true, "login": ME}]}}),
        ),
    );
    assert_eq!(run(signed_out.gh().probe()), Err(GhError::NotAuthenticated));

    let none = FakeGh::signed_in(ME);
    none.set(
        "auth status --hostname github.com --json hosts",
        fake::ok(json!({"hosts": {}})),
    );
    assert_eq!(run(none.gh().probe()), Err(GhError::NotAuthenticated));

    let good = FakeGh::signed_in(ME);
    assert_eq!(run(good.gh().probe()).unwrap().login, ME);
    assert!(
        good.calls().iter().all(|call| !call.contains("token")),
        "never asks for a token"
    );
}

#[test]
fn account_switch_is_detected() {
    let fake = FakeGh::signed_in("someone-else");
    assert_eq!(
        run(fake.gh().verify_owner(ME)),
        Err(GhError::AccountChanged {
            expected: ME.into(),
            actual: "someone-else".into()
        })
    );
    assert!(run(FakeGh::signed_in(ME).gh().verify_owner("OCTO")).is_ok());
}

#[cfg(unix)]
#[test]
fn subprocess_runner_disables_prompts_and_passes_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("gh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '{\"prompt\":\"%s\",\"pager\":\"%s\",\"args\":\"%s\"}' \"$GH_PROMPT_DISABLED\" \"$GH_PAGER\" \"$*\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let gh = Gh::new(Arc::new(Subprocess::new(&script)));
    // A thread forking elsewhere can briefly hold the new script's write descriptor, which makes
    // exec fail with "Text file busy". Retry that specific error.
    let mut result = run(gh.get::<Value>("repos/owner/repo"));
    for _ in 0..50 {
        match &result {
            Err(GhError::Failed(message)) if message.contains("Text file busy") => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                result = run(gh.get("repos/owner/repo"));
            }
            _ => break,
        }
    }
    let value = result.unwrap();
    assert_eq!(value["prompt"], "1");
    assert_eq!(value["pager"], "cat");
    assert_eq!(
        value["args"],
        "api --hostname github.com --method GET repos/owner/repo"
    );
}

#[test]
fn private_repositories_are_admitted_and_inaccessible_ones_rejected() {
    let fake = FakeGh::signed_in(ME);
    fake.get(
        "repos/owner/repo",
        json!({"node_id": "R_9", "name": "Repo", "owner": {"login": "Owner"}, "private": true,
               "default_branch": "main", "permissions": {"push": true}}),
    );
    let github = GitHub::new(fake.gh());
    let remote = Remote {
        checkout: "/tmp/local".into(),
        name: "origin".into(),
        owner: "owner".into(),
        repo: "repo".into(),
    };
    let verified = run(github.verify(remote.clone())).unwrap();
    assert_eq!(
        (verified.repository_id.as_str(), verified.name.as_str()),
        ("R_9", "Repo")
    );
    let hidden = Remote {
        repo: "hidden".into(),
        ..remote
    };
    let error = run(github.verify(hidden)).unwrap_err();
    assert!(error.contains("cannot access"), "{error}");
}

#[test]
fn repository_access_reports_read_only_archived_and_replaced_repositories() {
    let fake = FakeGh::signed_in(ME);
    let github = GitHub::new(fake.gh());
    let repo = |push: bool, archived: bool, id: &str| {
        json!({"node_id": id, "name": "repo", "owner": {"login": "owner"}, "default_branch": "trunk",
               "archived": archived, "permissions": {"push": push}})
    };
    fake.respond(
        "api --hostname github.com --method GET repos/owner/repo",
        fake::ok(repo(false, false, "R_1")),
    );
    fake.respond(
        "api --hostname github.com --method GET repos/owner/repo",
        fake::ok(repo(true, true, "R_1")),
    );
    fake.respond(
        "api --hostname github.com --method GET repos/owner/repo",
        fake::ok(repo(true, false, "R_other")),
    );
    let first = run(github.access(&project())).unwrap();
    assert!(!first.can_push);
    assert_eq!(first.default_branch, "trunk");
    assert!(run(github.access(&project())).unwrap().archived);
    assert!(run(github.access(&project())).is_err());
}

#[test]
fn issue_page_follows_link_header_excludes_prs_and_rejects_foreign_hosts() {
    let fake = FakeGh::signed_in(ME);
    let first = "repos/owner/repo/issues?state=open&per_page=10&sort=created&direction=desc";
    let body = json!([issue(1), with(issue(2), json!({"pull_request": {}}))]);
    fake.respond(
        &format!("api --hostname github.com --method GET --include {first}"),
        fake::text(&format!(
            "HTTP/2.0 200 OK\r\nLink: <https://api.github.com/repositories/1/issues?page=2&after=cursor>; rel=\"next\"\r\n\r\n{body}"
        )),
    );
    let github = GitHub::new(fake.gh());
    let page = run(github.issue_page(&project(), None));
    assert_eq!(page.error, None);
    assert_eq!(
        page.issues.iter().map(|i| i.number).collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        page.next.as_deref(),
        Some("repositories/1/issues?page=2&after=cursor")
    );

    fake.respond(
        "api --hostname github.com --method GET --include next",
        fake::text("HTTP/2.0 200 OK\r\nLink: <https://evil.test/x>; rel=\"next\"\r\n\r\n[]"),
    );
    assert!(
        run(github.issue_page(&project(), Some("next")))
            .error
            .unwrap()
            .contains("unexpected host")
    );

    fake.respond(
        "api --hostname github.com --method GET --include broken",
        fake::fail("gh: Validation Failed (HTTP 422)"),
    );
    let failed = run(github.issue_page(&project(), Some("broken")));
    assert!(failed.issues.is_empty());
    assert!(failed.error.unwrap().contains("HTTP 422"));
}

#[test]
fn candidates_are_assigned_ready_open_issues_and_failures_stay_per_project() {
    let fake = FakeGh::signed_in(ME);
    fake.get_all(
        "repos/owner/repo/issues?state=open&assignee=octo&labels=ready-for-agent&per_page=100&sort=created&direction=asc",
        json!([[
            issue(1),
            with(issue(2), json!({"pull_request": {}})),
            with(issue(3), json!({"sub_issues_summary": {"total": 2}})),
            with(issue(4), json!({"assignees": [{"login": "other"}]})),
        ], [issue(5)]]),
    );
    let mut broken = project();
    broken.id = 2;
    broken.name = "private-gone".into();
    let search = run(eligibility::candidates(
        &fake.gh(),
        &[project(), broken],
        ME,
    ));
    assert_eq!(
        search
            .candidates
            .iter()
            .map(|c| c.number)
            .collect::<Vec<_>>(),
        [1, 5]
    );
    assert_eq!(search.errors.len(), 1);
    assert_eq!(search.errors[0].0, 2);
    assert!(matches!(search.errors[0].1, GhError::NotFound(_)));
    assert!(
        fake.calls()
            .iter()
            .all(|call| !call.contains("state=closed")),
        "closed issues are not listed"
    );
}

#[test]
fn standalone_ready_issue_is_eligible_with_its_snapshot() {
    let fake = FakeGh::signed_in(ME);
    standalone(&fake, json!({}));
    let result = check(&fake);
    assert!(result.eligible(), "{:?}", result.problems);
    assert_eq!(result.issue.unwrap().description, "Body 5");
    assert_eq!(result.parent, None);
}

#[test]
fn issue_gates_report_each_problem() {
    for (changes, expected) in [
        (json!({"labels": [{"name": "bug"}]}), Problem::MissingLabel),
        (json!({"labels": []}), Problem::MissingLabel),
        (
            json!({"assignees": [{"login": "other"}]}),
            Problem::NotAssigned { login: ME.into() },
        ),
        (
            json!({"state": "closed", "state_reason": "completed"}),
            Problem::NotOpen,
        ),
        (
            json!({"sub_issues_summary": {"total": 1}}),
            Problem::IsParent,
        ),
        (json!({"pull_request": {}}), Problem::PullRequest),
    ] {
        let fake = FakeGh::signed_in(ME);
        standalone(&fake, changes.clone());
        let result = check(&fake);
        assert!(!result.eligible(), "{changes}");
        assert_eq!(result.problems, [expected], "{changes}");
    }
}

#[test]
fn open_blockers_block_and_closed_blockers_resolve_for_every_reason() {
    let fake = FakeGh::signed_in(ME);
    standalone(&fake, summary(5));
    blocked_by(
        &fake,
        5,
        vec![
            closed(1, Some("completed")),
            closed(2, Some("not_planned")),
            closed(3, Some("duplicate")),
            closed(4, None),
            closed(6, Some("reopened-is-not-a-closure-but-state-wins")),
        ],
    );
    let result = check(&fake);
    assert!(result.eligible(), "{:?}", result.problems);
    assert_eq!(result.blockers.len(), 5);
    assert_eq!(
        result.blockers[3].state_reason, None,
        "absent closure reason still resolves"
    );

    let open = FakeGh::signed_in(ME);
    standalone(&open, summary(2));
    blocked_by(&open, 5, vec![closed(1, Some("completed")), issue(2)]);
    let result = check(&open);
    assert_eq!(
        result.problems,
        [Problem::Blocked(Blocker {
            repository: "owner/repo".into(),
            number: 2,
            title: "Issue 2".into(),
            open: true,
            state_reason: None,
            inherited: false,
        })]
    );
}

#[test]
fn inaccessible_or_failed_dependency_lookups_block() {
    let hidden = FakeGh::signed_in(ME);
    standalone(&hidden, summary(2));
    blocked_by(&hidden, 5, vec![closed(1, Some("completed"))]);
    let result = check(&hidden);
    assert!(
        matches!(&result.problems[..], [Problem::DependenciesUnverifiable { number: 5, inherited: false, detail }] if detail.contains("only 1"))
    );

    let failing = FakeGh::signed_in(ME);
    failing.get("repos/owner/repo/issues/5", issue(5));
    failing.respond(
        "api --hostname github.com --method GET --paginate --slurp repos/owner/repo/issues/5/dependencies/blocked_by?per_page=100",
        fake::fail("gh: API rate limit exceeded (HTTP 403)"),
    );
    let result = check(&failing);
    assert!(!result.eligible());
    assert!(matches!(
        &result.problems[..],
        [Problem::DependenciesUnverifiable { .. }]
    ));

    let unreadable = FakeGh::signed_in(ME);
    let result = check(&unreadable);
    assert!(matches!(
        &result.problems[..],
        [Problem::Unverifiable { .. }]
    ));
    assert_eq!(result.issue, None);
}

fn with_parent(fake: &FakeGh, parent: Value) {
    standalone(
        fake,
        json!({"parent_issue_url": "https://api.github.com/repos/owner/repo/issues/50"}),
    );
    fake.get("repos/owner/repo/issues/50", parent);
    blocked_by(fake, 50, vec![]);
}

#[test]
fn parent_must_be_open_ready_and_readable_and_supplies_context() {
    let ready = FakeGh::signed_in(ME);
    // The parent may belong to someone else; only its label and state matter.
    with_parent(
        &ready,
        with(
            issue(50),
            json!({"assignees": [{"login": "senior"}], "sub_issues_summary": {"total": 3}}),
        ),
    );
    let result = check(&ready);
    assert!(result.eligible(), "{:?}", result.problems);
    assert_eq!(result.parent.unwrap().description, "Body 50");

    let unlabeled = FakeGh::signed_in(ME);
    with_parent(&unlabeled, with(issue(50), json!({"labels": []})));
    assert_eq!(
        check(&unlabeled).problems,
        [Problem::ParentMissingLabel { number: 50 }]
    );

    let closed_parent = FakeGh::signed_in(ME);
    with_parent(&closed_parent, closed(50, Some("completed")));
    assert_eq!(
        check(&closed_parent).problems,
        [Problem::ParentClosed { number: 50 }]
    );

    let hidden = FakeGh::signed_in(ME);
    standalone(
        &hidden,
        json!({"parent_issue_url": "https://api.github.com/repos/owner/repo/issues/50"}),
    );
    assert!(matches!(
        &check(&hidden).problems[..],
        [Problem::ParentUnverifiable { .. }]
    ));
}

#[test]
fn open_parent_dependencies_are_inherited() {
    let fake = FakeGh::signed_in(ME);
    with_parent(&fake, with(issue(50), summary(1)));
    blocked_by(
        &fake,
        50,
        vec![with(
            issue(60),
            json!({"repository_url": "https://api.github.com/repos/other/lib"}),
        )],
    );
    let result = check(&fake);
    assert!(
        matches!(&result.problems[..], [Problem::Blocked(b)] if b.inherited && b.number == 60 && b.repository == "other/lib")
    );

    let resolved = FakeGh::signed_in(ME);
    with_parent(&resolved, with(issue(50), summary(1)));
    blocked_by(&resolved, 50, vec![closed(60, Some("completed"))]);
    assert!(check(&resolved).eligible());
}

#[test]
fn completed_local_execution_does_not_satisfy_an_open_dependency() {
    use crate::{
        domain::{IssueIdentity, IssueSnapshot, NewProject},
        storage::{AttemptEnd, Entry, Store},
    };
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().into()).await.unwrap();
        let saved = store
            .add_project(NewProject {
                checkout_path: "/tmp/fixture".into(),
                github_host: "github.com".into(),
                repository_id: "R_1".into(),
                owner: "owner".into(),
                name: "repo".into(),
                remote: "origin".into(),
                setup_command: None,
            })
            .await
            .unwrap();
        // Blocker #2 finished locally and is waiting for review, but is open on GitHub.
        let checking = store
            .begin_checking(
                saved.id,
                IssueIdentity {
                    id: "I_2".into(),
                    number: 2,
                },
                ME.into(),
            )
            .await
            .unwrap();
        let Entry::Entered(blocker) = store
            .enter_working(
                checking.id,
                IssueSnapshot {
                    id: "I_2".into(),
                    number: 2,
                    title: "Two".into(),
                    description: "".into(),
                },
                None,
                Some(1),
            )
            .await
            .unwrap()
        else {
            panic!("no capacity")
        };
        store
            .end_working_attempt(
                blocker.id,
                AttemptEnd::Finished {
                    result: "completed".into(),
                },
                true,
            )
            .await
            .unwrap();
        let fake = FakeGh::signed_in(ME);
        standalone(&fake, summary(1));
        blocked_by(&fake, 5, vec![issue(2)]);
        let result = eligibility::check(&fake.gh(), &saved, 5, ME).await;
        assert!(matches!(&result.problems[..], [Problem::Blocked(b)] if b.number == 2));
        store.close().await.unwrap();
    });
}

#[test]
fn start_check_blocks_on_identity_access_and_push_before_issue_gates() {
    use crate::application::start::{self, Gate};
    let github = |fake: &Arc<FakeGh>| GitHub::new(fake.gh());
    let repo = |push: bool| json!({"node_id": "R_1", "name": "repo", "owner": {"login": "owner"}, "default_branch": "main", "permissions": {"push": push}});

    let switched = FakeGh::signed_in("someone-else");
    let result = run(start::check(&github(&switched), &project(), 5, ME));
    assert!(matches!(
        &result.gates[..],
        [Gate::Identity(GhError::AccountChanged { .. })]
    ));
    assert!(
        switched.calls().iter().all(|c| !c.contains("repos/")),
        "no repository calls after an identity failure"
    );

    let read_only = FakeGh::signed_in(ME);
    read_only.get("repos/owner/repo", repo(false));
    let result = run(start::check(&github(&read_only), &project(), 5, ME));
    assert_eq!(result.gates, [Gate::ReadOnly]);
    assert!(result.gates[0].blocks_project());

    // A writable repository with a remote that rejects the push preflight.
    let (_root, _remote, checkout) = crate::git::testing::repository();
    let mut local = project();
    local.checkout_path = checkout.to_str().unwrap().into();
    local.remote = "missing".into();
    let writable = FakeGh::signed_in(ME);
    writable.get("repos/owner/repo", repo(true));
    let result = run(start::check(&github(&writable), &local, 5, ME));
    assert!(matches!(&result.gates[..], [Gate::PushPreflight(_)]));
    assert!(
        writable.calls().iter().all(|c| !c.contains("issues/5")),
        "issue not read before push preflight passes"
    );

    local.remote = "origin".into();
    standalone(&writable, json!({}));
    let result = run(start::check(&github(&writable), &local, 5, ME));
    assert!(result.eligible(), "{}", result.summary());
    assert_eq!(result.default_branch.as_deref(), Some("main"));
}

#[test]
fn stale_generations_are_rejected() {
    let mut generations = crate::application::start::Generations::default();
    let first = generations.advance();
    let second = generations.advance();
    assert!(!generations.is_current(first));
    assert!(generations.is_current(second));
}

#[test]
fn checkout_inspection_validates_git_and_keeps_distinct_remotes() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| crate::git::testing::git(dir.path(), args);
        assert!(inspect_checkout(dir.path().into()).await.is_err());
        git(&["init"]);
        assert!(inspect_checkout(dir.path().into()).await.is_err());
        git(&["commit", "--allow-empty", "-m", "fixture"]);
        git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
        git(&["remote", "add", "mirror", "https://github.com/owner/repo"]);
        git(&["remote", "add", "upstream", "https://github.com/other/repo"]);
        let remotes = inspect_checkout(dir.path().into()).await.unwrap();
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "origin");
    });
}

fn issue_thread_fixture(fake: &FakeGh, number: u32, changes: Value) {
    fake.set(
        &format!("api --hostname github.com --method GET repos/owner/repo/issues/{number}"),
        fake::ok(with(issue(number), changes)),
    );
    fake.get_all(
        &format!("repos/owner/repo/issues/{number}/comments?per_page=100"),
        json!([[{"id": 7, "body": "Looks good", "user": {"login": "reviewer"}, "created_at": "2026-10-24T10:00:00Z"}]]),
    );
}

#[test]
fn issue_thread_reads_the_issue_comments_and_viewer_and_rejects_pull_requests() {
    use issues::Comment;
    let fake = FakeGh::signed_in(ME);
    issue_thread_fixture(&fake, 4, json!({"user": {"login": "lead"}}));
    let thread = run(GitHub::new(fake.gh()).issue_thread(&project(), 4)).unwrap();
    assert_eq!(thread.viewer, ME);
    assert_eq!(thread.issue.body, "Body 4");
    assert_eq!(thread.issue.author, "lead");
    assert!(thread.issue.open && thread.issue.ready() && thread.issue.assigned_to("OCTO"));
    assert_eq!(
        thread.comments,
        vec![Comment {
            id: 7,
            author: "reviewer".into(),
            body: "Looks good".into(),
            created_at: "2026-10-24T10:00:00Z".into(),
        }]
    );

    issue_thread_fixture(&fake, 5, json!({"pull_request": {}}));
    let error = run(GitHub::new(fake.gh()).issue_thread(&project(), 5)).unwrap_err();
    assert!(error.to_string().contains("pull request"), "{error}");
}

#[test]
fn create_issue_assigns_and_labels_only_when_asked() {
    use issues::Draft;
    let fake = FakeGh::signed_in(ME);
    fake.respond_prefix(
        "api --hostname github.com --method POST repos/owner/repo/issues --input -",
        fake::ok(with(issue(9), json!({"labels": [], "assignees": []}))),
    );
    let github = GitHub::new(fake.gh());
    let draft = Draft {
        title: "  Add keyboard help  ".into(),
        body: "Line one\nLine two".into(),
        assign_me: true,
        ready: false,
    };
    let thread = run(github.create_issue(&project(), &draft)).unwrap();
    assert_eq!(thread.issue.number, 9);
    let post = fake
        .calls()
        .into_iter()
        .find(|c| c.contains("POST"))
        .unwrap();
    let body: Value = serde_json::from_str(post.split(" <<< ").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"title": "Add keyboard help", "body": "Line one\nLine two", "assignees": [ME]})
    );

    let ready = Draft {
        ready: true,
        assign_me: false,
        ..draft
    };
    run(github.create_issue(&project(), &ready)).unwrap();
    let post = fake
        .calls()
        .into_iter()
        .rfind(|c| c.contains("POST"))
        .unwrap();
    let body: Value = serde_json::from_str(post.split(" <<< ").nth(1).unwrap()).unwrap();
    assert_eq!(body["labels"], json!(["ready-for-agent"]));
    assert!(body.get("assignees").is_none());

    let blank = Draft {
        title: "   ".into(),
        ..Default::default()
    };
    assert!(run(github.create_issue(&project(), &blank)).is_err());
    assert_eq!(
        fake.calls().iter().filter(|c| c.contains("POST")).count(),
        2,
        "a blank title never reaches GitHub"
    );
}

#[test]
fn issue_changes_use_the_matching_endpoint_and_reread_the_issue() {
    use issues::Change;
    let cases = [
        (
            Change::Edit {
                title: " New ".into(),
                body: "Body".into(),
            },
            "--method PATCH repos/owner/repo/issues/4 --input - <<< {\"title\":\"New\",\"body\":\"Body\"}",
        ),
        (
            Change::Comment("Thanks".into()),
            "--method POST repos/owner/repo/issues/4/comments --input - <<< {\"body\":\"Thanks\"}",
        ),
        (
            Change::Close { not_planned: false },
            "--method PATCH repos/owner/repo/issues/4 --input - <<< {\"state\":\"closed\",\"state_reason\":\"completed\"}",
        ),
        (
            Change::Close { not_planned: true },
            "--method PATCH repos/owner/repo/issues/4 --input - <<< {\"state\":\"closed\",\"state_reason\":\"not_planned\"}",
        ),
        (
            Change::Reopen,
            "--method PATCH repos/owner/repo/issues/4 --input - <<< {\"state\":\"open\"}",
        ),
        (
            Change::SetReady(true),
            "--method POST repos/owner/repo/issues/4/labels --input - <<< {\"labels\":[\"ready-for-agent\"]}",
        ),
        (
            Change::SetReady(false),
            "--method DELETE repos/owner/repo/issues/4/labels/ready-for-agent --input -",
        ),
        (
            Change::SetAssigned(true),
            "--method POST repos/owner/repo/issues/4/assignees --input - <<< {\"assignees\":[\"octo\"]}",
        ),
        (
            Change::SetAssigned(false),
            "--method DELETE repos/owner/repo/issues/4/assignees --input - <<< {\"assignees\":[\"octo\"]}",
        ),
    ];
    for (change, expected) in cases {
        let fake = FakeGh::signed_in(ME);
        fake.respond_prefix("api --hostname github.com --method P", fake::ok(json!({})));
        fake.respond_prefix(
            "api --hostname github.com --method DELETE",
            fake::ok(json!([])),
        );
        issue_thread_fixture(&fake, 4, json!({}));
        let changed = run(GitHub::new(fake.gh()).change_issue(&project(), 4, &change)).unwrap();
        let issues::Changed::Reread(thread) = changed else {
            panic!("{change:?} was not re-read: {changed:?}");
        };
        assert_eq!(thread.issue.number, 4);
        let calls = fake.calls();
        let write = calls
            .iter()
            .position(|c| c.contains(expected))
            .unwrap_or_else(|| panic!("{change:?}: no `{expected}` in {calls:#?}"));
        let read = calls
            .iter()
            .rposition(|c| c.ends_with("--method GET repos/owner/repo/issues/4"))
            .unwrap();
        assert!(write < read, "{change:?} re-reads the issue after writing");
    }
}

#[test]
fn issue_change_failures_and_missing_labels() {
    use issues::Change;
    let fake = FakeGh::signed_in(ME);
    issue_thread_fixture(&fake, 4, json!({"labels": []}));
    fake.respond_prefix(
        "api --hostname github.com --method DELETE repos/owner/repo/issues/4/labels/",
        fake::fail("gh: Label does not exist (HTTP 404)"),
    );
    let github = GitHub::new(fake.gh());
    let changed = run(github.change_issue(&project(), 4, &Change::SetReady(false))).unwrap();
    let issues::Changed::Reread(thread) = changed else {
        panic!("not re-read: {changed:?}");
    };
    assert!(!thread.issue.ready(), "an absent label is already removed");

    fake.respond_prefix(
        "api --hostname github.com --method PATCH",
        fake::fail("gh: Must have admin rights to Repository. (HTTP 403)"),
    );
    let error = run(github.change_issue(&project(), 4, &Change::Reopen)).unwrap_err();
    assert!(matches!(error, GhError::Forbidden(_)), "{error:?}");

    let before = fake.calls().len();
    assert!(run(github.change_issue(&project(), 4, &Change::Comment(" \n ".into()))).is_err());
    assert_eq!(
        fake.calls().len(),
        before,
        "a blank comment never reaches GitHub"
    );
}

#[test]
fn a_change_whose_reread_fails_still_reports_the_write() {
    use issues::{Change, Changed};
    let fake = FakeGh::signed_in(ME);
    fake.respond_prefix(
        "api --hostname github.com --method POST repos/owner/repo/issues/4/comments",
        fake::ok(json!({"id": 9})),
    );
    fake.respond_prefix(
        "api --hostname github.com --method GET repos/owner/repo/issues/4",
        fake::fail("gh: Bad Gateway (HTTP 502)"),
    );
    let changed =
        run(GitHub::new(fake.gh()).change_issue(&project(), 4, &Change::Comment("Once".into())))
            .unwrap();
    assert!(matches!(changed, Changed::Unread(_)), "{changed:?}");
    assert_eq!(
        fake.calls().iter().filter(|c| c.contains("POST")).count(),
        1,
        "the comment is posted once"
    );
}
