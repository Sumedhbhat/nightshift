# Nightshift implementation plan

## Intent and status

Use Zed's native Rust UI architecture to build a responsive macOS application for the workflow in [MVP_CONCEPT.md](../MVP_CONCEPT.md). Zed is the reference for how the UI is written and how work stays off the UI thread. Nightshift's visual design is independent.

The MVP now includes unattended dependent work and independent PR creation. The manual execution sequence and milestones below describe the earlier baseline, not the complete current scope. They must be revised after the review phases and failure rules are settled. Explicit dependencies require their blockers to close before dependent work starts. Closing a parent removes its child pile from the candidate workload.

Execution start is the remote-state boundary. Start eligibility validates every required GitHub and local gate immediately before reservation and local launch. After launch, no GitHub state change, authentication failure, access loss, missing issue, or rate limit interrupts local execution. Retain local results when remote operations fail.

After local work finishes, publication eligibility checks one workflow condition only: the issue remains open. Do not reapply assignment, readiness, parent, or dependency gates. A closed issue skips branch push and pull-request creation while retaining the local branch, worktree, output, and audit reason. Do not alter a pull request already opened before closure.

If open-state verification or either publication step fails, stop the publication attempt and persist the error. Do not retry through timers, active-run discovery, or later issue refreshes. Preserve the local result and expose an explicit owner-requested publication recheck.

Implement publication recheck as a per-execution action. Reverify open state, stop on closed or unverifiable state, and otherwise resume incomplete publication. Keep ordinary issue refresh free of branch pushes and pull-request creation.

Only explicit owner shutdown or a terminal local process condition can stop active work. Setup failure, Codex exit, owned-process crash, and operating-system or machine shutdown are local terminal conditions; GitHub failures are not.

Use one persisted monotonic workflow for the full lifecycle: `checking` → `working` → `branch_published` → `pull_request_published`. A transition validates its guard, performs only that step's side effect, and commits the new state, evidence, and audit event atomically. Failure records the error without advancing. An explicit owner retry resumes the incomplete transition; it must inspect saved evidence and remote state, never replay a completed transition or skip a prerequisite.

Define a static policy for every workflow step, including `consumes_execution_slot: bool`. The orchestrator, not individual step handlers, owns capacity changes. Before entering a consuming step, atomically reserve capacity with the state transition; if capacity is unavailable, retain the prior completed step and wait. Keep the reservation across adjacent consuming steps. Release it only after leaving the final consuming step and confirming any owned local process has stopped. Checking and publication steps are non-consuming; working consumes one slot. Tests must derive expected reservation behavior from step policies so future steps cannot omit the declaration or bypass the orchestrator.

Keep the workflow graph and step policies compile-time typed for the MVP. Persist current state and evidence, not editable step definitions. Settings may change the concurrency limit but cannot change a step's capacity policy.

Model recoverable failure orthogonally to workflow position. Persist the current step, attempted transition, structured error, evidence already obtained, and whether owner retry is available. Do not move to a generic `failed` workflow state. On explicit retry, rerun only that transition and atomically clear the error when saving success. Reserve terminal states for interruption or unrecoverable local process outcomes.

The existing storage enum (`preparing`, `running`, `interrupted`, `failed`) is foundation state and cannot express this target workflow. Add a forward SQLite migration and update the domain model rather than overloading the existing values. Preserve immutable issue snapshots and the audit history through migration.

The [public issue discovery scaffold](public-issue-discovery.md) now supports adding local projects and fetching public GitHub issues. It does not implement run eligibility or execution.

## Independent execution and dependency rules

For an explicit GitHub dependency A then B, B waits until GitHub reports A closed. The team closes implemented issues after merging their pull requests. Nightshift uses closure as the dependency signal. Each execution fetches the latest default branch, starts its own branch from that commit, and targets its PR at the default branch. Human review and merging remain manual. PR stacking and `gh stack` are outside the MVP.

