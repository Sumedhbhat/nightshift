# Platform services

`src/platform/` owns the subprocess environment and application data paths for macOS and Linux. GPUI's platform implementation continues to own windows, input, and desktop integration. The boundary follows Zed's login-shell environment loading without importing its application infrastructure or mutating Nightshift's process-wide environment.

## Startup and commands

The window opens immediately. A Tokio startup task awaits `platform::initialize()` before opening storage and enabling project or run operations. Initialization captures an immutable environment once per app launch. `platform::command()` applies it to each child, so executable lookup, script interpreters, credential helpers, and descendants all use the same `PATH`.

GitHub CLI, checkout inspection, Git operations, project setup, Codex, and process supervision use this command factory. `NIGHTSHIFT_GH` and `NIGHTSHIFT_CODEX` remain explicit launch-provided executable overrides. Library callers that do not initialize receive the inherited environment plus fallback tool directories. The Codex example initializes explicitly.

## Shell capture

On both platforms, Nightshift invokes `$SHELL` as an interactive login shell in the owner's home directory. When `SHELL` is absent, the macOS backend uses `/bin/zsh` and Linux uses `/bin/sh`. Bash, Zsh, and POSIX shells use `-l -i -c`; Csh/Tcsh use login-style `argv[0]`; Fish also emits `fish_prompt`; Nushell uses `-l -e`.

An absolute `/usr/bin/env -0` captures exported variables between NUL-delimited markers. Startup and logout banners are ignored, and values retain newlines, equals signs, and non-UTF-8 bytes. Valid output is usable even if a logout hook exits unsuccessfully. Environment contents and shell output are never logged.

Capture has closed standard input, a separate session and process group, a five-second timeout, and a 1 MiB output limit. The new session prevents the interactive shell from taking control of a terminal-launched app. Failure stops the shell group, retains the inherited environment, and shows a startup warning. No shell configuration is edited, and no credentials are installed or repaired.

Shell exports augment the child environment. `NIGHTSHIFT_*` settings, `HOME`, `XDG_DATA_HOME`, and shell bookkeeping variables retain their inherited values, so profile side effects do not redirect application storage or override a selected executable.

## Platform backends

`macos.rs` and `linux.rs` define default shells, fallback tool directories, and application data paths. Search keeps the chosen environment's directory order, then appends missing fallbacks:

- Both platforms include `~/.local/bin`, `~/.cargo/bin`, and `~/.bun/bin`, plus standard system directories.
- macOS includes Apple silicon and Intel Homebrew directories.
- Linux includes Snap and the standard Linuxbrew prefix.

Data lives under `~/Library/Application Support/Nightshift` on macOS and `$XDG_DATA_HOME/nightshift` on Linux, falling back to `~/.local/share/nightshift` when XDG data home is absent or not absolute. `NIGHTSHIFT_DATA_DIR` takes precedence on both platforms.

This does not add Windows support. A new supported OS needs its own backend and corresponding process-supervision implementation. Environment changes take effect after reopening Nightshift; capture does not run per project or per command.

## Verification

Platform tests reproduce a desktop `PATH` with tools installed elsewhere, including scripts whose runtime is also outside that `PATH`. They cover actual Bash and Zsh profiles when installed, startup noise, malformed and oversized output, timeout cleanup, fallback discovery, executable overrides, and data-directory isolation. Run them with `cargo test --locked platform::`.

See [desktop foundation](desktop-foundation.md), [GitHub CLI integration](public-issue-discovery.md#github-cli-adapter), and decision D18 in [decisions.md](decisions.md).
