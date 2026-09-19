# Public issue discovery scaffold

The main screen now implements adding a local project and fetching public GitHub issues. It replaces the synthetic replay screen. The broader execution workflow remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Project selection

The native folder picker accepts directories. Background Git commands discover the checkout root and require an initialized repository with a commit. The root is canonicalized before storage. GitHub HTTPS, SSH, and Git remotes are recognized; other hosts are ignored. Remotes pointing to the same repository are combined, preferring `origin`. Distinct repositories are presented for owner selection.

The GitHub repository endpoint verifies public access and supplies stable repository identity and canonical owner/name before the project is saved. Re-adding a saved checkout selects it. Projects reload from SQLite at startup.

## Issue search

Search issues fetches all added projects. The sidebar filters the displayed results. The default is all open issues, as requested for this scaffold. The optional `ready-for-agent` filter applies to the GitHub request. Browsing all issues does not change the readiness requirements for future execution.

Requests use an anonymous HTTP client, a Nightshift user-agent, and the GitHub REST API. No credentials are loaded and no remote writes occur. Pull requests are excluded using the response's `pull_request` field. Numbered pages follow the API's next-page indication. A failed page preserves earlier results and produces a per-project error; failures do not appear as successful empty searches. The UI permits one search at a time, so older searches cannot overwrite newer ones.

Initial limits are 20 seconds per HTTP request, 8 MiB per response page, and 100 pages per repository. Hitting a limit reports incomplete results. Issue results stay in memory until the next search or app quit; execution records are not created by discovery. Requests and Git commands run outside GPUI's foreground thread. Git commands have a ten-second timeout and terminate when their future is dropped.

This scaffold does not evaluate parents or dependencies and does not claim that displayed issues are eligible for execution. Project setup commands, authentication, execution controls, and PR creation are still absent.

## Planned authenticated replacement

The anonymous HTTP client is historical scaffold behavior, not the target integration. Replace it with a GitHub CLI adapter that requires `gh` to be installed and already authenticated, uses the active account for repository verification and issue discovery, and never falls back to anonymous requests. The adapter must consume structured command output without extracting or storing the CLI's token. GitHub CLI installation and login remain outside Nightshift; the app reports a blocking prerequisite error when either is unavailable. It also validates the required commands and structured fields directly, rather than trusting a version threshold, and asks the owner to update an incompatible CLI. This moves request accounting to the user's authenticated GitHub identity and raises the normal REST primary limit from 60 unauthenticated requests per hour to the authenticated user's applicable limit.

The replacement removes the scaffold's public-only project restriction. A public or private repository is eligible to be added when the active account can access it. Repositories remain explicitly enrolled through the project workflow rather than imported from the account automatically.

## Verification

Tests cover accepted remote formats, invalid hosts and paths, initialized checkout validation, remote deduplication and selection, public metadata, private-repository rejection, paginated results, pull-request exclusion, readiness filtering, and retained results on a failed page. Storage tests also verify that projects reload after reopening.

For isolated native testing, set `NIGHTSHIFT_DATA_DIR` before launching the executable. Test projects then stay separate from normal application data.

The live adapter smoke check verified a local checkout with a `rust-lang/rustlings` remote, saved and reloaded its canonical GitHub identity in a temporary database, and fetched 26 open issues without credentials. Run `cargo run --locked --example discover -- /path/to/repository` to repeat that check. All 21 automated tests, formatting, Clippy, and the release build pass. Native click-through verification was unavailable because the computer-use connector failed to start; the new screen has not been visually verified in that session.

API references: [repository metadata](https://docs.github.com/en/rest/repos/repos#get-a-repository) and [repository issues](https://docs.github.com/en/rest/issues/issues#list-repository-issues).
