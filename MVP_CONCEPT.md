# Nightshift MVP

A local macOS desktop app for manually finding a GitHub issue and starting Codex work on it.

## Workflow

1. Add a project by selecting an existing, initialized local repository. Identify its GitHub repository from its Git remote.
2. Click **Search issues** to list open issues labeled `ready-for-agent` across all added projects.
3. Choose an issue and click **Start execution**. Require its own readiness label and resolved dependencies. Show why a blocked issue cannot start.
4. Save an execution record in the local database, including the issue identity, starting description, status, and creation time.
5. Create a dedicated worktree and branch from the latest fetched GitHub default branch. Run the project's optional setup command, then start Codex in that worktree.
6. Show execution status, agent output, and errors.

## Rules

- The owner initiates every search and execution. Finishing an execution does not trigger another search or start.
- Allow at most two issues in progress across all projects combined. Prevent duplicate active executions of the same issue.
- Use the saved description for the execution. The local database owns execution state and retains a basic history of state changes.
- Keep agent changes in the issue's worktree, separate from the owner's original checkout.
- Scope ends at searching, starting, and observing execution. Do not add scheduling, issue editing, a code editor, or automated PR, review, retry, recovery, or cleanup workflows.

See [docs/desktop-foundation.md](docs/desktop-foundation.md) for implementation references and [CONTEXT.md](CONTEXT.md) for terminology.
