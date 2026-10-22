//! Start eligibility from GitHub state. Only GitHub's own data counts: a local execution
//! record never resolves a dependency, and anything that cannot be verified blocks the start.
use super::{
    Label,
    cli::{Gh, GhError},
    repo_path,
};
use crate::domain::{IssueSnapshot, Project};
use serde::Deserialize;

pub const READY_LABEL: &str = "ready-for-agent";

#[derive(Deserialize, Clone)]
struct User {
    login: String,
}

#[derive(Deserialize, Clone, Default)]
struct DependencySummary {
    #[serde(default)]
    total_blocked_by: u32,
}

#[derive(Deserialize, Clone, Default)]
struct SubIssueSummary {
    #[serde(default)]
    total: u32,
}

#[derive(Deserialize, Clone)]
struct ApiIssue {
    node_id: String,
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
    pull_request: Option<serde_json::Value>,
    #[serde(default)]
    parent_issue_url: Option<String>,
    #[serde(default)]
    issue_dependencies_summary: Option<DependencySummary>,
    #[serde(default)]
    sub_issues_summary: Option<SubIssueSummary>,
    #[serde(default)]
    repository_url: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

impl ApiIssue {
    fn ready(&self) -> bool {
        self.labels
            .iter()
            .any(|label| label.name.eq_ignore_ascii_case(READY_LABEL))
    }

    fn assigned_to(&self, login: &str) -> bool {
        self.assignees
            .iter()
            .any(|user| user.login.eq_ignore_ascii_case(login))
    }

    fn is_parent(&self) -> bool {
        self.sub_issues_summary
            .as_ref()
            .is_some_and(|s| s.total > 0)
    }

    fn snapshot(&self) -> IssueSnapshot {
        IssueSnapshot {
            id: self.node_id.clone(),
            number: self.number,
            title: self.title.clone(),
            description: self.body.clone().unwrap_or_default(),
        }
    }

    fn repository(&self) -> String {
        self.repository_url
            .as_deref()
            .and_then(|url| url.strip_prefix("https://api.github.com/repos/"))
            .unwrap_or("unknown repository")
            .to_owned()
    }
}

/// An open, assigned, labeled issue found by discovery. Eligibility is checked separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub project_id: i64,
    pub issue_id: String,
    pub number: u32,
    pub title: String,
    pub url: String,
}

/// Discovery across projects. A failing project reports an error without hiding others.
#[derive(Debug, Default)]
pub struct CandidateSearch {
    pub candidates: Vec<Candidate>,
    pub errors: Vec<(i64, GhError)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub open: bool,
    /// GitHub's closure reason, absent for open issues and for some closed ones.
    pub state_reason: Option<String>,
    /// Declared on the open parent rather than the issue itself.
    pub inherited: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    NotOpen,
    PullRequest,
    NotAssigned {
        login: String,
    },
    MissingLabel,
    IsParent,
    ParentClosed {
        number: u32,
    },
    ParentMissingLabel {
        number: u32,
    },
    ParentUnverifiable {
        detail: String,
    },
    Blocked(Blocker),
    DependenciesUnverifiable {
        number: u32,
        inherited: bool,
        detail: String,
    },
    Unverifiable {
        detail: String,
    },
}

impl Problem {
    pub fn describe(&self) -> String {
        match self {
            Problem::NotOpen => "The issue is closed.".into(),
            Problem::PullRequest => "This is a pull request, not an issue.".into(),
            Problem::NotAssigned { login } => format!("The issue is not assigned to {login}."),
            Problem::MissingLabel => format!("The issue does not have `{READY_LABEL}`."),
            Problem::IsParent => "Parent issues provide context and are not executed.".into(),
            Problem::ParentClosed { number } => format!("Parent #{number} is closed."),
            Problem::ParentMissingLabel { number } => {
                format!("Parent #{number} does not have `{READY_LABEL}`.")
            }
            Problem::ParentUnverifiable { detail } => {
                format!("The parent issue could not be verified: {detail}")
            }
            Problem::Blocked(blocker) => format!(
                "Blocked by {}#{} ({}), which is open{}.",
                blocker.repository,
                blocker.number,
                blocker.title,
                if blocker.inherited {
                    " (inherited from the parent)"
                } else {
                    ""
                }
            ),
            Problem::DependenciesUnverifiable {
                number,
                inherited,
                detail,
            } => format!(
                "Dependencies of {}#{number} could not be verified: {detail}",
                if *inherited { "parent " } else { "" }
            ),
            Problem::Unverifiable { detail } => {
                format!("Eligibility could not be verified: {detail}")
            }
        }
    }
}

/// Outcome of a start-eligibility check. Eligible only when `problems` is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eligibility {
    pub issue: Option<IssueSnapshot>,
    pub parent: Option<IssueSnapshot>,
    /// Every explicit blocker, resolved or not, for display.
    pub blockers: Vec<Blocker>,
    pub problems: Vec<Problem>,
}

impl Eligibility {
    pub fn eligible(&self) -> bool {
        self.problems.is_empty() && self.issue.is_some()
    }

