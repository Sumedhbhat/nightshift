# Implementation decisions

Routine choices made during the autonomous MVP run, with the reason and tradeoff. Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md); the plan is in [implementation-plan.md](implementation-plan.md).

## D1. Validation channels

**Decision.** Use CI as the authoritative record for formatting, Clippy, tests, and release builds. Use the local scratch toolchain described in [overnight-progress.md](overnight-progress.md) for iteration and for running the Linux app.

**Reason.** The host has no C compiler, and the run may not install tools. CI runs on clean macOS and Linux runners, which also proves the build needs no sibling checkout.

**Tradeoff.** CI round trips take about ten minutes. Local results come from a nonstandard toolchain and are labelled as such.

## D2. Completion and follow-on work

**Decision.** Completing an execution never starts another search or execution by itself. Outside an active run, completion triggers nothing. During an owner-started run, a finish frees capacity, and the run's scheduler performs the refresh the MVP requires. Every candidate from that refresh passes the full start-eligibility check. Completion never causes publication of other executions.

**Reason.** Milestone 6 asks that completion trigger no extra search or execution, while [MVP_CONCEPT.md](../MVP_CONCEPT.md) requires a refresh when capacity frees during an active run. Treating the run, not completion, as the trigger satisfies both.

**Tradeoff.** Tests must cover both cases: no activity without a run, and gated refresh with one.

## D3. What occupies an execution slot

**Decision.** A slot is held by a *slot attempt*: a persisted attempt row created atomically when the orchestrator enters a step whose policy consumes a slot, or retries such a step. The attempt releases its slot only after the attempt ends and the owned process is confirmed stopped. Only `working` consumes a slot. An execution that stays at `working` after its attempt ended, because local work finished and publication is pending, failed, or awaits an owner retry, does not hold a slot.

**Reason.** [CONTEXT.md](../CONTEXT.md) says an interrupted execution occupies a slot *until its work is confirmed stopped*, and the MVP requires **Retry working** to *acquire* capacity before resuming. Both only make sense if a slot follows live owned work, not the step label. Holding slots for failed pushes would also let remote failures starve all capacity until the owner intervened.

**Tradeoff.** Capacity accounting needs a second table instead of a count of rows at `working`. The step policy still decides which steps create slot attempts, so a future slot-consuming step needs no new reservation code.

## D4. One execution record per issue

**Decision.** Each GitHub issue gets at most one execution record per local database. The record is created at `checking` and carries the whole lifecycle. Ineligible candidates stay at `checking` with an error that the active run re-evaluates on each refresh. Once an issue's execution has reached `working`, the run never starts it again; further work on it uses the explicit owner actions on that same record.

**Reason.** An issue stays open and labeled after its pull request is published, until the team merges and closes it. Without this rule, every refresh would start a duplicate execution and pull request. The MVP's "prevent duplicate active executions" alone does not cover that case.

**Tradeoff.** Re-running an issue from scratch needs a future owner action; the MVP has none. Records migrated from schema 1 are marked legacy and excluded from the uniqueness rule.

## D5. Concurrency limit setting

**Decision.** Persist the limit in a `settings` table, defaulting to 2, with a supported range of 1 to 8. A change applies to the next slot acquisition. Lowering it never stops active work; new starts wait until active slot attempts drop below the new limit.

**Reason.** The MVP leaves the default and range open. Two matches the measured two-stream baseline. Eight keeps output and process load within what the replay measurements cover on the reference machines.

**Tradeoff.** Owners with more capacity cannot exceed eight until the range is revisited with new measurements.

## D6. Step errors and terminal outcomes

**Decision.** Each execution has a step and a status: `pending`, `running`, `error`, `interrupted`, or `ended`. Errors record the attempted transition, a message, diagnostic detail, and whether an owner retry is available. Success clears the error in the same transaction that saves the new evidence and audit event. `ended` is terminal and carries an outcome: `pull_request_published` or `publication_skipped` (the issue was closed after local work).

**Reason.** The MVP stores recoverable failure on the current step instead of a generic failed step, and reserves terminal states for the end of the workflow.

**Tradeoff.** Setup failure and a failed Codex turn are step errors at `working` that **Retry working** can continue. They are not terminal, so the execution remains visible until the owner acts.

## D7. Output queue, cache, and timeout limits

**Decision.** Keep the measured replay bounds as the initial production limits:

| Limit | Value |
| --- | --- |
| UI delivery cadence | at most every 33 ms |
| Messages drained per UI update | 256 |
| Bounded channel from producers | 256 blocks |
| Live output blocks cached in a view | 2,048 |
| Stored output page | 100 blocks; 24 pages cached per view |
| Output block | 64 KiB; storage batches of up to 4,096 blocks |
| `gh` command timeout | 30 s |
| Git inspection command timeout | 10 s |

Process output is spooled to storage in batches. If storage fails or its backlog fills, the execution records an error and stops instead of growing memory.

**Reason.** In ten-minute runs at 2 × 100 events/second, the largest drain was 40 events per update. RSS plateaued, and stored pages loaded with a p95 under 16 ms. See [foundation-implementation.md](foundation-implementation.md).

**Tradeoff.** Bursts above roughly 7,000 events/second would apply backpressure to producers. Real Codex output is far lower, and storage, not the UI, is the overflow boundary.

