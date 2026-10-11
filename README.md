# Nightshift

A native desktop scaffold for adding local projects and fetching issues from public GitHub repositories. CI is configured to check builds on macOS, Linux, and Windows; release packaging is not yet implemented for distribution.

## Run

Requires Rust through rustup and the native build prerequisites for the selected platform. Rust and GPUI are pinned; Cargo fetches the dependencies without needing sibling checkouts.

```sh
cargo run --locked
```

On macOS, build a local app bundle:

```sh
sh scripts/bundle.sh --release
open target/release/Nightshift.app
```

## Use

1. Click **Add project** and select an existing Git repository with at least one commit and a public `github.com` remote.
2. If its remotes point to different GitHub repositories, choose one.
3. Click **Search issues**. Nightshift fetches open issues across your added projects, excluding pull requests.
4. Select a project in the sidebar to view its results. Click an issue to open it on GitHub.
5. Click **Filter: all open issues** to search only issues labeled `ready-for-agent`. Click again to show all open issues.

Projects persist between launches. Issue results are fetched when you search. Public access needs no sign-in; GitHub's unauthenticated API rate limits apply, and failures appear in the window. Private repositories, Codex execution, worktrees, and pull requests are not implemented.

Use the platform's primary shortcut modifier with O to add a project, R to search, and Q to quit.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Application data uses the platform's standard per-user data directory. Set `NIGHTSHIFT_DATA_DIR` to a specific directory for manual testing. The original synthetic replay is retained as a library test utility and is no longer in the UI.

See [public issue discovery](docs/public-issue-discovery.md), [storage](docs/storage-implementation.md), and the [implementation plan](docs/implementation-plan.md). The local bundle is not signed or configured for distribution.
