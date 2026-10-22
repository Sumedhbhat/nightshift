//! GitHub access through the authenticated GitHub CLI, plus local checkout inspection.
pub mod cli;
pub mod eligibility;

use crate::domain::{NewProject, Project};
use cli::{Gh, GhError};
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub checkout: String,
    pub name: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub number: u32,
    pub title: String,
    pub labels: Vec<String>,
    pub url: String,
}

#[derive(Default)]
pub struct IssuePage {
    pub issues: Vec<Issue>,
    pub next: Option<String>,
    pub error: Option<String>,
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

pub fn parse_remote(value: &str) -> Option<(String, String)> {
    let path = if let Some(path) = value.strip_prefix("git@github.com:") {
        path
    } else {
        let (scheme, rest) = value.split_once("://")?;
        if !matches!(scheme, "https" | "ssh" | "git") || rest.contains(['?', '#']) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host
            .strip_suffix(":443")
            .or_else(|| host.strip_suffix(":22"))
            .unwrap_or(host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (valid_segment(owner) && valid_segment(name)).then(|| (owner.to_owned(), name.to_owned()))
}

async fn git(path: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Git took too long to inspect this folder.".to_string())?
    .map_err(|_| {
        "Could not run Git. Install Git (on macOS, the Xcode command line tools).".to_string()
    })?;
    if !output.status.success() {
        return Err("Choose an initialized Git repository with at least one commit.".into());
    }
    String::from_utf8(output.stdout)
        .map_err(|_| "Git returned an unsupported path or remote name.".into())
}

pub async fn inspect_checkout(path: PathBuf) -> Result<Vec<Remote>, String> {
    let root = git(&path, &["rev-parse", "--show-toplevel"]).await?;
    let root = PathBuf::from(root.trim_end());
    git(&root, &["rev-parse", "--verify", "HEAD"]).await?;
    let canonical = tokio::task::spawn_blocking(move || root.canonicalize())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let checkout = canonical
        .to_str()
        .ok_or("The checkout path is not valid UTF-8.")?
        .to_string();
    let names = git(&canonical, &["remote"]).await?;
    let mut remotes = Vec::new();
    for name in names.lines() {
        let url = git(&canonical, &["remote", "get-url", "--", name]).await?;
        if let Some((owner, repo)) = parse_remote(url.trim()) {
            remotes.push(Remote {
                checkout: checkout.clone(),
                name: name.into(),
                owner,
                repo,
            });
        }
    }
    remotes.sort_by_key(|remote| (remote.name != "origin", remote.name.clone()));
    let mut unique = Vec::<Remote>::new();
    for remote in remotes {
        if !unique.iter().any(|other| {
            other.owner.eq_ignore_ascii_case(&remote.owner)
                && other.repo.eq_ignore_ascii_case(&remote.repo)
        }) {
            unique.push(remote);
        }
    }
    if unique.is_empty() {
        return Err(
            "No github.com remote found. Add a GitHub remote to this repository first.".into(),
        );
    }
    Ok(unique)
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Deserialize)]
struct Repository {
    node_id: String,
    name: String,
    owner: Owner,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    permissions: Option<Permissions>,
}

#[derive(Deserialize, Default, Clone, Copy)]
struct Permissions {
    #[serde(default)]
    push: bool,
}

#[derive(Deserialize, Clone)]
pub(crate) struct Label {
    pub name: String,
}

#[derive(Deserialize)]
struct ApiIssue {
    number: u32,
    title: String,
    labels: Vec<Label>,
    html_url: Option<String>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PullRequestHead {
    #[serde(rename = "ref")]
    pub ref_name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u32,
    pub html_url: String,
    pub node_id: String,
    pub head: PullRequestHead,
}

/// What the active account may do with a repository, checked before each new execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryAccess {
    pub default_branch: String,
    pub can_push: bool,
    pub archived: bool,
}

pub fn repo_path(project: &Project) -> Result<String, GhError> {
    if project.github_host != cli::HOST
        || !valid_segment(&project.owner)
        || !valid_segment(&project.name)
    {
        return Err(GhError::Failed(
            "Only github.com repositories are supported.".into(),
        ));
    }
    Ok(format!("repos/{}/{}", project.owner, project.name))
}

/// Repository operations through the authenticated GitHub CLI. No anonymous fallback exists.
#[derive(Clone)]
pub struct GitHub {
    pub gh: Gh,
}

impl GitHub {
    pub fn new(gh: Gh) -> Self {
        Self { gh }
    }

    pub fn from_env() -> Self {
        Self::new(Gh::from_env())
    }

    /// Admit a public or private repository the active account can access.
    pub async fn verify(&self, remote: Remote) -> Result<NewProject, String> {
        self.gh.probe().await.map_err(|e| e.to_string())?;
        if !valid_segment(&remote.owner) || !valid_segment(&remote.repo) {
            return Err("The remote does not name a valid GitHub repository.".into());
        }
        let repo: Repository = self
            .gh
            .get(&format!("repos/{}/{}", remote.owner, remote.repo))
            .await
            .map_err(|error| match error {
                GhError::NotFound(_) => format!(
                    "{}/{} was not found, or the active GitHub account cannot access it.",
                    remote.owner, remote.repo
                ),
                other => other.to_string(),
            })?;
        if !valid_segment(&repo.owner.login) || !valid_segment(&repo.name) {
            return Err("GitHub returned an unsupported repository name.".into());
        }
        Ok(NewProject {
            checkout_path: remote.checkout,
            github_host: cli::HOST.into(),
            repository_id: repo.node_id,
            owner: repo.owner.login,
            name: repo.name,
            remote: remote.name,
            setup_command: None,
        })
    }

    /// Re-read access for a start: the repository must still be the enrolled one and writable.
    pub async fn access(&self, project: &Project) -> Result<RepositoryAccess, GhError> {
        let repo: Repository = self.gh.get(&repo_path(project)?).await?;
        if repo.node_id != project.repository_id {
            return Err(GhError::Failed(format!(
                "{}/{} now resolves to a different repository than the one added.",
                project.owner, project.name
            )));
        }
        Ok(RepositoryAccess {
            default_branch: repo.default_branch,
            can_push: repo.permissions.unwrap_or_default().push,
            archived: repo.archived,
        })
    }

    /// Whether the issue is open now: the sole publication gate.
    pub async fn issue_open(&self, project: &Project, number: u32) -> Result<bool, GhError> {
        #[derive(Deserialize)]
        struct State {
            state: String,
        }
        let state: State = self
            .gh
            .get(&format!("{}/issues/{number}", repo_path(project)?))
            .await?;
        Ok(state.state == "open")
    }

    /// An existing pull request whose head is exactly this repository's `branch`, in any state.
    pub async fn pull_request_for(
        &self,
        project: &Project,
        branch: &str,
    ) -> Result<Option<PullRequest>, GhError> {
        let pulls: Vec<PullRequest> = self
            .gh
            .get(&format!(
                "{}/pulls?head={}:{branch}&state=all&per_page=10",
                repo_path(project)?,
                project.owner
            ))
            .await?;
        Ok(pulls.into_iter().find(|pull| pull.head.ref_name == branch))
    }

    /// Open a pull request from `branch` in the same repository into `base`.
    pub async fn create_pull_request(
        &self,
        project: &Project,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, GhError> {
        self.gh
            .post(
                &format!("{}/pulls", repo_path(project)?),
                &serde_json::json!({"title": title, "head": branch, "base": base, "body": body}),
            )
            .await
    }

    /// One browse page of open issues (at most ten), following GitHub's next link.
    pub async fn issue_page(&self, project: &Project, cursor: Option<&str>) -> IssuePage {
        let mut result = IssuePage::default();
        let target = match (cursor, repo_path(project)) {
            (Some(cursor), _) => cursor.to_owned(),
            (None, Ok(path)) => {
                format!("{path}/issues?state=open&per_page=10&sort=created&direction=desc")
            }
            (None, Err(error)) => {
                result.error = Some(error.to_string());
                return result;
            }
        };
        match self.gh.get_page::<Vec<ApiIssue>>(&target).await {
            Ok((issues, next)) => {
                result.issues = issues
                    .into_iter()
                    .filter(|issue| issue.pull_request.is_none())
                    .map(|issue| Issue {
                        url: issue.html_url.unwrap_or_else(|| {
                            format!(
                                "https://github.com/{}/{}/issues/{}",
                                project.owner, project.name, issue.number
                            )
                        }),
                        number: issue.number,
                        title: issue.title,
                        labels: issue.labels.into_iter().map(|label| label.name).collect(),
                    })
                    .collect();
                result.next = next;
            }
            Err(error) => result.error = Some(error.to_string()),
        }
        result
    }
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
