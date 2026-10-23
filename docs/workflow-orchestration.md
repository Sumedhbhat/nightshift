# Workflow orchestration

This implements milestone 6 of the [implementation plan](implementation-plan.md): the owner-started run, execution tasks, publication, and the executions view. Product rules are in [MVP_CONCEPT.md](../MVP_CONCEPT.md). The building blocks are described in [storage-implementation.md](storage-implementation.md), [public-issue-discovery.md](public-issue-discovery.md), [worktree-preparation.md](worktree-preparation.md), and [codex-integration.md](codex-integration.md).

## The run

`application::run` is an actor on the Tokio runtime. Commands, timer ticks, refresh results, started executions, and finished tasks all arrive through one inbox, so state changes are serialized and refreshes never overlap.

- **Start run** (`Ctrl/⌘⇧R`) probes `gh` and captures the active account as run owner, then refreshes immediately.
- **End run** (`Ctrl/⌘⇧R` again) stops discovery and new starts. Active executions continue.
- **Automatic refresh** runs every 5 minutes while the run is active, and immediately whenever an execution finishes (decision D2).
- **Manual refresh** (`Ctrl/⌘R` in the executions view) runs at once without moving the schedule. A request during a refresh runs once afterwards.
- **Backoff.** Rate limits and transient failures back the schedule off: 1 minute, doubling to at most 30 minutes, reset by a clean refresh (decision D15).

A refresh does the following:

1. Verify the run owner is still the active `gh` account.
2. List candidates in every added project.
3. For each candidate, while capacity remains:
   - Create or resume its record at `checking`.
   - Run every start gate.
   - Enter `working` through the coordinator, which acquires the slot atomically.
4. Report each started execution to the actor at once, so shutdown can always reach it.

Issues that already ran are skipped (decision D4). A project-level gate failure, such as identity, access, read-only, or push preflight, blocks that project for the refresh and is shown in the run bar. When every slot is taken, the remaining candidates are not checked at all.

## An execution

`application::execution::run` performs the slot-holding work:

1. Prepare the worktree, or reuse the recorded one on retry. Stopping during preparation cancels it, which kills a running setup command.
2. If shutdown began meanwhile, end as interrupted without starting Codex.
3. Start one Codex app server, record its PID, and run the turn, resuming the saved thread on retry.
4. Stop the server and every process it started.
5. On a completed turn, commit all changes in the worktree with the owner's Git identity. No changes, a failed turn, or an error becomes a retryable step error at `working`.
6. End the slot attempt. Capacity is released only if every owned process is confirmed stopped.

Publication then runs without a slot:

1. Re-read the issue's open state. A closed issue ends the execution as `publication_skipped`, keeping the branch and worktree.
2. Push the exact commit to `refs/heads/<branch>` on the selected remote, then read the remote ref back as evidence.
3. Look for an existing pull request from that branch and recover it. Otherwise create one against the default branch through `gh api`, in the same repository only.

Any publication failure is a step error with nothing retried automatically. **Recheck publication** (`Ctrl/⌘⇧P`) restarts from the incomplete step, re-verifying the open state first if the branch is not yet published.

**Retry working** (`Ctrl/⌘⇧T`) applies to an interrupted or failed working step:

1. Confirm the previous Codex process and its sandboxed commands are gone.
2. Acquire capacity through the coordinator.
3. Continue with the saved snapshot, worktree, branch, and Codex thread.

## Quitting and restarting

Quitting closes the window. Then `main` asks the orchestrator to shut down before closing storage:

- New starts stop, including candidates of an in-flight refresh.
- Every execution receives a stop. Codex turns are interrupted and their processes stopped, waiting up to 60 seconds for all of them.
- Each execution is recorded as interrupted. Its slot is released only when its processes are confirmed gone.

After a crash, startup marks unfinished attempts interrupted with their slots held. It then stops any orphaned Codex process recorded for them, and releases the slot once nothing remains. Nothing resumes automatically.

## Executions view

`Ctrl/⌘E` shows every execution, newest first, with its step and status.

- **Navigation.** Arrows and `j`/`k` move the selection. Tab reaches every control, and Enter activates it.
- **Details.** The detail pane shows the evidence: run owner, branch, worktree, base commit, Codex thread, local result, pushed ref, pull request, and parent. It also shows the current step error with its attempted transition.
- **Actions.** The owner actions are enabled only when the stored state allows them. Enter opens the pull request.
- **Output.** It is paged from storage, 100 blocks per page with at most 24 pages cached, and follows new output every 500 ms.
- **Run bar.** It shows the run owner, slots in use against the limit, the next refresh, and recent problems. `Ctrl/⌘[` and `Ctrl/⌘]` change the concurrency limit (1–8, decision D5).
- **Setup command.** In a project's issues view, **Setup** beside the project name or `Ctrl/⌘⇧S` opens and focuses the setup command editor. Enter saves and hides it; Esc cancels editing and hides it. The editor stays hidden during normal issue browsing.

## Verification

`src/application/tests.rs` runs the orchestrator against temporary stores, two projects with local bare remotes, a fake `gh`, and a scripted Codex:

- **Capacity across projects.** Three eligible issues across two projects with limit 2: two run while the third is not started. A manual refresh starts nothing more, and a duplicate start from another checkout is rejected. After release, all three are pushed and published, each with Codex's change on a branch from the fetched default branch. Later refreshes start nothing again.
- **No run, no follow-on work.** With the run ended, completing an execution triggers no candidate search and no start.
- **Quitting.** Running executions are interrupted, slots are released after the processes stop, worktrees are kept, and no Codex process remains.
- **Retry and recheck.** **Retry working** continues an interrupted execution as attempt 2. A failed push stays failed until **Recheck publication**, which then publishes.
- **Account switch.** After switching the `gh` account, active work still finishes and publishes, and no new execution starts.

The release binary was also driven in a headless GNOME Shell with injected keys, using a fake `gh` and a scripted Codex against a local bare remote:

- `Ctrl+⇧R` filled both slots.
- Both executions pushed their branches and recorded pull requests.
- `Ctrl+E`, `Ctrl+1`, and arrow keys worked.
- `Ctrl+Q` during Codex work recorded both executions as interrupted, released their slots, kept the worktrees, and left no processes.