    fn unverifiable(detail: String) -> Self {
        Self {
            issue: None,
            parent: None,
            blockers: vec![],
            problems: vec![Problem::Unverifiable { detail }],
        }
    }
}

fn issue_path(url: &str) -> Option<String> {
    let path = super::cli::api_path(url)?;
    path.starts_with("repos/").then_some(path)
}

/// Open issues assigned to `login` and labeled ready, excluding pull requests and parents.
pub async fn candidates(gh: &Gh, projects: &[Project], login: &str) -> CandidateSearch {
    let mut search = CandidateSearch::default();
    for project in projects {
        let path = match repo_path(project) {
            Ok(path) => path,
            Err(error) => {
                search.errors.push((project.id, error));
                continue;
            }
        };
        let target = format!(
            "{path}/issues?state=open&assignee={login}&labels={READY_LABEL}&per_page=100&sort=created&direction=asc"
        );
        match gh.get_all::<ApiIssue>(&target).await {
            Ok(issues) => search.candidates.extend(
                issues
                    .into_iter()
                    .filter(|issue| {
                        issue.pull_request.is_none()
                            && issue.state == "open"
                            && issue.assigned_to(login)
                            && issue.ready()
                            && !issue.is_parent()
                    })
                    .map(|issue| Candidate {
                        project_id: project.id,
                        url: issue.html_url.clone().unwrap_or_default(),
                        issue_id: issue.node_id,
                        number: issue.number,
                        title: issue.title,
                    }),
            ),
            Err(error) => search.errors.push((project.id, error)),
        }
    }
    search
}

/// Re-read the issue and evaluate every GitHub start gate for `login`.
pub async fn check(gh: &Gh, project: &Project, number: u32, login: &str) -> Eligibility {
    let path = match repo_path(project) {
        Ok(path) => format!("{path}/issues/{number}"),
        Err(error) => return Eligibility::unverifiable(error.to_string()),
    };
    let issue: ApiIssue = match gh.get(&path).await {
        Ok(issue) => issue,
        Err(error) => return Eligibility::unverifiable(error.to_string()),
    };
    let mut result = Eligibility {
        issue: Some(issue.snapshot()),
        parent: None,
        blockers: vec![],
        problems: vec![],
    };
    if issue.pull_request.is_some() {
        result.problems.push(Problem::PullRequest);
    }
    if issue.state != "open" {
        result.problems.push(Problem::NotOpen);
    }
    if !issue.assigned_to(login) {
        result.problems.push(Problem::NotAssigned {
            login: login.into(),
        });
    }
    if !issue.ready() {
        result.problems.push(Problem::MissingLabel);
    }
    if issue.is_parent() {
        result.problems.push(Problem::IsParent);
    }
    dependencies(gh, &path, &issue, false, &mut result).await;
    if let Some(parent_url) = &issue.parent_issue_url {
        match issue_path(parent_url) {
            None => result.problems.push(Problem::ParentUnverifiable {
                detail: format!("unexpected parent URL {parent_url}"),
            }),
            Some(parent_path) => match gh.get::<ApiIssue>(&parent_path).await {
                Err(error) => result.problems.push(Problem::ParentUnverifiable {
                    detail: error.to_string(),
                }),
                Ok(parent) => {
                    if parent.state != "open" {
                        result.problems.push(Problem::ParentClosed {
                            number: parent.number,
                        });
                    } else {
                        // An open parent's dependencies gate its children too.
                        dependencies(gh, &parent_path, &parent, true, &mut result).await;
                    }
                    if !parent.ready() {
                        result.problems.push(Problem::ParentMissingLabel {
                            number: parent.number,
                        });
                    }
                    result.parent = Some(parent.snapshot());
                }
            },
        }
    }
    result
}

/// Explicit "blocked by" relationships. A blocker resolves only when GitHub reports it closed,
/// whatever the closure reason. Blockers GitHub counts but does not list are inaccessible to the
/// active account and block the start.
async fn dependencies(
    gh: &Gh,
    path: &str,
    issue: &ApiIssue,
    inherited: bool,
    result: &mut Eligibility,
) {
    let target = format!("{path}/dependencies/blocked_by?per_page=100");
    match gh.get_all::<ApiIssue>(&target).await {
        Err(error) => result.problems.push(Problem::DependenciesUnverifiable {
            number: issue.number,
            inherited,
            detail: error.to_string(),
        }),
        Ok(blockers) => {
            let expected = issue
                .issue_dependencies_summary
                .as_ref()
                .map_or(0, |s| s.total_blocked_by);
            if (blockers.len() as u32) < expected {
                result.problems.push(Problem::DependenciesUnverifiable {
                    number: issue.number,
                    inherited,
                    detail: format!(
                        "GitHub reports {expected} blockers but the active account can read only {}.",
                        blockers.len()
                    ),
                });
            }
            for blocker in blockers {
                let entry = Blocker {
                    repository: blocker.repository(),
                    number: blocker.number,
                    title: blocker.title.clone(),
                    open: blocker.state != "closed",
                    state_reason: blocker.state_reason.clone(),
                    inherited,
                };
                if entry.open {
                    result.problems.push(Problem::Blocked(entry.clone()));
                }
                result.blockers.push(entry);
            }
        }
    }
}