Remaining design questions, with detailed completion criteria and review phases deferred for now:

- The checks and automated review phases that establish successful local work before publication.
- Whether readiness checks extend beyond the direct parent, and who closes an open parent after its children finish.
- Review-fix limits and what happens when checks, review, push, or PR creation fail.

Before each start, re-read the issue's own `ready-for-agent` label and query its GitHub parent relationship. When a parent exists, verify that it also has `ready-for-agent`. A failed parent lookup or unreadable parent cannot establish eligibility. This is a readiness check, separate from explicit dependency evaluation; it does not require the parent to finish. Validate standalone issues, both labels present, either label missing, and inaccessible parents.

Exclude parent issues from execution candidates even when labeled. Fetch and snapshot parent context with the child's start-time description, and supply both to Codex with their roles distinguished. A parent does not get a separate execution or branch.

Evaluate the union of the child's explicit blockers and its open parent's explicit blockers. Preserve whether each blocker is direct or inherited so the UI can explain why a child cannot start. If P is blocked by Q, P's children wait until Q closes; Q's child states do not alter that dependency gate. Sharing a parent creates neither an explicit dependency nor a serialization requirement. Exclude every unstarted child whose parent is closed.

Apply the active-account assignment gate only to executable children. A parent may remain assigned to a different team member; its state, readiness label, context, and inherited dependencies still participate in child eligibility.

Treat multiple assignees as independent accountable owners. Membership of the active account in the assignee set is sufficient; do not require sole assignment or extra approval. Different owners may produce competing pull requests for the same issue. Issue closure prevents new starts but remains non-terminal for active local work.

Check readiness from current label presence only. Do not fetch label-event history or require the run owner to have applied the label.

Reserve global execution capacity and prevent duplicate active work on the same issue. Eligible siblings may execute concurrently. Do not reserve a parent-wide slot or build on another execution's branch. The current storage schema still enforces a parent-wide slot; remove that constraint in a forward migration and update its tests before claiming this workflow is implemented.

For a same-repository dependency P on Q, do not start P while Q remains open. Reevaluate P after GitHub reports Q closed. Dependencies across repositories follow the same closure gate.

When an explicit blocker is a parent issue, rely on that parent's GitHub state for dependency resolution. A closed parent resolves the dependency regardless of open children, while its own remaining children become ineligible for new execution. An active child execution continues; Nightshift does not close or detach any children. Automatic versus manual closure of an open parent after its children finish remains undecided.

Use ordinary Git branches and independent pull requests. Do not add automatic rebasing or branch-chain management. The concurrency limit is configurable. Stop-on-quit remains in force; interrupted work may continue only through explicit owner retry.

The native foundation and local persistence implementation have started. See [foundation-implementation.md](foundation-implementation.md) for build and replay validation, and [storage-implementation.md](storage-implementation.md) for implemented storage rules and remaining work. The service boundaries, storage design, and performance budgets below remain proposed implementation decisions unless recorded there. Product scope remains in the MVP; domain terms remain in [CONTEXT.md](../CONTEXT.md).

## What to take from Zed

| Zed pattern | Nightshift application |
| --- | --- |
| GPUI application and native platform backend | One macOS application with GPU rendering through Metal |
| Entities and views implementing `Render` | Small views for projects, issue results, and execution details |
| Actions and focus handling | Buttons and keyboard bindings call the same application commands |
| Foreground UI work separated from asynchronous work | Render immediately while Git, GitHub, storage, and Codex work runs elsewhere |
| Virtualized lists | Render visible issue rows and output blocks without constructing the entire history every frame |

