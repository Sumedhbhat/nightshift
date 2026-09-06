# Nightshift

## Purpose

Nightshift is a planned desktop application for turning the owner's software ideas into working software through autonomous work at night.

The owner captures what a project needs as issues and develops those issues during the day or after work on weekdays. A nightly job should pick up prepared work and build the software while the owner is away.

The owner always supplies an existing repository, which may be empty. The first version supports GitHub only, with work tracked in GitHub Issues on the supplied repository. Creating repositories is outside Nightshift's scope.

## Intended experience

- Use a desktop interface to configure and manage the workflow.
- Let the owner view existing issues and work on their details through the GUI.
- Provide AI-assisted issue refinement and breakdown through an existing coding-agent chat foundation. Use [T3 Code](https://github.com/pingdotgg/t3code) as the reference for this experience; do not build a new chat system from scratch for the MVP.
- Save configurations created through the GUI to files. JSON is one possible format; the format has not been chosen.
- Support both local execution on the owner's computer and remote execution. Let the owner choose through the desktop app and save that choice in configuration.
- Let the owner trigger configured tests and additional review or refactoring cycles from the GUI, with fresh chats for review or refactoring.

## Nightly workflow

- Give agents access to the supplied repository's issues, including their descriptions, labels, and comments, so they can identify ready work and read the owner's answers. The workflow also requires posting questions and linking resulting PRs to those issues.
- Use the `ready-for-agent` label to mark issues as eligible for autonomous nightly work. Issues without this label are not eligible.
- Link each pull request produced by Nightshift to the issue it addresses, so the pull request can be found from the issue itself.
- Leave pull requests open for the owner's review. Never merge them automatically.
- If an unclear requirement prevents progress, post a question as a comment on the issue for the owner to answer the next morning. Pause that issue and continue with other ready issues. Routine implementation choices can be made autonomously.
- On the next nightly run, resume a paused issue if the owner's reply resolves the question. The owner does not need to reapply `ready-for-agent`.
- Automatically run the configured tests and have the AI review the code in a fresh chat before handing the PR to the owner for review.

## Issue hierarchy and dependencies

- Support breaking a large goal into a parent issue and smaller child issues. Let the owner express this hierarchy through the app.
- Use GitHub's parent/sub-issue relationships and explicit blocked-by/blocking relationships. Let the owner view and set these relationships through the app.
- Dependencies must be explicitly marked. Sharing a parent does not impose an execution order on child issues.
- Show both "Blocked by" and "Blocking" for each issue in the GUI, with links to the related issues. Make it clear which prerequisites are unresolved and why an issue cannot start.
- Mark child issues individually with `ready-for-agent`; a label on the parent does not automatically make its children ready.
- The `ready-for-agent` label does not override unresolved dependencies. Check the explicit dependency relationships before starting work.
- When working on a child issue, agents must read the parent issue for context and understand how the child's work contributes to the broader goal.
- If an issue depends on another issue's changes, wait until the prerequisite PR is merged before starting the dependent work. Continue with independent ready issues while waiting.

## Execution and merge conflicts

- Work on multiple independent ready issues in parallel. Do not impose a one-issue-at-a-time restriction to avoid merge conflicts. An open PR awaiting the owner's review does not prevent work on another independent ready issue.
- When a PR merges, automatically check Nightshift's other open PRs for merge conflicts and repair affected PRs.
- Give the agent repairing a conflict the context of both the issue behind the merged changes and the issue behind the affected PR. The resolution must preserve the requirements of both issues.
- After repairing a conflict, rerun the configured tests and perform a fresh AI review of the updated change. The PR still requires the owner's review and must never be merged automatically.
- If satisfying both issues requires clarification, post the question on the affected issue and pause that work for the owner's answer.

## Testing and review

- Each AI review starts in a fresh chat and examines the change itself, without inheriting the implementation conversation or earlier review conversations. This applies to reviews after fixes as well as the initial review.
- Define deterministic test checks and their commands in configuration files. Nightshift must use these configured checks rather than choose arbitrary tests during a run.
- Each project must expose a single project-wide command that runs all its test files.
- When a review finds problems, fix them automatically, rerun the configured checks, and review the updated changes again.
- Record the feedback that prompted each fix and the changes made in response.
- Allow at most four retries of the fix, test, and review cycle. If problems remain after the limit, record what is unresolved on the PR and wait for the owner.

## Product constraints

- Build on the Codex platform's existing capabilities. Do not build a custom agent harness from scratch.
- Reuse an established coding-agent interface and keep the application extensible. T3 Code is the reference and a candidate foundation; the exact reuse or integration approach has not been selected.
- Design for the owner's personal workflow of preparing issues when available and having work carried out autonomously at night.
- Keep these intentions in mind when making application decisions.
- Keep this file focused on project purpose, user workflow, and agreed product constraints. Put technical design details elsewhere.

## Current state

The initial MVP product direction is captured in this document. The repository currently contains project documentation, not an implemented application. Technical choices, including the chat foundation's integration, desktop framework, remote execution setup, and configuration format, remain open.

See [CONTEXT.md](CONTEXT.md) for agreed project terminology.
