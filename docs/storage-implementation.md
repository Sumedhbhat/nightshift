# Local persistence implementation

This implements the storage and reservation portion of milestone 2 in the [implementation plan](implementation-plan.md). The UI now supports [public issue discovery](public-issue-discovery.md). Product scope remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Ownership and startup

`Store` opens `nightshift.sqlite3` on a dedicated named thread. Startup creates the application data directory under `~/Library/Application Support/Nightshift` on macOS or `$XDG_DATA_HOME/nightshift` (default `~/.local/share/nightshift`) on Linux, acquires an OS file lock on `nightshift.lock`, and applies the schema migration. A second worker for the same directory returns an explicit error. File existence alone is not the lock; the OS releases ownership when the file handle closes or the process exits.

The window opens without awaiting SQLite. A one-shot result updates the footer with history counts or an error. The application owns initialization and waits for it during shutdown, including an immediate quit. It then drains earlier storage commands and closes the database before shutting down Tokio. Synthetic replay data never enters persistent history.

The worker serializes requests through a 32-message channel. SQLite uses foreign keys, `synchronous=FULL`, and a two-second busy timeout. Migrations 1 and 2 run in one transaction; a newer schema is rejected. `rusqlite` 0.40.2 bundles SQLite, with transitive versions in `Cargo.lock`.

## Workflow and capacity rules

Schema 2 (`002_workflow.sql`) implements the typed workflow from the [implementation plan](implementation-plan.md). It rebuilds the execution tables in one migration transaction. Schema 1 rows become legacy history: unconfirmed work becomes an interrupted record that keeps its slot, and confirmed work ends with a `legacy` outcome. Their events and output are preserved.

- **One record per issue.** An execution is created at `checking` from the issue's stable identity, the GitHub host, and the run owner. A unique index allows one non-legacy record per issue across all checkouts. Once a record reaches `working`, the issue is never started again (decision D4 in [decisions.md](decisions.md)). The parent-wide slot is gone; siblings run independently.
- **Steps and statuses.** Steps are `checking`, `working`, `branch_published`, and `pull_request_published`. Triggers keep them monotonic. Statuses are `pending`, `running`, `error`, `interrupted`, and `ended`; `ended` is final and carries an outcome. Recoverable failures are step errors with the attempted transition, message, detail, and retry availability. Success clears the error in the same transaction (decision D6).
- **Step policies and slots.** `Step::policy()` in `src/domain/mod.rs` declares whether each step consumes a slot; only `working` does. Entering a consuming step opens a *slot attempt* in the same immediate transaction that checks capacity, saves the immutable snapshots, and writes the audit event. Without capacity, the execution stays at `checking`, pending. The count query is built from the policies, so a new consuming step needs no reservation code (decision D3).
- **Releasing capacity.** An attempt ends as finished, failed, or interrupted. It releases its slot only when the owned process is confirmed stopped, either at the end or later through `confirm_stopped`. **Retry working** requires a released previous attempt and acquires capacity again, preserving the snapshots and attempt history.
- **Evidence.** Default branch, base commit, branch, worktree path, and Codex thread are written once; a conflicting value is rejected. Publication saves the pushed remote, ref, and commit, then the pull request's number, URL, and ID, each atomically with its step change.
- **Publication.** It starts only after local work finished and the slot is released. A closed issue ends the execution as `publication_skipped`. A failed push or pull-request creation is a step error. Only `begin_publication`, the explicit **Recheck publication**, can continue it.
- **Restart.** `reconcile_after_restart` runs before the window loads projects. Running checks return to pending. Unfinished working attempts become interrupted and keep their slots until the owned process is confirmed stopped. Interrupted publication becomes a retryable error. Nothing resumes automatically.
- **Settings.** The concurrency limit lives in `settings`: default 2, range 1–8 (decision D5). The coordinator reads it in the same worker call that acquires capacity.

Snapshots, identity, and the run owner are immutable through SQL as well, enforced by triggers.

## Output and limits

Output blocks retain a source, payload, UTC timestamp, and monotonic sequence. Blocks are limited to 64 KiB by UTF-8 byte length; oversized input returns an error rather than truncating it. Output and audit reads use cursors and pages of at most 100 records. A child or parent snapshot is limited to 1 MiB before queueing. These initial engineering bounds are not measured production throughput limits.

Process output (`setup`, `agent`, `stderr`) needs an unreleased slot attempt, so late output after a confirmed stop is rejected. Nightshift's own `system` and `git` messages may be added until the execution ends. Batches of up to 4,096 blocks commit in one transaction. The process adapter must finish persisting output before confirming the stop.

## Validation and remaining work

Eighteen storage tests use temporary databases. They cover:

- simultaneous starts filling the limit across projects, with the rest left pending and without snapshots;
- two independent connections that cannot overbook;
- duplicates across checkouts and after completion, and independent siblings;
- failed checks staying at `checking`, and policy-derived slot accounting through every step;
- interrupted attempts holding capacity until the stop is confirmed, and retry re-acquiring it;
- immutable evidence, snapshots, identity, and step order;
- publication skip and explicit recheck, and restart reconciliation;
- rollback when the audit insert fails and when the commit fails;
- schema 1 migration, settings bounds, and batched, bounded, paged output;
- the instance lock and newer-schema rejection.

Run orchestration, process supervision, and the executions view use these operations through the coordinator; see [workflow-orchestration.md](workflow-orchestration.md).

Implementation references: [rusqlite](https://docs.rs/rusqlite/0.40.2/rusqlite/), [SQLite transactions](https://www.sqlite.org/lang_transaction.html), and [Rust file locking](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
