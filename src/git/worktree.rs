//! Isolated worktree preparation. The owner's checkout is never switched, reset, or edited:
//! Nightshift only fetches into its remote-tracking ref and adds a branch and worktree.
//! Each stage records its evidence before the next starts, and failures are never cleaned up.
use super::{NETWORK_TIMEOUT, run};
use crate::{
    domain::{OutputSource, WorkingEvidence},
    github::cli::BoxFuture,
    storage::MAX_OUTPUT_BYTES,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Setup commands may install dependencies; allow a generous bound before treating them as hung.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Where preparation saves evidence and output, normally the execution's storage record.
pub trait Recorder: Send + Sync {
    fn evidence(&self, evidence: WorkingEvidence) -> BoxFuture<'_, Result<(), String>>;
    fn output(&self, source: OutputSource, text: String) -> BoxFuture<'_, Result<(), String>>;
}

#[derive(Clone, Debug)]
pub struct Request {
    pub checkout: PathBuf,
    pub remote: String,
    pub default_branch: String,
    /// Directory that holds Nightshift's worktrees, outside every checkout.
    pub worktrees_root: PathBuf,
    pub repository: String,
    pub issue_number: u32,
    pub issue_title: String,
    pub execution_id: i64,
    pub setup_command: Option<String>,
    /// Evidence from an earlier attempt. A recorded worktree is reused, never recreated.
    pub existing: WorkingEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Fetch,
    Branch,
    Worktree,
    Setup,
    Recording,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub stage: Stage,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub worktree: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

fn failure(stage: Stage) -> impl FnOnce(String) -> Failure {
    move |message| Failure { stage, message }
}

/// Lowercase ASCII slug of the issue title, at most 40 characters.
pub fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "issue".into()
    } else {
        slug
    }
}

pub fn branch_name(issue_number: u32, title: &str, execution_id: i64) -> String {
    format!("nightshift/{issue_number}-{}-{execution_id}", slug(title))
}

fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}

/// Prepare (or, for a retry, verify) the execution's worktree, then run optional setup.
pub async fn prepare(request: &Request, recorder: &dyn Recorder) -> Result<Prepared, Failure> {
    if !valid_branch(&request.default_branch) {
        return Err(Failure {
            stage: Stage::Fetch,
            message: format!(
                "`{}` is not a supported default branch name.",
                request.default_branch
            ),
        });
    }
    let existing = &request.existing;
    let prepared = match (
        &existing.worktree_path,
        &existing.branch,
        &existing.base_commit,
    ) {
        (Some(path), Some(branch), Some(commit)) => {
            reuse(Path::new(path), branch).await?;
            note(recorder, format!("Reusing worktree {path} on {branch}.")).await?;
            Prepared {
                worktree: path.into(),
                branch: branch.clone(),
                base_commit: commit.clone(),
            }
        }
        _ => create(request, recorder).await?,
    };
    if let Some(command) = request
        .setup_command
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        setup(&prepared.worktree, command, recorder).await?;
    }
    Ok(prepared)
}

async fn note(recorder: &dyn Recorder, text: String) -> Result<(), Failure> {
    recorder
        .output(OutputSource::Git, text)
        .await
        .map_err(failure(Stage::Recording))
}

async fn record(recorder: &dyn Recorder, evidence: WorkingEvidence) -> Result<(), Failure> {
    recorder
        .evidence(evidence)
        .await
        .map_err(failure(Stage::Recording))
}

async fn reuse(path: &Path, branch: &str) -> Result<(), Failure> {
    let head = run(path, &["symbolic-ref", "--short", "HEAD"], LOCAL_TIMEOUT)
        .await
        .map_err(|error| Failure {
            stage: Stage::Worktree,
            message: format!(
                "The recorded worktree {} is not usable: {error}",
                path.display()
            ),
        })?;
    if head.stdout != branch {
        return Err(Failure {
            stage: Stage::Worktree,
            message: format!(
                "The recorded worktree {} is on `{}`, not `{branch}`. Restore it manually.",
                path.display(),
                head.stdout
            ),
        });
    }
    Ok(())
}

