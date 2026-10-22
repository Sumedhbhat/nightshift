# Overnight progress

Durable log for the autonomous MVP run. If a session restarts, read this file and continue from the first unchecked item. Decisions live in [decisions.md](decisions.md); the plan lives in [implementation-plan.md](implementation-plan.md).

## Current state

- Started 2026-10-20 from `origin/main` at `4755408` in worktree `t3-41f14eac`.
- No earlier progress file or partial MVP work existed. The sibling worktree on `sb/complete-nightshift-mvp` was clean at the same commit and is left untouched.
- Branch stack: `nightshift/m1-foundation` (from `4755408`), then one branch per later milestone, each based on the previous one.

## Environment

| Item | Finding |
| --- | --- |
| Host | Linux x86_64, 8 cores, 5 GB RAM, Wayland session |
| Rust | 1.97.1 via rustup, matching `rust-toolchain.toml` |
| C toolchain | None installed. Only `cpp-15` (which ships `cc1`), binutils `as`/`ld`, and glibc headers exist |
| `gh` | 2.102.0, logged in as the active account through the keyring |
| `codex` | codex-cli 0.162.1 |
| CI | `Build` runs fmt, Clippy, tests, and a release build on macOS for every push. `Release` lints and tests on Linux and uploads packages for pull requests |

### Local build workaround

`cargo test` first failed with "linker `cc` not found". No tools were installed. Instead, a scratch shim outside the repository drives the installed `cc1`, `as`, and `ld`:

- `/tmp/ns-toolchain/bin/cc`: a minimal gcc-driver substitute in Python.
- `/tmp/ns-toolchain/include`: minimal compiler headers (`stddef.h`, `stdarg.h`, `limits.h`, `float.h`, `stdatomic.h`, an SSE2 subset).
- `/tmp/ns-toolchain/lib`: symlinks for unversioned `.so` names, plus a stub `libxkbcommon-x11` because that runtime library is absent. The stub aborts on X11 keyboard setup; Wayland is unaffected.
- `source /tmp/ns-toolchain/env.sh` sets `CC`, the Cargo linker, `CFLAGS=-DOPENSSL_NO_SSE2_FOR_TESTING`, `RUST_FONTCONFIG_DLOPEN=1`, and `LD_LIBRARY_PATH`.

Local results are for fast iteration. CI on clean macOS and Linux runners is the authoritative check, and every milestone PR records its CI result.

## Milestones

- [x] 1. Prove and finish the GPUI foundation (branch `nightshift/m1-foundation`)
- [x] 2. Persist projects and execution records (branch `nightshift/m2-storage`)
- [x] 3. Implement manual issue discovery (branch `nightshift/m3-discovery`)
- [x] 4. Prepare isolated worktrees (branch `nightshift/m4-worktrees`)
- [x] 5. Integrate Codex (branch `nightshift/m5-codex`)
- [x] 6. Validate the complete workflow (branch `nightshift/m6-workflow`)

## Log

### 2026-10-20

- Read AGENTS.md, MVP_CONCEPT.md, CONTEXT.md, the implementation plan, and all docs and source.
- Baseline `cargo test --locked` failed: no C linker. Built the scratch shim described above.
- With the shim: `cargo fmt --check` passed, `cargo test --locked` passed (27 tests), and `cargo clippy --locked --all-targets -- -D warnings` passed.
- Added the performance scenario: `src/perf.rs`, `src/ui/perf_view.rs`, `scripts/perf.sh`, and `.github/workflows/perf.yml`. Also added batched output append in storage.
- Fixed a race in the storage lock test. A child process forked by another test briefly inherits the lock descriptor. The test now retries the reopen for up to 1 s.
- The locked desktop session withholds frames, so local GUI runs use `/tmp/ns-headless/run.sh`. It starts GNOME Shell headless on an isolated D-Bus session.
- Local results with the shim: fmt passed, Clippy passed, and tests passed in 10 consecutive runs.
- CI for `4913d3d`: Build passed on `nightshift/m1-foundation` (run 39514581373) and on `perf/m1-foundation` (run 39514584230). Performance passed (run 39514584345).
- Performance: ten-minute runs on macOS CI and Linux headless. Results are in docs/foundation-implementation.md. Milestone 1 is complete; the macOS frame p95 deviation matches the platform baseline.
- Milestone 1 PR: https://github.com/Sumedhbhat/nightshift/pull/12 (base `main`). The `perf/m1-foundation` branch exists only to trigger the macOS performance workflow.

### 2026-10-21

