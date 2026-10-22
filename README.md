# Nightshift

A native macOS and Linux app for adding local projects and fetching their GitHub issues through the GitHub CLI.

## Install a release

Stable releases come from `release/X.Y` branches. Nightly builds come from every push to `main`, for anyone who wants changes sooner. Both are on the [releases page](https://github.com/Sumedhbhat/nightshift/releases).

Download the package for your machine from a release:

- macOS on Apple silicon: `nightshift-macos-aarch64.dmg`. Intel Macs: `nightshift-macos-x86_64.dmg`. Open it and drag Nightshift into Applications.
- Debian or Ubuntu: `nightshift-linux-<arch>.deb`. Install it with `sudo apt install ./nightshift-linux-x86_64.deb`.

The macOS app isn't notarized yet, so the first launch needs one extra step. See [releases](docs/releases.md#macos). The installed version appears next to the app name and from `nightshift --version`.

Linux builds need glibc 2.35 or newer, Git, and a Wayland or X11 session with Vulkan drivers. See [releases](docs/releases.md) for how releases are cut and patched.

## Run from source

Requires Rust through rustup. On macOS, also install Xcode's macOS toolchain. On Debian-based Linux, run `sh scripts/install-linux-deps.sh`. Rust and GPUI are pinned; Cargo fetches the dependencies without needing sibling checkouts.

```sh
cargo run --locked
```

Or build a local app bundle:

```sh
sh scripts/bundle.sh --release        # macOS: target/release/Nightshift.app
sh scripts/bundle-dmg.sh              # macOS: target/release/nightshift-macos-<arch>.dmg
sh scripts/bundle-deb.sh              # Linux: target/release/nightshift-linux-<arch>.deb
```

## Use

Prerequisites: the GitHub CLI signed in with `gh auth login`, the Codex CLI signed in, and Git push access through your remote's existing SSH or HTTPS setup. Nightshift never signs in or changes credentials by itself.

1. Press Ctrl/⌘O and select a Git repository with at least one commit and a `github.com` remote your GitHub CLI account can access. Choose a remote if several point to different repositories.
2. Select the project (Ctrl/⌘1–9). Optionally enter a setup command, such as `npm ci`, with Ctrl/⌘⇧S and Enter; it runs in each new worktree before Codex starts.
3. Browse issues: press `/` to search, then Enter or Ctrl/⌘R to fetch. Search supports `label:`, `-label:`, `repo:`, and quoted phrases.
4. Press Ctrl/⌘⇧R to start a run. Nightshift works through open issues assigned to you and labeled `ready-for-agent`, up to the concurrency limit (Ctrl/⌘[ and Ctrl/⌘]). Each execution works in its own worktree and branch, and publishes a pull request against the default branch.
5. Press Ctrl/⌘E to see executions, their evidence, errors, and output. Use Ctrl/⌘⇧T to retry working after an interruption or failure, and Ctrl/⌘⇧P to recheck publication.

Quitting stops active executions and records them as interrupted. Their worktrees and branches are kept. Review and merge pull requests yourself.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

Application data lives in `~/Library/Application Support/Nightshift` on macOS and `$XDG_DATA_HOME/nightshift` (default `~/.local/share/nightshift`) on Linux. Set `NIGHTSHIFT_DATA_DIR` to a separate directory for manual testing. The original synthetic replay is retained as a library test utility and is no longer in the UI.

See [public issue discovery](docs/public-issue-discovery.md), [storage](docs/storage-implementation.md), and the [implementation plan](docs/implementation-plan.md). See [releases](docs/releases.md) for the release workflow.

## License

Nightshift is available under the [MIT License](LICENSE).