The local GPUI README describes entities, views, actions, and its event-loop-integrated executor. Its macOS platform guidance requires the `font-kit` feature for glyph rendering. The [GPUI project site](https://gpui.rs/) identifies it as the Rust UI framework used to build Zed.

Use `gpui` and `gpui_platform` from a compatible pinned revision. Verify standalone dependency resolution before committing to that revision. Do not use wildcard versions or require developers to have the sibling checkout. Record the working Rust toolchain and commit `Cargo.lock` when the application is created.

Write a small Nightshift component layer. Zed's `ui` crate depends on its theme, icons, menu, and other workspace crates, so importing it would introduce application dependencies before Nightshift needs them. The initial app does not need Zed's editor, language services, collaboration, or workspace docking system.

## Runtime and module boundaries

Start with one Cargo package and modules. Split crates only when compilation or ownership boundaries justify it.

```text
src/
  main.rs                 application startup and service lifetime
  ui/                     views, components, focus, actions, styling
  domain/                 project and execution types, start rules
  application/            commands, execution coordinator, UI events
  storage/                SQLite migrations and queries
  github/                 repository identity, issues, dependencies
  git/                    fetch, branch and worktree operations
  codex/                  child process, protocol, event translation
```

UI actions send commands to the application layer. The application layer applies domain rules and calls the adapters. Results return as typed events and small view snapshots. Domain rules have no GPUI or Codex protocol dependencies.

GPUI owns UI entities on its foreground thread. Keep selection, focus, loading indicators, and visible output ranges there. Use one Tokio runtime for asynchronous subprocess and network work. Bridge results into GPUI using its supported async context and entity update APIs. Zed's `gpui_tokio` implementation is a reference for this boundary; whether to depend on it directly is a build-spike decision.

Use a dedicated database worker for synchronous SQLite operations. Do not run queries, wait on processes, parse large protocol messages, or rebuild execution history inside `Render`. The application owns execution tasks, so switching views cannot cancel an execution. Views own and release their UI subscriptions independently.

## Keeping the UI responsive

- Virtualize issue lists and execution output. Page historical output from storage and keep only a bounded working set in UI memory.
- Accumulate output deltas in background work and publish batches, initially at most once every 33 milliseconds. Lifecycle changes and errors should reach the UI promptly.
- Store output as indexed blocks or chunks. Avoid repeatedly appending to and copying one growing transcript string. Bound individual display blocks as well as the number of cached blocks.
- Bound channels by bytes as well as event count. Coalesce repaint notifications, not status transitions. Drain subprocess output independently of rendering and spool output to storage. If storage fails or its backlog reaches the configured bound, report an execution error and stop the affected work rather than grow memory indefinitely.
- Update the affected view snapshots. Avoid reloading all projects or all executions when one output block arrives.
- Start with local state and load history on demand. Do not wait for GitHub or launch Codex before showing the window. Manual searches remain available; automatic discovery runs only while an owner-started run is active.

These are architectural constraints. They are not evidence that Nightshift already matches Zed's performance.

## Storage proposal

Use SQLite through `rusqlite`, with schema migrations, foreign keys, and serialized writes. Store application data under `~/Library/Application Support/Nightshift/`.

| Table | Main fields and purpose |
| --- | --- |
| `projects` | ID, canonical checkout path, GitHub host and repository ID, owner/name, selected remote, optional setup command, creation time |
| `executions` | ID, project ID, stable issue identity and number, title and saved description, status, creation/update times, branch, worktree path, fetched base commit, Codex thread/turn IDs, error details |
| `execution_events` | Execution ID, monotonic sequence, previous/new status, timestamp, reason |
| `execution_output` | Execution ID, sequence, source, timestamp, bounded payload chunk |

Database state is authoritative for executions. UI state is a projection. Save each status change and its audit event in one transaction. Keep timestamps in UTC and use sequences for ordering.

Reserve an execution slot in a short write transaction before any worktree or process side effects. Check the global active count against the configured concurrency limit inside that transaction. A partial unique index on stable issue identity must reject duplicate active executions, including when the same GitHub repository is added through different local checkouts. The active set includes preparation and uncertain process outcomes, not just model generation.

Use one coordinator and an application instance lock for the MVP. Failed commits must prevent further execution side effects. Long network calls and Git operations must never hold a database transaction open.

Store per-project setup configuration with the project. Persist the global concurrency limit as an application setting and expose it in Settings. Its default, supported range, and behavior when changed during a run remain to be settled. A small versioned TOML file may hold machine settings such as the Codex executable path. It must not become another source of execution state.

## Earlier manual workflow baseline

1. **Add a project.** Validate an initialized repository and canonicalize its path. Resolve its GitHub identity from the selected remote. If several remotes point to different repositories, ask the owner to select one. Persist the project only after validation.
2. **Search issues.** On the explicit command, query open issues with `ready-for-agent` for each added repository. Handle pagination and exclude pull requests. Report per-project errors and identify incomplete results. Use a search generation ID so late results cannot overwrite a newer search.
3. **Validate a start.** Re-read the issue and fetch its explicit "blocked by" relationships through GitHub's [issue dependencies API](https://docs.github.com/en/rest/issues/issue-dependencies), handling pagination. Do not parse descriptions or comments for dependencies or infer them from shared parents. Require the issue's own readiness label and resolved dependencies. An inaccessible dependency or failed dependency lookup is not evidence of resolution. Show the reason if eligibility cannot be established. Save the description from this start-time read; GitHub does not provide a transaction spanning remote eligibility and local execution creation.
4. **Reserve and record.** Atomically enforce the configured global concurrency limit and no duplicate issue, then save the description and creation event in a `preparing` execution. Repeated clicks cannot create another active record.
5. **Prepare the worktree.** Discover the GitHub default branch, fetch it from the selected remote, and record the fetched commit. Create a unique branch and dedicated worktree from that commit. Use argument arrays for Git subprocesses. Keep generated paths outside the owner's checkout. Do not switch or reset the owner's branch. Run the optional setup command in the new worktree and capture its output; a setup failure prevents Codex launch.
6. **Run and observe.** Start Codex in the worktree using the saved description. Translate process and protocol events into persisted execution status and output. Finishing only updates this execution.

For each explicit blocker, require GitHub issue state `closed`; the state reason does not affect eligibility. An open or unreadable blocker prevents the start. Show the blocking issue and closure reason when available. Do not substitute a local execution outcome for GitHub closure or repair dependencies automatically; the owner corrects relationships in GitHub.

Git worktrees isolate ordinary working files, but share repository metadata. Worktree selection is not a sandbox. The runtime policy must preserve the MVP requirement that agent changes stay separate from the owner's checkout.

## Codex integration boundary

Adapt only the process and protocol portions of the sibling T3 Code implementation into Rust. Start with one app-server child per execution so process ownership and failures are easy to associate with a record.

Codex owns its stored conversation threads and agent work. Nightshift owns the issue execution record and the local app-server process it launches. A stored thread can outlive that process; reopening it with `thread/resume` is distinct from keeping an in-progress turn running through an application crash.

On reopening Nightshift, load interrupted execution details and output for inspection and expose **Retry working**. Explicit retry continues the same execution using its saved worktree, branch, immutable snapshots, and continuation evidence. Confirm old-process termination and acquire capacity through the orchestrator before launching continuation. Preserve previous attempts and output. If continuation is unavailable, record an error at the working step rather than silently starting over. Validate runtime continuation behavior before implementation; never resume automatically.

The sibling T3 Code runtime confirms this separation: `makeCodexSessionRuntime` spawns a scoped child with a force-kill timeout, watches its exit code, and closes the runtime scope when stopping a session. `openCodexThread` separately starts or resumes a Codex thread using its saved ID. `CodexAdapter` closes its sessions during adapter teardown. Use these as lifecycle implementation references. They do not establish behavior after an uncatchable application crash; test that with the selected Codex runtime before designing additional process supervision.

The official [Codex App Server documentation](https://learn.chatgpt.com/docs/app-server) specifies stdio transport, the `initialize` request followed by `initialized`, then thread and turn creation. It also defines streamed item updates and `turn/completed` terminal statuses. Keep reading protocol output continuously, correlate request IDs, and handle stderr separately. Use the installed runtime's supported schema and record the tested Codex version.

Persist thread and turn IDs for event correlation. A successful `turn/start` response only acknowledges the start. Determine the execution outcome from the matching turn's terminal status; a finished turn does not prove that the issue requirements were satisfied. EOF, malformed messages, and process failure need explicit error handling.

Approval and user-input handling require a decision before a real execution milestone. Do not silently approve server requests or leave them pending forever. The existing MVP has no conversational intervention workflow. The integration spike must establish a compatible runtime policy and a defined response to requests that the MVP cannot handle.

## Status and process lifetime

Proposed normal transitions are `preparing -> running -> completed`, with `failed` for confirmed preparation or execution failure. Use `interrupted` for work stopped on app quit or a lost connection with uncertain child-process state. Track termination confirmation separately from the outcome: an interrupted execution continues to reserve capacity until process termination is established.

Persist the execution's process ownership information before relying on it for supervision. On terminal events, settle outstanding output and close the owned process before freeing the slot. Do not mistake a late event for a new transition or let it affect another execution.

On orderly quit, reject new starts, stop active preparation and Codex work, settle captured output, and persist interrupted outcomes with a quit reason. Confirm termination of owned work before freeing slots. Preserve output, history, branches, and worktrees. The shutdown mechanism and timeout must be verified during integration, including quitting during setup and ensuring preparation cannot launch Codex after shutdown begins.

Crash behavior remains to be settled before execution implementation. The minimum design must avoid orphaned work being mistaken for a free slot after restart. No automatic resume, retry, worktree deletion, or recovery pipeline is part of this plan. Any necessary user-facing behavior must first be recorded in the MVP.

## Performance validation

Use release builds on a named reference Mac. Record hardware, macOS, display refresh rate, dataset, and dependency revisions. Measure Nightshift separately from Codex child processes. The following are initial engineering budgets, subject to the first measured baseline. Two-stream scenarios are measurement baselines, not a product concurrency cap; extend load validation once the supported settings range is settled.

| Measure | Initial target and scenario |
| --- | --- |
| Local startup | Interactive window within 1 second on warm launch; report cold launch separately |
| UI feedback | p95 action-to-visible-feedback below 50 milliseconds while two executions stream |
| Frame time | p95 within 16.7 milliseconds at 60 Hz during scrolling; also report missed frames and p99 stalls |
| Large lists | Responsive navigation through 10,000 issue rows and 100,000 stored output blocks |
| Sustained output | Two replay streams at 100 events/second each for 10 minutes, with payload sizes recorded; UI backlog stays bounded |
| Memory | After caches fill, app memory reaches a plateau rather than growing with total stored output; record RSS and peak allocations |

Use Instruments Time Profiler and Allocations to diagnose stalls and memory growth. Instrument command receipt, persistence, UI event delivery, and rendering. Separate network completion time from immediate UI feedback. Compare equivalent launch and navigation measurements with Zed on the same machine if a relative comparison is useful; do not compare a Nightshift issue list with a Zed indexing workload.

## Implementation milestones, pending orchestration revision

1. **Prove the GPUI foundation.** Create the Rust package, pin compatible dependencies, and render a native macOS window with text, focus, actions, and a virtualized list. Replay two synthetic output streams off the UI thread. Measure startup, frames, and memory before adding external integration. Exit when the build is reproducible without sibling checkouts and the UI remains responsive.
2. **Persist projects and execution records.** Add migrations, the storage worker, coordinator, and state rules. Test simultaneous starts up to the configured limit, rejection of additional starts, duplicates across checkouts, failed transactions, and saved-description immutability.
3. **Implement manual issue discovery.** Add repository identification, authenticated GitHub access, pagination, and readiness validation. List only open issues assigned to the active account as candidates; fetch closed issues only for parent or dependency evaluation. Verify missing labels, open blockers, blockers with each supported closure reason, absent closure reasons, inaccessible dependencies, partial search failures, and stale-result handling. Confirm a completed local execution cannot satisfy an open GitHub dependency.
4. **Prepare isolated worktrees.** Use temporary Git repositories to verify default-branch fetches, unique branch/path creation, setup failure, and an unchanged original checkout. Record partial failures without automatic cleanup.
5. **Integrate Codex.** Test a fake stdio server for handshake, out-of-order responses, output bursts, terminal statuses, unsupported requests, and abrupt exit. Then validate one owner-started execution using the real installed runtime. Resolve runtime permissions and process shutdown before this step.
6. **Validate the complete workflow.** Run executions across projects up to the configured limit, verify additional and duplicate starts are blocked, and confirm completion never triggers another search or execution. Re-run the defined performance scenarios and document results.

The native foundation now has build and test commands in [AGENTS.md](../AGENTS.md). Milestone 1 remains open until its performance validation is complete. Later milestones still require revision for the current orchestration scope.

## Decisions to settle during implementation

- GitHub CLI adapter. Require an installed and already authenticated `gh`, consume structured command output without extracting its token, and provide no anonymous fallback. Report missing installation or authentication as a blocking prerequisite; do not install the CLI or launch its login flow. Probe the required commands and structured fields instead of relying only on a version threshold, and reject incompatible installations with an update instruction. Dependency relationships come from GitHub's "blocked by" API and require blockers to be closed; verify access against the repositories in use.
- Owner-managed Git credentials. Do not run `gh auth setup-git` or change Git credential configuration. Push through the selected remote's existing SSH or HTTPS setup and preserve completed local branches when publication fails.
- Non-destructive push preflight. Verify the selected remote can accept a non-interactive push before reservation. Block new work for that project on failure without consuming a slot or affecting active executions.
- Direct-push publication only. Do not create or discover forks or support cross-repository pull-request heads in the MVP; a read-only selected repository is ineligible for new execution.
- Authenticated repository admission. Allow public and private repositories that the active account can access, while retaining explicit project enrollment and rejecting inaccessible repositories.
- Remote-state boundary enforcement. Prevent new starts when authentication, access, issue state, or eligibility cannot be verified, while allowing active local work to finish and preserving unpublished branches and errors without account switching or silent write retries.
- Run-owner identity pinning. Capture the active GitHub CLI account at run start, recheck it before each new execution, and block new starts after logout or account switching without interrupting active executions.
- A compatible pinned GPUI revision, Rust toolchain, and whether the Tokio bridge is a direct dependency or a small local adapter.
- Codex sandbox and approval policy, unsupported input requests, crash process ownership, and the mechanism for enforcing stop-on-quit.
- Exact output queue/cache limits and timeout values, based on the replay measurements.

These decisions do not block the first UI foundation milestone. They do block claiming a complete execution implementation.

## Source references

Local source inspected on 2026-09-06:

- Zed commit `5a9b9558db01a6b906cec2fb70a797affdc58cdd`: [GPUI README](../../zed/crates/gpui/README.md), [application example](../../zed/crates/gpui/examples/hello_world.rs), [virtualized list](../../zed/crates/gpui/examples/uniform_list.rs), [platform configuration](../../zed/crates/gpui_platform/Cargo.toml), [Tokio bridge](../../zed/crates/gpui_tokio/src/gpui_tokio.rs), and [UI dependencies](../../zed/crates/ui/Cargo.toml).
- T3 Code commit `beab6886f45bf42906d0bd01aefe5dfe9e66a867`: [session runtime](../../t3code/apps/server/src/provider/Layers/CodexSessionRuntime.ts), [adapter](../../t3code/apps/server/src/provider/Layers/CodexAdapter.ts), and [protocol client](../../t3code/packages/effect-codex-app-server/src/client.ts).

Sibling links are local research references. They are not application build dependencies. Re-check protocol and framework APIs when pinning implementation versions.
