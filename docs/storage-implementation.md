# Local persistence implementation

This implements the storage and reservation portion of milestone 2 in the [implementation plan](implementation-plan.md). The UI now supports [public issue discovery](public-issue-discovery.md). Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Ownership and startup

`Store` opens `nightshift.sqlite3` on a dedicated named thread. Startup resolves the per-user application data directory through the platform abstraction (or the exact directory supplied by `NIGHTSHIFT_DATA_DIR`), acquires an OS file lock on `nightshift.lock`, and applies the schema migration. A second worker for the same directory returns an explicit error. File existence alone is not the lock; the OS releases ownership when the file handle closes or the process exits.

The window opens without awaiting SQLite. A one-shot result updates the footer with history counts or an error. The application owns initialization and waits for it during shutdown, including an immediate quit. It then drains earlier storage commands and closes the database before shutting down Tokio. Synthetic replay data never enters persistent history.

The worker serializes requests through a 32-message channel. SQLite uses foreign keys, `synchronous=FULL`, and a two-second busy timeout. Migration 1 runs in a transaction; a newer schema is rejected. `rusqlite` 0.40.2 bundles SQLite, with transitive versions in `Cargo.lock`.

## Reservation and state rules

The coordinator requires an explicit nonzero concurrency limit. It has no default, Settings UI, or live reconfiguration policy yet. Reservation is only a local capacity operation. Future run orchestration must establish owner authorization, readiness, dependency eligibility, and default-branch selection before calling it.

Each reservation uses `BEGIN IMMEDIATE` to check capacity and insert the execution plus its first audit event. Partial unique indexes protect the issue and parent slots across local checkouts, using GitHub host plus stable issue identity. The current implementation limits siblings to a single parent slot. That restriction is obsolete under the independent-PR plan: a forward migration must remove it while preserving global capacity and duplicate-issue protection. Until then, storage does not match the target concurrency behavior. Transactional checks also hold when tested through separate SQLite connections.

Saved child and parent descriptions, titles, identities, and creation times are immutable. A database trigger rejects attempts to update them. Each state change and its audit event commit together, with UTC timestamps and per-execution sequences.

Supported changes currently cover preparation, running, interruption, confirmed failure, and termination confirmation. Interrupted executions retain capacity until owned work is confirmed stopped. They cannot transition back to running. Failure releases capacity only when the caller confirms failure and termination together. Late state changes and output after termination confirmation are rejected.

The planned **Retry working** action supersedes the current prohibition on interrupted continuation. The future migration must preserve attempt history, confirm old-process termination, and reserve capacity before a continuation attempt. Completed attempts remain immutable; reopening the app does not resume work automatically.

There is intentionally no successful-completion operation yet. Passing checks, automated review, successor eligibility, and PR publication still need the decisions listed in the MVP. Free capacity alone does not make another issue eligible.

The target workflow is now an ordered persisted state machine: checking, working, branch published, and pull request published. The current schema does not implement those states. A later forward migration must store each transition's evidence and error atomically with its audit event, leave failures at the incomplete step, and let an explicit owner retry resume only that step.

Each target workflow step has explicit orchestration metadata declaring whether it consumes an execution slot. The orchestrator acquires capacity atomically when entering a consuming step, carries it across adjacent consuming steps, and releases it only after safely leaving the last one. Checking and publication do not consume capacity; working does. Future steps must declare the policy rather than modifying reservation logic directly.

Recoverable errors attach to the current workflow step with the attempted transition and retained evidence. They do not advance to a generic failure state. An explicit owner retry resumes that transition; success clears the error atomically with the next state and audit event. Terminal local outcomes remain separate.

## Output and limits

Output blocks retain a source, payload, UTC timestamp, and monotonic sequence. Blocks are limited to 64 KiB by UTF-8 byte length; oversized input returns an error rather than truncating it. Output and audit reads use cursors and pages of at most 100 records. A child or parent snapshot is limited to 1 MiB before queueing. These initial engineering bounds are not measured production throughput limits.

Output remains appendable during interruption while buffers drain, but not after termination is confirmed. The future process adapter must finish persistence before reporting termination to storage. No subprocess output adapter or storage-failure stop policy is wired yet.

## Validation and remaining work

Thirteen storage tests use temporary databases. They cover reopening with saved snapshots and output, global capacity across projects, duplicates across checkouts, competing siblings, independent parent slots, interrupted capacity, invalid late transitions, immutable snapshots, bounded output paging, instance locking, newer schemas, and rollback on both audit insertion and commit failure.

Native release inspection confirmed the saved-history footer updates after asynchronous initialization. Quitting with replay active exited successfully, released the database lock, and left SQLite's integrity check passing with zero synthetic executions saved. One earlier native inspection remained on the loading message; a fresh launch with handoff logging completed successfully, without reproducing that stall. Repeated cold-launch validation remains open.

Remaining milestone 2 work includes persisted Settings after its policy is settled, application run orchestration, process ownership records, branch/worktree metadata, and history inspection views. Startup currently preserves reserved records; it does not reconcile crashed processes, free uncertain slots, or resume work. Real execution must wait for process-lifetime decisions and integration tests.

The [public issue discovery scaffold](public-issue-discovery.md) now verifies local projects and GitHub identity before calling project persistence. Storage itself remains separate from Git and network validation.

Implementation references: [rusqlite](https://docs.rs/rusqlite/0.40.2/rusqlite/), [SQLite transactions](https://www.sqlite.org/lang_transaction.html), and [Rust file locking](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