async fn create(request: &Request, recorder: &dyn Recorder) -> Result<Prepared, Failure> {
    let checkout = &request.checkout;
    let remote = &request.remote;
    let default = &request.default_branch;
    // Fetch only the default branch into its remote-tracking ref. The owner's branches,
    // index, and working files are untouched.
    let tracking = format!("refs/remotes/{remote}/{default}");
    note(recorder, format!("Fetching {default} from {remote}…")).await?;
    run(
        checkout,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--",
            remote,
            &format!("+refs/heads/{default}:{tracking}"),
        ],
        NETWORK_TIMEOUT,
    )
    .await
    .map_err(failure(Stage::Fetch))?;
    let base_commit = run(
        checkout,
        &["rev-parse", "--verify", &format!("{tracking}^{{commit}}")],
        LOCAL_TIMEOUT,
    )
    .await
    .map_err(failure(Stage::Fetch))?
    .stdout;
    record(
        recorder,
        WorkingEvidence {
            default_branch: Some(default.clone()),
            base_commit: Some(base_commit.clone()),
            ..Default::default()
        },
    )
    .await?;

    let base_name = branch_name(
        request.issue_number,
        &request.issue_title,
        request.execution_id,
    );
    let mut branch = base_name.clone();
    let mut suffix = 1;
    while run(
        checkout,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        LOCAL_TIMEOUT,
    )
    .await
    .is_ok()
    {
        suffix += 1;
        if suffix > 100 {
            return Err(Failure {
                stage: Stage::Branch,
                message: format!("Could not find an unused branch name for {base_name}."),
            });
        }
        branch = format!("{base_name}-{suffix}");
    }
    let leaf = branch.trim_start_matches("nightshift/").replace('/', "-");
    let worktree = request.worktrees_root.join(&request.repository).join(&leaf);
    let canonical_checkout = checkout.canonicalize().unwrap_or_else(|_| checkout.clone());
    if worktree.starts_with(&canonical_checkout) || worktree.starts_with(checkout) {
        return Err(Failure {
            stage: Stage::Worktree,
            message: "The worktree directory must be outside the project checkout.".into(),
        });
    }
    if worktree.exists() {
        return Err(Failure {
            stage: Stage::Worktree,
            message: format!(
                "{} already exists. Nightshift never reuses unrecorded directories.",
                worktree.display()
            ),
        });
    }
    std::fs::create_dir_all(worktree.parent().expect("worktree has a parent")).map_err(
        |error| Failure {
            stage: Stage::Worktree,
            message: format!(
                "Could not create {}: {error}",
                request.worktrees_root.display()
            ),
        },
    )?;
    let path = worktree.to_str().ok_or(Failure {
        stage: Stage::Worktree,
        message: "The worktree path is not valid UTF-8.".into(),
    })?;
    // Record intent first: if `worktree add` partly succeeds, the evidence still names it.
    record(
        recorder,
        WorkingEvidence {
            branch: Some(branch.clone()),
            worktree_path: Some(path.to_owned()),
            ..Default::default()
        },
    )
    .await?;
    note(
        recorder,
        format!("Creating worktree {path} on {branch} at {base_commit}."),
    )
    .await?;
    run(
        checkout,
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            &branch,
            "--",
            path,
            &base_commit,
        ],
        LOCAL_TIMEOUT,
    )
    .await
    .map_err(failure(Stage::Worktree))?;
    Ok(Prepared {
        worktree,
        branch,
        base_commit,
    })
}

