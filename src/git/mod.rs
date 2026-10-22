//! Git subprocesses with argument arrays. Credentials stay owner-managed: Nightshift never
//! changes Git configuration and disables every interactive prompt.
pub mod worktree;

use std::{path::Path, time::Duration};

pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Build a non-interactive Git command. If the owner has not configured an SSH command, use
/// OpenSSH batch mode so a passphrase or host-key prompt fails instead of waiting.
pub fn command(directory: &Path) -> tokio::process::Command {
    let mut command = crate::platform::command("git");
    command
        .arg("-C")
        .arg(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if crate::platform::current()
        .var_os("GIT_SSH_COMMAND")
        .is_none()
        && crate::platform::current().var_os("GIT_SSH").is_none()
    {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    command
}

/// Run Git, returning stdout on success or a message with Git's stderr.
pub async fn run(directory: &Path, args: &[&str], timeout: Duration) -> Result<GitOutput, String> {
    let output = tokio::time::timeout(timeout, command(directory).args(args).output())
        .await
        .map_err(|_| {
            format!(
                "`git {}` did not finish within {} seconds.",
                args.join(" "),
                timeout.as_secs()
            )
        })?
        .map_err(|error| format!("Could not run Git: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed{}",
            args.join(" "),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(GitOutput { stdout, stderr })
}

/// Verify the selected remote accepts a non-interactive push without changing it.
/// `git push --dry-run` authenticates and negotiates with the remote's receive service but
/// sends no objects and updates no refs.
pub async fn push_preflight(checkout: &Path, remote: &str) -> Result<(), String> {
    let probe = format!(
        "HEAD:refs/heads/nightshift-preflight-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    run(
        checkout,
        &["push", "--dry-run", "--porcelain", "--no-verify", "--", remote, &probe],
        NETWORK_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        format!("Push preflight to `{remote}` failed. Fix this remote's SSH or HTTPS credentials outside Nightshift. {error}")
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    pub fn git(directory: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A bare "remote" plus a clone with one commit on `main`.
    pub fn repository() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        let checkout = root.path().join("checkout");
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            root.path(),
            &["init", "--initial-branch=main", checkout.to_str().unwrap()],
        );
        std::fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        git(&checkout, &["add", "README.md"]);
        git(&checkout, &["commit", "-m", "initial"]);
        git(
            &checkout,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&checkout, &["push", "origin", "main"]);
        (root, remote, checkout)
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::*, *};

    fn run_async<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn push_preflight_accepts_a_writable_remote_without_changing_it() {
        let (_root, remote, checkout) = repository();
        let before = git(&remote, &["for-each-ref"]);
        run_async(push_preflight(&checkout, "origin")).unwrap();
        assert_eq!(
            git(&remote, &["for-each-ref"]),
            before,
            "dry run must not create refs"
        );
    }

    #[test]
    fn push_preflight_rejects_an_unreachable_remote() {
        let (root, _remote, checkout) = repository();
        let missing = root.path().join("missing.git");
        git(
            &checkout,
            &["remote", "add", "broken", missing.to_str().unwrap()],
        );
        let error = run_async(push_preflight(&checkout, "broken")).unwrap_err();
        assert!(
            error.contains("Push preflight to `broken` failed"),
            "{error}"
        );
        assert!(run_async(push_preflight(&checkout, "no-such-remote")).is_err());
    }
}
