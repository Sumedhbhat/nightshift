//! Owner-requested issue management: create, read, edit, comment, label, assign, close, reopen.
//! Every write is an explicit owner action; runs and executions never call these.
use super::{
    GitHub, Label,
    cli::{GhError, GhResult},
    eligibility::READY_LABEL,
    repo_path,
};
use crate::domain::Project;
use serde::Deserialize;

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct ApiIssue {
    number: u32,
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    assignees: Vec<User>,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ApiComment {
    id: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    created_at: String,
}

/// One issue as GitHub reports it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueDetail {
    pub number: u32,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub state_reason: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub author: String,
    pub created_at: String,
    pub url: String,
}

impl IssueDetail {
    pub fn ready(&self) -> bool {
        self.labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(READY_LABEL))
    }

    pub fn assigned_to(&self, login: &str) -> bool {
        self.assignees
            .iter()
            .any(|assignee| assignee.eq_ignore_ascii_case(login))
    }

    /// The browse row for this issue.
    pub fn summary(&self) -> super::Issue {
        super::Issue {
            number: self.number,
            title: self.title.clone(),
            labels: self.labels.clone(),
            url: self.url.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// An issue with its comments and the active account, as shown in the issue view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueThread {
    pub issue: IssueDetail,
    pub comments: Vec<Comment>,
    pub viewer: String,
}

/// A new issue as the owner wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub body: String,
    pub assign_me: bool,
    pub ready: bool,
}

/// One owner-requested change to an existing issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Edit { title: String, body: String },
    Comment(String),
    Close { not_planned: bool },
    Reopen,
    SetReady(bool),
    SetAssigned(bool),
}

impl Change {
    /// Past-tense status line after the change succeeds.
    pub fn done(&self) -> &'static str {
        match self {
            Change::Edit { .. } => "Issue saved",
            Change::Comment(_) => "Comment posted",
            Change::Close { not_planned: false } => "Issue closed as completed",
            Change::Close { not_planned: true } => "Issue closed as not planned",
            Change::Reopen => "Issue reopened",
            Change::SetReady(true) => "Added ready-for-agent",
            Change::SetReady(false) => "Removed ready-for-agent",
            Change::SetAssigned(true) => "Assigned to you",
            Change::SetAssigned(false) => "Unassigned you",
        }
    }
}

fn detail(project: &Project, issue: ApiIssue) -> GhResult<IssueDetail> {
    if issue.pull_request.is_some() {
        return Err(GhError::Failed(format!(
            "#{} is a pull request, not an issue.",
            issue.number
        )));
    }
    Ok(IssueDetail {
        url: issue.html_url.unwrap_or_else(|| {
            format!(
                "https://github.com/{}/{}/issues/{}",
                project.owner, project.name, issue.number
            )
        }),
        number: issue.number,
        title: issue.title,
        body: issue.body.unwrap_or_default(),
        open: issue.state == "open",
        state_reason: issue.state_reason,
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
        assignees: issue.assignees.into_iter().map(|user| user.login).collect(),
        author: issue.user.map(|user| user.login).unwrap_or_default(),
        created_at: issue.created_at,
    })
}

/// Percent-encode one URL path segment, such as a label name.
fn segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn required(value: &str, what: &str) -> GhResult<()> {
    if value.trim().is_empty() {
        return Err(GhError::Failed(format!("Enter {what} first.")));
    }
    Ok(())
}

impl GitHub {
    /// The issue, its comments, and the active account. Pull requests are rejected.
    pub async fn issue_thread(&self, project: &Project, number: u32) -> GhResult<IssueThread> {
        let path = format!("{}/issues/{number}", repo_path(project)?);
        let viewer = self.gh.active_account().await?.login;
        let issue = detail(project, self.gh.get::<ApiIssue>(&path).await?)?;
        let comments = self
            .gh
            .get_all::<ApiComment>(&format!("{path}/comments?per_page=100"))
            .await?
            .into_iter()
            .map(|comment| Comment {
                id: comment.id,
                author: comment.user.map(|user| user.login).unwrap_or_default(),
                body: comment.body.unwrap_or_default(),
                created_at: comment.created_at,
            })
            .collect();
        Ok(IssueThread {
            issue,
            comments,
            viewer,
        })
    }

    /// Open a new issue, optionally assigned to the active account and labeled ready-for-agent.
    pub async fn create_issue(&self, project: &Project, draft: &Draft) -> GhResult<IssueThread> {
        required(&draft.title, "a title")?;
        let path = format!("{}/issues", repo_path(project)?);
        let viewer = self.gh.active_account().await?.login;
        let mut body = serde_json::json!({"title": draft.title.trim(), "body": draft.body});
        if draft.assign_me {
            body["assignees"] = serde_json::json!([viewer]);
        }
        if draft.ready {
            body["labels"] = serde_json::json!([READY_LABEL]);
        }
        let issue = detail(project, self.gh.post::<ApiIssue>(&path, &body).await?)?;
        Ok(IssueThread {
            issue,
            comments: vec![],
            viewer,
        })
    }

    /// Apply one change, then return the issue as GitHub now reports it.
    pub async fn change_issue(
        &self,
        project: &Project,
        number: u32,
        change: &Change,
    ) -> GhResult<IssueThread> {
        let path = format!("{}/issues/{number}", repo_path(project)?);
        match change {
            Change::Edit { title, body } => {
                required(title, "a title")?;
                self.gh
                    .send::<serde_json::Value>(
                        "PATCH",
                        &path,
                        &serde_json::json!({"title": title.trim(), "body": body}),
                    )
                    .await?;
            }
            Change::Comment(body) => {
                required(body, "a comment")?;
                self.gh
                    .post::<serde_json::Value>(
                        &format!("{path}/comments"),
                        &serde_json::json!({"body": body}),
                    )
                    .await?;
            }
            Change::Close { not_planned } => {
                let reason = if *not_planned {
                    "not_planned"
                } else {
                    "completed"
                };
                self.gh
                    .send::<serde_json::Value>(
                        "PATCH",
                        &path,
                        &serde_json::json!({"state": "closed", "state_reason": reason}),
                    )
                    .await?;
            }
            Change::Reopen => {
                self.gh
                    .send::<serde_json::Value>(
                        "PATCH",
                        &path,
                        &serde_json::json!({"state": "open"}),
                    )
                    .await?;
            }
            Change::SetReady(true) => {
                self.gh
                    .post::<serde_json::Value>(
                        &format!("{path}/labels"),
                        &serde_json::json!({"labels": [READY_LABEL]}),
                    )
                    .await?;
            }
            Change::SetReady(false) => {
                let removed = self
                    .gh
                    .send::<serde_json::Value>(
                        "DELETE",
                        &format!("{path}/labels/{}", segment(READY_LABEL)),
                        &serde_json::json!({}),
                    )
                    .await;
                // A label that is already gone is the requested state.
                match removed {
                    Ok(_) | Err(GhError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            Change::SetAssigned(assign) => {
                let login = self.gh.active_account().await?.login;
                self.gh
                    .send::<serde_json::Value>(
                        if *assign { "POST" } else { "DELETE" },
                        &format!("{path}/assignees"),
                        &serde_json::json!({"assignees": [login]}),
                    )
                    .await?;
            }
        }
        self.issue_thread(project, number).await
    }
}