/// Run the project's setup command with `sh -c` in the worktree, streaming its output.
async fn setup(worktree: &Path, command: &str, recorder: &dyn Recorder) -> Result<(), Failure> {
    note(recorder, format!("Running setup: {command}")).await?;
    let mut child = crate::platform::command("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(worktree)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| Failure {
            stage: Stage::Setup,
            message: format!("Could not start setup: {error}"),
        })?;
    // Read both pipes concurrently into one bounded channel, preserving line order per pipe.
    let (lines, mut received) = tokio::sync::mpsc::channel::<String>(256);
    let readers: Vec<_> = [
        Box::new(child.stdout.take().expect("piped"))
            as Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box::new(child.stderr.take().expect("piped")),
    ]
    .into_iter()
    .map(|pipe| {
        let lines = lines.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if lines.send(line).await.is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(lines);
    let drain = async {
        while let Some(mut line) = received.recv().await {
            if line.len() > MAX_OUTPUT_BYTES {
                let mut end = MAX_OUTPUT_BYTES;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                line.truncate(end);
            }
            recorder
                .output(OutputSource::Setup, line)
                .await
                .map_err(failure(Stage::Recording))?;
        }
        child.wait().await.map_err(|error| Failure {
            stage: Stage::Setup,
            message: format!("Setup did not finish: {error}"),
        })
    };
    let status = tokio::time::timeout(SETUP_TIMEOUT, drain).await;
    for reader in readers {
        reader.abort();
    }
    let status = status.map_err(|_| Failure {
        stage: Stage::Setup,
        message: "Setup did not finish within 30 minutes and was stopped.".into(),
    })??;
    if !status.success() {
        return Err(Failure {
            stage: Stage::Setup,
            message: format!(
                "Setup failed with {status}. Codex was not started. The worktree is kept for inspection."
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{super::testing::*, *};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log {
        evidence: Mutex<Vec<WorkingEvidence>>,
        output: Mutex<Vec<(OutputSource, String)>>,
    }

    impl Recorder for Log {
        fn evidence(&self, evidence: WorkingEvidence) -> BoxFuture<'_, Result<(), String>> {
            self.evidence.lock().unwrap().push(evidence);
            Box::pin(async { Ok(()) })
        }
        fn output(&self, source: OutputSource, text: String) -> BoxFuture<'_, Result<(), String>> {
            self.output.lock().unwrap().push((source, text));
            Box::pin(async { Ok(()) })
        }
    }

    impl Log {
        fn merged(&self) -> WorkingEvidence {
            let mut merged = WorkingEvidence::default();
            for e in self.evidence.lock().unwrap().iter() {
                merged.default_branch = merged.default_branch.or(e.default_branch.clone());
                merged.base_commit = merged.base_commit.or(e.base_commit.clone());
                merged.branch = merged.branch.or(e.branch.clone());
                merged.worktree_path = merged.worktree_path.or(e.worktree_path.clone());
            }
            merged
        }
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn request(checkout: &Path, root: &Path, execution_id: i64) -> Request {
        Request {
            checkout: checkout.into(),
            remote: "origin".into(),
            default_branch: "main".into(),
            worktrees_root: root.join("worktrees"),
            repository: "owner-repo".into(),
            issue_number: 7,
            issue_title: "Fix the thing: now!".into(),
            execution_id,
            setup_command: None,
            existing: WorkingEvidence::default(),
        }
    }

    /// The owner's checkout state that preparation must never change.
    fn checkout_state(checkout: &Path) -> (String, String, String, String) {
        (
            git(checkout, &["symbolic-ref", "HEAD"]),
            git(checkout, &["rev-parse", "HEAD"]),
            git(
                checkout,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            std::fs::read_to_string(checkout.join("README.md")).unwrap(),
        )
    }

    #[test]
    fn worktree_starts_from_the_fetched_default_branch_and_leaves_the_checkout_unchanged() {
        let (root, remote, checkout) = repository();
        // Someone else pushes to main after the owner's last fetch.
        let other = root.path().join("other");
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        std::fs::write(other.join("NEW.md"), "remote change\n").unwrap();
        git(&other, &["add", "NEW.md"]);
        git(&other, &["commit", "-m", "remote change"]);
        git(&other, &["push", "origin", "main"]);
        let latest = git(&other, &["rev-parse", "HEAD"]);
        // The owner is on another branch with uncommitted edits.
        git(&checkout, &["switch", "-c", "owner-work"]);
        std::fs::write(checkout.join("README.md"), "owner edit\n").unwrap();
        std::fs::write(checkout.join("scratch.txt"), "untracked\n").unwrap();
        let before = checkout_state(&checkout);
        let log = Log::default();
        let prepared = block_on(prepare(&request(&checkout, root.path(), 1), &log)).unwrap();
        assert_eq!(
            prepared.base_commit, latest,
            "base is the fetched default branch, not the owner's HEAD"
        );
        assert_eq!(git(&prepared.worktree, &["rev-parse", "HEAD"]), latest);
        assert_eq!(prepared.branch, "nightshift/7-fix-the-thing-now-1");
        assert!(!prepared.worktree.starts_with(&checkout));
        assert!(prepared.worktree.join("NEW.md").exists());
        assert_eq!(
            checkout_state(&checkout),
            before,
            "owner checkout must not change"
        );
        let evidence = log.merged();
        assert_eq!(evidence.base_commit.as_deref(), Some(latest.as_str()));
        assert_eq!(evidence.branch.as_deref(), Some(prepared.branch.as_str()));
        assert_eq!(
            evidence.worktree_path.as_deref(),
            prepared.worktree.to_str()
        );
    }

    #[test]
    fn branches_and_paths_are_unique_per_execution_and_avoid_existing_branches() {
        let (root, _remote, checkout) = repository();
        let first = block_on(prepare(
            &request(&checkout, root.path(), 1),
            &Log::default(),
        ))
        .unwrap();
        let second = block_on(prepare(
            &request(&checkout, root.path(), 2),
            &Log::default(),
        ))
        .unwrap();
        assert_ne!(first.branch, second.branch);
        assert_ne!(first.worktree, second.worktree);
        // An owner branch that happens to use the generated name is never reused.
        git(&checkout, &["branch", "nightshift/7-fix-the-thing-now-3"]);
        let third = block_on(prepare(
            &request(&checkout, root.path(), 3),
            &Log::default(),
        ))
        .unwrap();
        assert_eq!(third.branch, "nightshift/7-fix-the-thing-now-3-2");
    }

    #[test]
    fn setup_failure_keeps_the_worktree_branch_and_output() {
        let (root, _remote, checkout) = repository();
        let mut req = request(&checkout, root.path(), 1);
        req.setup_command = Some("echo installing; echo broken >&2; exit 3".into());
        let log = Log::default();
        let error = block_on(prepare(&req, &log)).unwrap_err();
        assert_eq!(error.stage, Stage::Setup);
        assert!(
            error.message.contains("Codex was not started"),
            "{}",
            error.message
        );
        let evidence = log.merged();
        let path = PathBuf::from(evidence.worktree_path.unwrap());
        assert!(path.join("README.md").exists(), "worktree is retained");
        let branch = evidence.branch.unwrap();
        git(
            &checkout,
            &["show-ref", "--verify", &format!("refs/heads/{branch}")],
        );
        let output = log.output.lock().unwrap().clone();
        assert!(output.contains(&(OutputSource::Setup, "installing".into())));
        assert!(output.contains(&(OutputSource::Setup, "broken".into())));

        let mut ok = request(&checkout, root.path(), 2);
        ok.setup_command = Some("touch prepared.flag".into());
        let prepared = block_on(prepare(&ok, &Log::default())).unwrap();
        assert!(
            prepared.worktree.join("prepared.flag").exists(),
            "setup runs in the worktree"
        );
        assert!(!checkout.join("prepared.flag").exists());
    }

    #[test]
    fn partial_failures_are_recorded_without_cleanup() {
        let (root, _remote, checkout) = repository();
        // Fetch failure: nothing recorded.
        let mut bad_remote = request(&checkout, root.path(), 1);
        bad_remote.remote = "missing".into();
        let log = Log::default();
        assert_eq!(
            block_on(prepare(&bad_remote, &log)).unwrap_err().stage,
            Stage::Fetch
        );
        assert!(log.evidence.lock().unwrap().is_empty());
        // Worktree failure after the fetch: the base commit stays recorded and the
        // pre-existing directory is left alone.
        let req = request(&checkout, root.path(), 2);
        let blocked = req.worktrees_root.join("owner-repo/7-fix-the-thing-now-2");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("keep.txt"), "owner data").unwrap();
        let log = Log::default();
        let error = block_on(prepare(&req, &log)).unwrap_err();
        assert_eq!(error.stage, Stage::Worktree);
        assert!(log.merged().base_commit.is_some());
        assert_eq!(
            std::fs::read_to_string(blocked.join("keep.txt")).unwrap(),
            "owner data"
        );
    }

    #[test]
    fn retry_reuses_the_recorded_worktree_and_rejects_a_changed_one() {
        let (root, _remote, checkout) = repository();
        let first = block_on(prepare(
            &request(&checkout, root.path(), 1),
            &Log::default(),
        ))
        .unwrap();
        let mut retry = request(&checkout, root.path(), 1);
        retry.existing = WorkingEvidence {
            base_commit: Some(first.base_commit.clone()),
            branch: Some(first.branch.clone()),
            worktree_path: Some(first.worktree.to_str().unwrap().into()),
            ..Default::default()
        };
        let log = Log::default();
        assert_eq!(block_on(prepare(&retry, &log)).unwrap(), first);
        assert!(
            log.evidence.lock().unwrap().is_empty(),
            "no new evidence on reuse"
        );
        git(&first.worktree, &["switch", "-c", "elsewhere"]);
        assert_eq!(
            block_on(prepare(&retry, &Log::default()))
                .unwrap_err()
                .stage,
            Stage::Worktree
        );
    }

    #[test]
    fn slugs_and_branch_names_are_safe() {
        assert_eq!(slug("Fix: the ünïcode/../ thing!!"), "fix-the-n-code-thing");
        assert_eq!(slug("!!!"), "issue");
        assert!(slug(&"word ".repeat(30)).len() <= 40);
        assert!(valid_branch(&branch_name(1, "a/b..c", 9)));
        assert!(!valid_branch("../main"));
        assert!(!valid_branch("-main"));
    }
}
