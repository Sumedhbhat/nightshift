# Working on Nightshift

This repository contains a native Rust/GPUI desktop app for macOS, Linux, and Windows for adding local projects and fetching public GitHub issues, a SQLite persistence layer, and planning documents. Codex execution is not implemented.

- Run the app with `cargo run --locked`.
- Validate with `cargo fmt --check`, `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings`.
- Build for performance checks with `cargo build --locked --release`.
- See [docs/foundation-implementation.md](docs/foundation-implementation.md) for implementation status and replay limits.
- See [docs/public-issue-discovery.md](docs/public-issue-discovery.md) for the current project and issue workflow.
- See [docs/storage-implementation.md](docs/storage-implementation.md) for persistence rules and unfinished integration work. Storage tests use temporary databases.

- Read [MVP_CONCEPT.md](MVP_CONCEPT.md) for the complete current scope.
- Use [CONTEXT.md](CONTEXT.md) for terminology and [docs/desktop-foundation.md](docs/desktop-foundation.md) for implementation references.
- Keep the implementation limited to the workflow documented in `MVP_CONCEPT.md`, including its stated unresolved decisions. Do not infer additional features from reference applications.
- Keep product requirements in `MVP_CONCEPT.md`, terminology in `CONTEXT.md`, and technical decisions in `docs/`.
- Check documentation links and consistency after edits.

## Keyboard interface

- Follow the keyboard workflow in `MVP_CONCEPT.md` for every new UI control. Provide visible focus, keyboard activation, and shortcut hints.
- Keep printable navigation shortcuts inactive inside text inputs. Use standard text editing without Vim modes.
- Keep filtering and result navigation on the same result set. Preserve query text when switching projects.
- Document new search qualifiers and reject unsupported syntax visibly.