## D8. GPUI revision, toolchain, and Tokio bridge

**Decision.** Keep Zed revision `5a9b9558db01a6b906cec2fb70a797affdc58cdd` for `gpui` and `gpui_platform`, and Rust 1.97.1. Keep the small local Tokio adapter instead of depending on `gpui_tokio`: Tokio tasks send results over channels, and GPUI foreground tasks await them or poll on a 33 ms timer.

**Reason.** Clean CI runners on macOS and Linux build from the pinned Git dependencies with no sibling checkout. The adapter is a few lines, and `gpui_tokio` would pull in Zed's application infrastructure.

**Tradeoff.** Updating GPUI requires re-running the performance scenario and the keyboard checks.

## D9. GitHub CLI adapter shape

**Decision.** Use `gh api` with REST paths for every read and write, so all output is JSON. Probe capabilities by checking `gh api --help` for the flags used and by parsing `gh auth status --json hosts`. Inject a `Runner` trait so tests use an in-memory fake. Never pass `--show-token`.

**Reason.** One command family keeps the probe small and exact. `gh issue` and `gh pr` subcommands print human-oriented output for writes. `gh pr create`, for example, prints only a URL, while `gh api` returns the pull request's number and node ID.

**Tradeoff.** Nightshift builds REST paths itself and must track GitHub REST changes rather than `gh` subcommand changes.

## D10. Inaccessible dependencies and parents

**Decision.** Compare the issue's `issue_dependencies_summary.total_blocked_by` with the blockers the account can list. Any shortfall means a blocker the account cannot see, and the start is blocked. Treat an issue with sub-issues as a parent, and exclude it from execution even when labeled. Read the parent through `parent_issue_url`, which may point to another repository.

**Reason.** The "blocked by" endpoint omits issues the account cannot read. Without the count check, a hidden open blocker would look resolved. Single-issue reads include both summaries; list endpoints return them as null, so candidate lists are re-checked one issue at a time before any start.

**Tradeoff.** One extra read per candidate and per parent at start time.

## D11. Non-interactive push preflight

**Decision.** Run `git push --dry-run --porcelain --no-verify` to a probe ref on the selected remote. Set `GIT_TERMINAL_PROMPT=0`, `GCM_INTERACTIVE=never`, and `SSH_ASKPASS_REQUIRE=never`, with standard input closed. When the owner has not set `GIT_SSH_COMMAND` or `GIT_SSH`, use `ssh -o BatchMode=yes`. Never change Git configuration.

**Reason.** A dry run authenticates against the remote's receive service, so it exposes missing write access or credentials that would need a prompt. It sends no objects and creates no refs.

**Tradeoff.** An owner-configured `core.sshCommand` without batch mode could still prompt on a terminal-attached launch. Overriding it would change owner-managed credential behavior, so Nightshift leaves it alone.

## D12. Worktree location, naming, and setup

**Decision.** Create worktrees under the application data directory in `worktrees/<owner>-<repo>/`. Name branches `nightshift/<issue>-<slug>-<execution-id>`. Run setup with `/bin/sh -c` and a 30-minute timeout. Fetch only the default branch into its remote-tracking ref.

**Reason.** The data directory is outside every checkout and already owned by Nightshift. The execution ID makes names unique across retries, duplicate checkouts, and other accountable owners. A shell command matches how owners write setup steps such as `npm ci && make`.

**Tradeoff.** Worktrees are not next to the project, so editors must open them from the path in the execution details. Updating `refs/remotes/<remote>/<default>` is visible in the owner's repository, which is what any `git fetch` would do.

## D13. Codex sandbox, approvals, and commits

**Decision.** Start threads with `sandbox: workspace-write` and `approvalPolicy: never`, with the worktree as the working directory. Decline approval requests, decline MCP elicitations, and answer every other server request with JSON-RPC error `-32601`. Log each one in the execution output. Codex does not commit; after a completed turn, Nightshift commits the worktree's changes during publication.

**Reason.** The MVP has no conversational intervention, so no request may wait for an answer. `workspace-write` keeps writes inside the worktree. A Git worktree's metadata lives in the owner's repository, outside that sandbox, so Codex could not commit there anyway. Committing outside the sandbox keeps the sandbox tight.

**Tradeoff.** Work that needs network access or writes outside the worktree fails inside the turn, and Codex reports it as a failed command. Owners can do such preparation in the project's setup command, which runs outside the sandbox.

## D14. Process ownership, stop-on-quit, and crashes

**Decision.** One app server per execution, in its own process group, with its PID recorded on the slot attempt. Stopping interrupts the turn and closes stdin. It then signals the group, and finally stops every descendant captured beforehand plus any Codex sandbox process naming the worktree. The slot is released only when all are confirmed gone. After a crash, restart marks the attempt interrupted, keeping its slot. Confirming the stop checks that the recorded PID is still a Codex app server before signaling it, and sweeps sandbox processes for the worktree.

**Reason.** In a real interrupted turn, Codex's sandboxed command outlived the app server, so killing the process group alone would leave owned work running while freeing its slot.

**Tradeoff.** Finding processes by command line is best effort after a crash. A sandboxed command whose wrapper has already exited and that does not mention the worktree cannot be found. The app server itself exits when its standard input closes, which happens when Nightshift dies.
