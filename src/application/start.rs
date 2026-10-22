//! Start eligibility: every GitHub and local gate, evaluated immediately before reservation.
use crate::{
    domain::{IssueSnapshot, Project},
    github::{
        GitHub, RepositoryAccess,
        cli::GhError,
        eligibility::{self, Eligibility},
    },
};
use std::path::Path;

/// Why a start cannot proceed. Project-level gates block every issue in that project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The CLI is missing, logged out, incompatible, or signed in as someone else.
    Identity(GhError),
    /// The repository cannot be read, or no longer matches the enrolled one.
    Access(GhError),
    ReadOnly,
    Archived,
    PushPreflight(String),
    Issue(eligibility::Problem),
}

impl Gate {
    /// Project-level gates stop new work for the whole project, not just one issue.
    pub fn blocks_project(&self) -> bool {
        !matches!(self, Gate::Issue(_))
    }

    /// Whether the failure is GitHub-side and transient, so the run should back off.
    pub fn is_transient(&self) -> bool {
        matches!(self, Gate::Identity(e) | Gate::Access(e) if e.is_transient())
    }

    pub fn describe(&self) -> String {
        match self {
            Gate::Identity(error) | Gate::Access(error) => error.to_string(),
            Gate::ReadOnly => "The active account cannot push to this repository. Nightshift needs direct push access; forks are not supported.".into(),
            Gate::Archived => "This repository is archived and cannot accept pushes.".into(),
            Gate::PushPreflight(error) => error.clone(),
            Gate::Issue(problem) => problem.describe(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCheck {
    pub gates: Vec<Gate>,
    pub issue: Option<IssueSnapshot>,
    pub parent: Option<IssueSnapshot>,
    pub default_branch: Option<String>,
    pub eligibility: Option<Eligibility>,
}

impl StartCheck {
    pub fn eligible(&self) -> bool {
        self.gates.is_empty() && self.issue.is_some() && self.default_branch.is_some()
    }

    pub fn summary(&self) -> String {
        self.gates
            .iter()
            .map(Gate::describe)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn blocked(gate: Gate) -> Self {
        Self {
            gates: vec![gate],
            issue: None,
            parent: None,
            default_branch: None,
            eligibility: None,
        }
    }
}

/// Verify identity, access, push capability, and the issue's own gates, in that order.
/// Each project-level failure stops further checks so no slot is consumed.
pub async fn check(github: &GitHub, project: &Project, number: u32, run_owner: &str) -> StartCheck {
    if let Err(error) = github.gh.verify_owner(run_owner).await {
        return StartCheck::blocked(Gate::Identity(error));
    }
    let access: RepositoryAccess = match github.access(project).await {
        Ok(access) => access,
        Err(error) => return StartCheck::blocked(Gate::Access(error)),
    };
    if access.archived {
        return StartCheck::blocked(Gate::Archived);
    }
    if !access.can_push {
        return StartCheck::blocked(Gate::ReadOnly);
    }
    if let Err(error) =
        crate::git::push_preflight(Path::new(&project.checkout_path), &project.remote).await
    {
        return StartCheck::blocked(Gate::PushPreflight(error));
    }
    let result = eligibility::check(&github.gh, project, number, run_owner).await;
    StartCheck {
        gates: result.problems.iter().cloned().map(Gate::Issue).collect(),
        issue: result.issue.clone(),
        parent: result.parent.clone(),
        default_branch: Some(access.default_branch).filter(|branch| !branch.is_empty()),
        eligibility: Some(result),
    }
}

/// Monotonic request generations, so late results never overwrite a newer search.
#[derive(Debug, Default)]
pub struct Generations {
    latest: u64,
}

impl Generations {
    pub fn advance(&mut self) -> u64 {
        self.latest += 1;
        self.latest
    }

    /// Accept results only from the most recent request.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.latest
    }
}
