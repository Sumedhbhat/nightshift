use crate::domain::{NewProject, Project};
use reqwest::Client;
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
pub struct IssueResults {
    pub issues: Vec<Issue>,
    pub error: Option<String>,
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
        path.to_owned()
    } else {
        let url = reqwest::Url::parse(value).ok()?;
        if url.host_str()? != "github.com"
            || !matches!(url.scheme(), "https" | "ssh" | "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        url.path().trim_start_matches('/').to_owned()
    };
    let path = path
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'));
    let (owner, name) = path.split_once('/')?;
    (valid_segment(owner) && valid_segment(name)).then(|| (owner.to_owned(), name.to_owned()))
}

async fn git(path: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("/usr/bin/git")
            .arg("-C")
            .arg(path)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Git took too long to inspect this folder.".to_string())?
    .map_err(|_| "Could not run Git. Install the Xcode command line tools.".to_string())?;
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
            "No github.com remote found. Add a public GitHub remote to this repository first."
                .into(),
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
    private: bool,
}
#[derive(Deserialize)]
struct Label {
    name: String,
}
#[derive(Deserialize)]
struct ApiIssue {
    number: u32,
    title: String,
    labels: Vec<Label>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct GitHub {
    client: Client,
    base: String,
}

impl GitHub {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            client: Client::builder()
                .user_agent("Nightshift/0.1")
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| e.to_string())?,
            base: "https://api.github.com".into(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        target: &str,
    ) -> Result<(T, Option<String>), String> {
        let base = reqwest::Url::parse(&format!("{}/", self.base.trim_end_matches('/')))
            .map_err(|_| "GitHub API configuration is invalid.".to_string())?;
        let url = base
            .join(target)
            .map_err(|_| "GitHub returned an invalid pagination link.".to_string())?;
        if url.scheme() != base.scheme()
            || url.host_str() != base.host_str()
            || url.port_or_known_default() != base.port_or_known_default()
        {
            return Err("GitHub returned a pagination link for an unexpected host.".into());
        }
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .send()
            .await
            .map_err(|_| {
                "Could not reach GitHub. Check your connection and try Search issues again."
                    .to_string()
            })?;
        let status = response.status();
        let next = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(|link| {
                link.split(',')
                    .find(|part| part.contains("rel=\"next\""))
                    .and_then(|part| part.split_once('<'))
                    .and_then(|(_, rest)| rest.split_once('>'))
                    .map(|(url, _)| url.to_owned())
            });
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "GitHub's response was interrupted. Try again.".to_string())?
        {
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err("GitHub's response exceeded the scaffold's 8 MiB page limit.".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            #[derive(Deserialize)]
            struct ApiError {
                message: Option<String>,
            }
            let detail = serde_json::from_slice::<ApiError>(&bytes)
                .ok()
                .and_then(|error| error.message)
                .filter(|message| !message.trim().is_empty())
                .map(|message| format!(" GitHub says: {message}"))
                .unwrap_or_default();
            return Err(match status.as_u16() {
                403 | 429 => format!(
                    "GitHub denied the request or its public API rate limit was reached. Wait before trying again.{detail}"
                ),
                404 => format!(
                    "Public repository not found. Check the Git remote; private repositories are not supported yet.{detail}"
                ),
                status => format!("GitHub returned HTTP {status}.{detail}"),
            });
        }
        let parsed = serde_json::from_slice(&bytes)
            .map_err(|_| "GitHub returned an unreadable response.".to_string())?;
        Ok((parsed, next))
    }

    pub async fn verify(&self, remote: Remote) -> Result<NewProject, String> {
        let (repo, _): (Repository, _) = self
            .get(&format!("/repos/{}/{}", remote.owner, remote.repo))
            .await?;
        if repo.private || !valid_segment(&repo.owner.login) || !valid_segment(&repo.name) {
            return Err("Only public github.com repositories are supported.".into());
        }
        Ok(NewProject {
            checkout_path: remote.checkout,
            github_host: "github.com".into(),
            repository_id: repo.node_id,
            owner: repo.owner.login,
            name: repo.name,
            remote: remote.name,
            setup_command: None,
        })
    }

    pub async fn issue_page(
        &self,
        project: &Project,
        ready_only: bool,
        cursor: Option<&str>,
    ) -> IssuePage {
        let mut result = IssuePage::default();
        if project.github_host != "github.com"
            || !valid_segment(&project.owner)
            || !valid_segment(&project.name)
        {
            result.error =
                Some("This scaffold only supports public github.com repositories.".into());
            return result;
        }
        let target = cursor.map(str::to_owned).unwrap_or_else(|| {
            let filter = if ready_only {
                "&labels=ready-for-agent"
            } else {
                ""
            };
            format!(
                "/repos/{}/{}/issues?state=open&per_page=10&sort=created&direction=desc{filter}",
                project.owner, project.name
            )
        });
        match self.get::<Vec<ApiIssue>>(&target).await {
            Ok((issues, next)) => {
                result.issues = issues
                    .into_iter()
                    .filter(|issue| issue.pull_request.is_none())
                    .map(|issue| Issue {
                        number: issue.number,
                        title: issue.title,
                        labels: issue.labels.into_iter().map(|label| label.name).collect(),
                        url: format!(
                            "https://github.com/{}/{}/issues/{}",
                            project.owner, project.name, issue.number
                        ),
                    })
                    .collect();
                result.next = next;
            }
            Err(error) => result.error = Some(error),
        }
        result
    }

    pub async fn issues(&self, project: &Project, ready_only: bool) -> IssueResults {
        let mut result = IssueResults::default();
        if project.github_host != "github.com"
            || !valid_segment(&project.owner)
            || !valid_segment(&project.name)
        {
            result.error =
                Some("This scaffold only supports public github.com repositories.".into());
            return result;
        }
        let filter = if ready_only {
            "&labels=ready-for-agent"
        } else {
            ""
        };
        let mut target = format!(
            "/repos/{}/{}/issues?state=open&per_page=100&sort=created&direction=desc{filter}",
            project.owner, project.name
        );
        for _ in 0..100 {
            match self.get::<Vec<ApiIssue>>(&target).await {
                Ok((issues, next)) => {
                    for issue in issues
                        .into_iter()
                        .filter(|issue| issue.pull_request.is_none())
                    {
                        if result
                            .issues
                            .iter()
                            .any(|other| other.number == issue.number)
                        {
                            continue;
                        }
                        result.issues.push(Issue {
                            number: issue.number,
                            title: issue.title,
                            labels: issue.labels.into_iter().map(|label| label.name).collect(),
                            url: format!(
                                "https://github.com/{}/{}/issues/{}",
                                project.owner, project.name, issue.number
                            ),
                        });
                    }
                    let Some(next) = next else { return result };
                    target = next;
                }
                Err(error) => {
                    result.error = Some(error);
                    return result;
                }
            }
        }
        result.error = Some("Stopped after 100 pages. Results are incomplete; use the ready-for-agent filter to narrow the search.".into());
        result
    }
}

#[cfg(test)]
mod tests;
