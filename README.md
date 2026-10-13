# Nightshift

A native desktop scaffold for macOS, Linux, and Windows for adding local projects and fetching issues from public GitHub repositories.

## Run

Requires Rust through rustup, Git on your `PATH`, and the platform prerequisites below. Rust and GPUI are pinned; Cargo fetches the dependencies without needing sibling checkouts.

| Platform | Prerequisites |
| --- | --- |
| macOS | Xcode's macOS toolchain |
| Linux | Run `sh scripts/install-linux-deps.sh` on Debian-based distributions. Other distributions need the equivalent fontconfig, FreeType, Vulkan, Wayland, XKB, and XCB development packages. |
| Windows | Visual Studio Build Tools with the C++ workload |

```sh
cargo run --locked
```

Or build a distributable archive for the current platform:

```sh
sh scripts/bundle.sh --release        # macOS: target/release/Nightshift.app
sh scripts/bundle-linux.sh            # Linux: target/release/nightshift-linux-<arch>.tar.gz
pwsh scripts/bundle-windows.ps1       # Windows: target/release/nightshift-windows-<arch>.zip
```

## Use

1. Click **Add project** and select an existing Git repository with at least one commit and a public `github.com` remote.
2. If its remotes point to different GitHub repositories, choose one.
3. Click **Search issues**. Nightshift fetches open issues across your added projects, excluding pull requests.
4. Select a project in the sidebar to view its results. Click an issue to open it on GitHub.
5. Click **Filter: all open issues** to search only issues labeled `ready-for-agent`. Click again to show all open issues.

Projects persist between launches. Issue results are fetched when you search. Public access needs no sign-in; GitHub's unauthenticated API rate limits apply, and failures appear in the window. Private repositories, Codex execution, worktrees, and pull requests are not implemented.

Ctrl-O adds a project; Ctrl-R searches; Ctrl-Q quits. On macOS, use Command instead of Ctrl.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Application data lives in the platform's per-user data directory: `~/Library/Application Support/Nightshift` on macOS, `$XDG_DATA_HOME/nightshift` (usually `~/.local/share/nightshift`) on Linux, and `%APPDATA%\Nightshift` on Windows. Set `NIGHTSHIFT_DATA_DIR` to a separate directory for manual testing. The original synthetic replay is retained as a library test utility and is no longer in the UI.

See [public issue discovery](docs/public-issue-discovery.md), [storage](docs/storage-implementation.md), and the [implementation plan](docs/implementation-plan.md). The bundles are not signed, notarized, or packaged as installers.

CI formats once, then lints, tests, builds, and packages on Linux (x86_64 and aarch64), macOS (Apple silicon and Intel), and Windows (x86_64), and uploads each archive as a workflow artifact.