- Milestone 2: added schema 2 (`src/storage/002_workflow.sql`), the typed workflow and step policies in `src/domain`, workflow storage operations, and a coordinator that reads the persisted limit. Startup now runs restart reconciliation. Decisions D3–D6.
- Local results with the shim: Clippy passed, and tests passed (39, including 18 storage tests). Disabling the capacity guard as a mutation check made 3 tests fail.
- Milestone 2 PR: https://github.com/Sumedhbhat/nightshift/pull/13 (base `nightshift/m1-foundation`).
- Milestone 3: added the `gh` adapter (`src/github/cli.rs`), an in-memory fake, candidate and eligibility evaluation (`src/github/eligibility.rs`), push preflight (`src/git`), and ordered start gates (`src/application/start.rs`). Removed `reqwest`, and with it aws-lc and rustls, from the dependency tree. Decisions D9–D11.
- `/tmp` (a 2.6 GB tmpfs shared with other processes) hit its quota during a build. The shim now uses `TMPDIR=~/.cache/ns-tmp`.
- Local results: fmt passed, Clippy passed, and tests passed (52). The live smoke check with `gh` 2.102.0 verified repositories, fetched issues, and evaluated eligibility for `cli/cli#14467`.
- Milestone 3 PR: https://github.com/Sumedhbhat/nightshift/pull/14 (base `nightshift/m2-storage`).
- Milestone 4: added `src/git/worktree.rs` (fetch, unique branch and path, worktree, setup, reuse on retry) and a storage-backed `ExecutionRecorder`. Docs are in docs/worktree-preparation.md. Decision D12.
- Fixed a "text file busy" race in the fake `gh` script test.
- Local results: fmt passed, Clippy passed, and tests passed (58) in 6 consecutive runs.
- Milestone 4 PR: https://github.com/Sumedhbhat/nightshift/pull/15 (base `nightshift/m3-discovery`).
- Milestone 5: added `src/codex/rpc.rs` (JSON-RPC over stdio) and `src/codex/mod.rs` (session, turn, server-request policy, output batching, process stop, prompt). Added `examples/codex_turn.rs`. Decisions D13 and D14.
- Real runtime, codex-cli 0.162.1, signed in: the handshake response was verified, and a one-file turn completed in 7.8 s. The first interrupt test left a `sleep` running: Codex's sandbox uses new sessions, and the leftover was stopped by hand. After adding descendant and sandbox cleanup, the rerun left nothing.
- Local results: fmt passed, Clippy passed, and the Codex tests passed (11).
- Milestone 5 PR: https://github.com/Sumedhbhat/nightshift/pull/16 (base `nightshift/m4-worktrees`).

### 2026-10-21 to 2026-10-22

- Milestone 6 (branch `nightshift/m6-workflow`):
  - Added the run orchestrator (`src/application/run.rs`) and execution and publication tasks (`src/application/execution.rs`).
  - Added the executions view, run bar, owner actions, setup command field, and the shutdown hook in `main`. Decisions D15–D17.
- Found and fixed a race in which a shutdown during a refresh could miss executions that had just entered `working`. Starts are now handed to the actor immediately, and shutdown waits for the in-flight refresh.
- Workflow tests (`src/application/tests.rs`, 5 tests) run across two projects with bare remotes, a fake `gh`, and a scripted Codex. They passed 4 times in a row. The full suite (74 tests) passed with `-j 2 --test-threads=4`. An earlier run with more parallelism was killed with exit 137, as the 5 GB host ran out of memory.
- UI check: the release binary ran in a headless GNOME Shell. Screenshots came from the shell's screenshot interface, called by a script holding an allow-listed bus name on the isolated bus, and keys were injected through Mutter's RemoteDesktop interface. The first chord of each injection session is dropped, so scripts send a throwaway Shift first.
  - A run filled 2 of 2 slots, and both executions pushed to a local bare remote and recorded pull requests.
  - `Ctrl+Q` during Codex work recorded both as interrupted and released the slots after their processes stopped.
- Real `gh` checks, read-only: pull-request lookup by head, the repository permission fields, and a dry-run push preflight to `origin`, which created no ref.
- CI: Build passed on `c3fed10` and `77ca5fc`. Performance passed on `77ca5fc` (run 39658034266).
- Performance rerun on macOS CI and Linux headless, recorded in docs/foundation-implementation.md.
- `/tmp` hit its quota again and a background waiter lost its output. Stale scratch files were removed, and new artifacts go to `~/.cache/ns-tmp`.

## Remaining owner validation

These need the owner's GitHub repository or hardware. Nothing else in scope is blocked.

1. **Real GitHub publication end to end.** No test repository, issue, branch, or pull request was created on GitHub, because those are outward-facing changes the owner should choose. To validate:
   1. In a repository you can push to, create an issue, assign it to yourself, and add the `ready-for-agent` label.
   2. Add the local checkout in Nightshift. Optionally set a setup command with Ctrl/⌘⇧S.
   3. Press Ctrl/⌘⇧R. Watch Ctrl/⌘E until the execution reaches "Pull request published".
   4. Check the branch `nightshift/<issue>-<slug>-<id>` and the pull request on GitHub.
2. **Reference-machine performance.** Run `sh scripts/perf.sh` on the owner's MacBook Air M3 to replace the CI runner numbers with reference-machine numbers.
3. **Native keyboard pass on macOS.** Linux was checked with injected keys in a headless shell. A short manual pass on macOS should cover Command shortcuts, Tab order, and the focus rings on the new controls.
