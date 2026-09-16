# Nightshift implementation plan

## Intent and status

Use Zed's native Rust UI architecture to build a responsive macOS application for the workflow in [MVP_CONCEPT.md](../MVP_CONCEPT.md). Zed is the reference for how the UI is written and how work stays off the UI thread. Nightshift's visual design is independent.

The MVP now includes unattended dependent work and stacked PR creation. The manual execution sequence and milestones below describe the earlier baseline, not the complete current scope. They must be revised after the stack gate, review phases, parent completion semantics, and failure rules are settled. In particular, the earlier rules requiring every dependency to be closed, every branch to start from the default branch, and completion never to start another execution are superseded for authorized stack progression.

The [public issue discovery scaffold](public-issue-discovery.md) now supports adding local projects and fetching public GitHub issues. It does not implement run eligibility or execution.

## Stacked PR design work

For a linear dependency A then B, A's PR targets the default branch and B's PR targets A's branch. B starts after A finishes and builds on A's changes without waiting for A to merge. Detailed completion criteria and review phases are deferred; do not add a specific review or PR-publication gate to successor startup until it is agreed. Preserve the distinction between GitHub issue resolution and permission to build on an unmerged predecessor. Human review and merge remain outside automation. See GitHub's [stacked PR overview](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs).

Remaining design questions, with detailed completion criteria and review phases deferred for now:

- The checks and automated review phases that permit a successor to start, and evidence tied to the predecessor commit that passed them.
- Refresh cadence for automatic discovery during active runs. Newly labeled open issues across added projects join the workload automatically.
- Whether readiness checks extend beyond the direct parent, and the precise completion and closure rules for parents.
- Ordering eligible children within a parent stack, and handling dependencies on multiple unmerged parent stacks or across repositories.
- Review-fix limits and what happens when checks, review, push, or PR creation fail.
- What happens to descendants when a predecessor changes or merges.

Before each start, re-read the issue's own `ready-for-agent` label and query its GitHub parent relationship. When a parent exists, verify that it also has `ready-for-agent`. A failed parent lookup or unreadable parent cannot establish eligibility. This is a readiness check, separate from explicit dependency evaluation; it does not require the parent to finish. Validate standalone issues, both labels present, either label missing, and inaccessible parents.

Exclude parent issues from execution candidates even when labeled. Fetch and snapshot parent context with the child's start-time description, and supply both to Codex with their roles distinguished. A parent does not get a separate execution or branch.

Evaluate the union of the child's explicit blockers and its parent's explicit blockers. Preserve whether each blocker is direct or inherited so the UI can explain why a child cannot start. If P is blocked by Q and both are parents, all Q children must finish before any P child can start. Sharing a parent does not create an explicit dependency, but children execute sequentially for stacking. Validate direct blockers, inherited blockers, a parent blocker with one unfinished child, and a parent blocker whose children have all finished.

Maintain one sequential stack per parent and atomically reserve both a global execution slot and that parent's active-child slot. Select children in an order that respects explicit dependencies; the tie-break between otherwise eligible children is undecided. Create each subsequent child's worktree from the preceding child's recorded branch commit, and target its PR at the preceding branch. The final branch accumulates the children's changes. Do not run siblings in parallel or add a branch-combination stage for them. Independent parent stacks may progress concurrently. Validate competing starts within one parent and simultaneous starts across different parents.

For a same-repository dependency P on Q, Q's final child branch supplies the accumulated prerequisite changes when P starts before Q merges. Dependencies on multiple unrelated unmerged parent stacks, and dependencies across repositories, still require a design decision.

When an explicit blocker is a parent issue, evaluate all its children instead of relying on the parent's label or closed state alone. Fetch the complete child list; incomplete or inaccessible results cannot establish completion. The distinction between children finished for stacked progression and children completed for GitHub parent closure still needs a decision. Parent closure is permitted only after all children complete; automatic versus manual closure is undecided.

Do not select a stacking tool or assume automatic rebasing yet. Verify the chosen GitHub mechanism during implementation. A configurable global concurrency limit replaces the fixed limit of two. Stop-on-quit behavior and the lack of interrupted-execution continuation remain in force.

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

For each explicit blocker, require GitHub issue state `closed` and state reason `completed`. An open blocker, another closure reason, an absent or unknown reason, or an unreadable blocker prevents the start. Show the blocking issue and reason when available. Do not substitute a local execution outcome for GitHub issue completion or repair dependencies automatically; the owner corrects relationships in GitHub.

