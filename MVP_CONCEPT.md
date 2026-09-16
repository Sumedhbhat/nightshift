# Nightshift MVP

A local macOS desktop app for finding GitHub issues and running owner-initiated Codex work that can continue unattended through dependent issues, producing stacked pull requests for manual review and merging.

## Planning status

The stacked-PR direction replaces the earlier manual-only workflow. The current progression rule is simple: B starts after its prerequisite A finishes, building on A's changes. Detailed completion criteria and review phases are deferred. The workload consists of open issues labeled `ready-for-agent` across added projects, with a configurable concurrency limit. Active runs automatically discover newly labeled issues. Parents provide context rather than separate executions; their completion depends on their children. Failure handling and the precise child-completion rule for parent closure remain to be defined.

## Current scaffold

The initial usable scaffold lets the owner add local repositories and fetch open issues from public GitHub repositories. It defaults to all open issues and offers a `ready-for-agent` filter. This browsing mode does not authorize execution or relax the run readiness rules below. Execution controls are not implemented yet.

## Workflow

1. Add a project by selecting an existing, initialized local repository. Identify its GitHub repository from its Git remote.
2. Click **Search issues** to list open issues labeled `ready-for-agent` across all added projects.
3. The owner starts a run over open issues labeled `ready-for-agent` across added projects. Start eligible issues up to the concurrency limit configured in Settings. Require each issue's own readiness label, its parent's readiness label when a parent exists, and eligible dependencies; show why blocked issues cannot start.
4. Save an execution record in the local database, including the issue identity, starting description, parent context when present, status, and creation time. Give the agent the child issue as its task and the parent issue as the big-picture context.
5. Create a dedicated worktree and branch. Start independent work from the latest fetched GitHub default branch; build dependent work on its eligible predecessor's unmerged branch. Run the project's optional setup command, then start Codex in that worktree.
6. Show execution status, agent output, and errors.
7. Publish execution branches and open pull requests. A dependent issue's PR targets its predecessor's branch, forming a stack. The timing of publication relative to successor startup is not yet specified.
8. When A finishes, automatically start eligible dependent issue B within the owner's authorized scope, building on A's changes. The owner reviews and merges the resulting PRs manually.

## Rules

- The owner starts unattended work. While a run is active, Nightshift automatically refreshes open `ready-for-agent` issues across all added projects and starts eligible work as capacity becomes available, including issues labeled after the run began. Manual search remains available.
- An issue must have `ready-for-agent` itself. If it has a parent issue, that parent must also have `ready-for-agent`; a missing or unverifiable parent label prevents a start. This readiness requirement does not make the parent a dependency or require it to finish first.
- Parent issues provide context for their child executions and are not executed separately. A parent can be closed once all its children are completed. Who closes it and the precise meaning of child completion for closure remain to be settled.
- A dependency on a parent issue depends on completion of all that parent's children. Merely labeling or closing the parent does not replace checking its children.
- A child's eligibility includes both its own explicit dependencies and its parent's explicit dependencies. If parent P depends on parent Q, P's children wait until all of Q's children finish.
- Use one sequential PR stack per parent. Execute one child at a time, respecting explicit dependencies; each subsequent child builds on the previous child's branch so the final branch contains the stack's changes. Different parents may run concurrently within the application-wide concurrency limit. Sharing a parent does not create a GitHub dependency, but it does serialize execution for stacking.
- Use GitHub's explicit "blocked by" relationships as the source of issue dependencies. Shared parents and references in descriptions or comments do not create dependencies. The owner maintains these relationships in GitHub.
- For an executable issue, a dependency is resolved when its blocking issue is closed as completed. For a parent blocker, also evaluate completion of its children. Stacking adds a separate way to proceed before resolution: B starts after A finishes and builds on A's unmerged changes. Detailed completion criteria and review phases are deferred; this does not define every process exit as completion. Issues closed for other reasons and unverifiable prerequisites remain blocking; the owner manually corrects obsolete or incorrect relationships in GitHub.
- Settings controls the concurrency limit: the maximum number of executions in progress across all projects combined. This limits simultaneous work, not the total number of issues in a run. Fill available slots with eligible issues as executions finish. Prevent duplicate active executions of the same issue.
- Use the saved description for the execution. The local database owns execution state and retains a basic history of state changes.
- Keep agent changes in the issue's worktree, separate from the owner's original checkout.
- Quitting Nightshift stops active executions, including any preparation work. Preserve their saved output, history, branches, and worktrees, and record stopped executions as interrupted. Do not resume them automatically.
- Interrupted executions remain available for inspection after reopening Nightshift. The MVP has no action to manually continue an interrupted execution.
- Scope includes unattended progression, required checks and automated review phases, branch publication, and stacked PR creation. Final human review and merging remain manual. Do not add scheduled starts, issue editing, a code editor, automatic recovery, or cleanup. Limits for fixing review findings and retrying failed phases are still to be decided.

See [docs/desktop-foundation.md](docs/desktop-foundation.md) for the architecture direction, [docs/implementation-plan.md](docs/implementation-plan.md) for implementation milestones, and [CONTEXT.md](CONTEXT.md) for terminology.
