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

- [ ] 1. Prove and finish the GPUI foundation
- [ ] 2. Persist projects and execution records
- [ ] 3. Implement manual issue discovery
- [ ] 4. Prepare isolated worktrees
- [ ] 5. Integrate Codex
- [ ] 6. Validate the complete workflow

## Log

### 2026-10-20

- Read AGENTS.md, MVP_CONCEPT.md, CONTEXT.md, the implementation plan, and all docs and source.
- Baseline `cargo test --locked` failed: no C linker. Built the scratch shim described above.
- With the shim: `cargo fmt --check` passed, `cargo test --locked` passed (27 tests), and `cargo clippy --locked --all-targets -- -D warnings` passed.