Git worktrees isolate ordinary working files, but share repository metadata. Worktree selection is not a sandbox. The runtime policy must preserve the MVP requirement that agent changes stay separate from the owner's checkout.

## Codex integration boundary

Adapt only the process and protocol portions of the sibling T3 Code implementation into Rust. Start with one app-server child per execution so process ownership and failures are easy to associate with a record.

Codex owns its stored conversation threads and agent work. Nightshift owns the issue execution record and the local app-server process it launches. A stored thread can outlive that process; reopening it with `thread/resume` is distinct from keeping an in-progress turn running through an application crash.

The MVP does not expose manual continuation of interrupted executions or automatically resume their Codex threads. On reopening Nightshift, load their saved execution details and output for inspection. Retain thread IDs for correlation without adding a resume action.

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

## Earlier implementation milestones, pending stack revision

1. **Prove the GPUI foundation.** Create the Rust package, pin compatible dependencies, and render a native macOS window with text, focus, actions, and a virtualized list. Replay two synthetic output streams off the UI thread. Measure startup, frames, and memory before adding external integration. Exit when the build is reproducible without sibling checkouts and the UI remains responsive.
2. **Persist projects and execution records.** Add migrations, the storage worker, coordinator, and state rules. Test simultaneous starts up to the configured limit, rejection of additional starts, duplicates across checkouts, failed transactions, and saved-description immutability.
3. **Implement manual issue discovery.** Add repository identification, authenticated GitHub access, pagination, and readiness validation. Verify missing labels, open blockers, blockers closed as completed, blockers closed for other reasons, unknown closure reasons, inaccessible dependencies, partial search failures, and stale-result handling. Confirm a completed local execution cannot satisfy an unresolved GitHub dependency.
4. **Prepare isolated worktrees.** Use temporary Git repositories to verify default-branch fetches, unique branch/path creation, setup failure, and an unchanged original checkout. Record partial failures without automatic cleanup.
5. **Integrate Codex.** Test a fake stdio server for handshake, out-of-order responses, output bursts, terminal statuses, unsupported requests, and abrupt exit. Then validate one owner-started execution using the real installed runtime. Resolve runtime permissions and process shutdown before this step.
6. **Validate the complete workflow.** Run executions across projects up to the configured limit, verify additional and duplicate starts are blocked, and confirm completion never triggers another search or execution. Re-run the defined performance scenarios and document results.

The native foundation now has build and test commands in [AGENTS.md](../AGENTS.md). Milestone 1 remains open until its performance validation is complete. Later milestones still require revision for the current stacked-PR scope.

## Decisions to settle during implementation

- GitHub authentication adapter. Dependency relationships come from GitHub's "blocked by" API and require blockers closed as completed; verify access against the repositories in use.
- A compatible pinned GPUI revision, Rust toolchain, and whether the Tokio bridge is a direct dependency or a small local adapter.
- Codex sandbox and approval policy, unsupported input requests, crash process ownership, and the mechanism for enforcing stop-on-quit.
- Exact output queue/cache limits and timeout values, based on the replay measurements.

These decisions do not block the first UI foundation milestone. They do block claiming a complete execution implementation.

## Source references

Local source inspected on 2026-09-06:

- Zed commit `5a9b9558db01a6b906cec2fb70a797affdc58cdd`: [GPUI README](../../zed/crates/gpui/README.md), [application example](../../zed/crates/gpui/examples/hello_world.rs), [virtualized list](../../zed/crates/gpui/examples/uniform_list.rs), [platform configuration](../../zed/crates/gpui_platform/Cargo.toml), [Tokio bridge](../../zed/crates/gpui_tokio/src/gpui_tokio.rs), and [UI dependencies](../../zed/crates/ui/Cargo.toml).
- T3 Code commit `beab6886f45bf42906d0bd01aefe5dfe9e66a867`: [session runtime](../../t3code/apps/server/src/provider/Layers/CodexSessionRuntime.ts), [adapter](../../t3code/apps/server/src/provider/Layers/CodexAdapter.ts), and [protocol client](../../t3code/packages/effect-codex-app-server/src/client.ts).

Sibling links are local research references. They are not application build dependencies. Re-check protocol and framework APIs when pinning implementation versions.
