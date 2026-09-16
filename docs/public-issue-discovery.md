# Public issue discovery scaffold

The main screen now implements adding a local project and fetching public GitHub issues. It replaces the synthetic replay screen. The broader execution workflow remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Project selection

The native folder picker accepts directories. Background Git commands discover the checkout root and require an initialized repository with a commit. The root is canonicalized before storage. GitHub HTTPS, SSH, and Git remotes are recognized; other hosts are ignored. Remotes pointing to the same repository are combined, preferring `origin`. Distinct repositories are presented for owner selection.

The GitHub repository endpoint verifies public access and supplies stable repository identity and canonical owner/name before the project is saved. Re-adding a saved checkout selects it. Projects reload from SQLite at startup.

## Keyboard controls

- `/` or ⌘F focuses the search field. Search has ordinary text editing, selection, copy, cut, and paste, with no Vim modes. Typing `/`, `j`, or `k` inside it inserts text.
- Enter in search or ⌘R refreshes the selected project's first ten GitHub results. **Load more** fetches that project's next page. Esc or Down from search focuses results without clearing the query.
- Up/Down or `j`/`k` browse matching results and scroll the selection into view. Enter opens the selected issue on GitHub.
- ⌘1–⌘9 select the first nine projects in sidebar order, preserving the search query. Each row shows its shortcut suffix. ⌘0 shows All projects. An unassigned number does nothing.
- ⌘O adds a project. Tab and Shift-Tab move focus. Enter activates a focused control; Space also activates project, remote, and issue rows. The folder picker uses standard macOS keyboard controls.

## Issue search

The search field filters fetched open issues as the owner types. Refresh is scoped to the selected project and replaces only that project's cached rows. **Load more** appends and deduplicates the next page. The **All projects** view combines project pages already fetched during the current session and does not issue network requests. Changing or removing filters does not require another request. Browsing all issues does not change the readiness requirements for future execution.

Supported query syntax:

| Input | Meaning |
| --- | --- |
| `keyboard search` | Title contains both terms, case-insensitively |
| `"keyboard search"` | Title contains this phrase |
| `label:ready-for-agent` | Exact label match, case-insensitively |
| `label:"help wanted"` | Label containing spaces |
| `label:bug label:ready-for-agent` | Both labels required |
| `label:bug,enhancement` | Either label matches |
| `-label:bug` | Exclude this label |
| `repo:owner/name` | Match a fetched repository |
| `no:label` | Issues without labels |
| `is:open is:issue in:title` | Explicitly state the view's existing scope |

A leading `-` also excludes title terms or repositories. `state:open` is accepted. Empty search matches all fetched open issues in the selected project. Unknown qualifiers, closed-issue searches, Boolean expressions, missing values, and unclosed quotes show a validation message and no results. The supported subset follows [GitHub's qualifier conventions](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/filtering-and-searching-issues-and-pull-requests); it does not call GitHub's search endpoint or search bodies, comments, closed issues, or repositories that have not been added.

The field uses a standalone GPUI text-input entity adapted from the pinned GPUI input example. It handles Unicode grapheme movement, UTF-16 input-method ranges, selection, clipboard operations, and horizontal cursor scrolling. Search parsing lives in `src/issue_query.rs`; filtering and result navigation share the same visible-index calculation in `src/ui.rs`.

Requests use an anonymous HTTP client, a Nightshift user-agent, and the GitHub REST API. No credentials are loaded and no remote writes occur. Each UI request asks GitHub for at most ten records from one selected repository. Pull requests are excluded using the response's `pull_request` field. Pagination follows GitHub's exact `rel="next"` URL, including its cursor; Nightshift rejects pagination links that change the API scheme, host, or port. A failed page preserves previously fetched project results and produces a per-project error, including GitHub's validation message when available. Failures do not appear as successful empty searches. The UI permits one fetch at a time, so older requests cannot overwrite newer ones.

Initial limits are ten GitHub records per UI request, 20 seconds per HTTP request, and 8 MiB per response page. Issue results stay in memory until the selected project is refreshed or the app quits; execution records are not created by discovery. Requests and Git commands run outside GPUI's foreground thread. Git commands have a ten-second timeout and terminate when their future is dropped.

This scaffold does not evaluate parents or dependencies and does not claim that displayed issues are eligible for execution. Project setup commands, authentication, execution controls, and PR creation are still absent.

## Verification

Tests cover accepted remote formats, invalid hosts and paths, initialized checkout validation, remote deduplication and selection, public metadata, private-repository rejection, cursor-paginated results, pull-request exclusion, readiness filtering, retained results on a failed page, and GitHub validation messages. Storage tests also verify that projects reload after reopening. Query tests cover combined title, label, and repository filters; quoted phrases; exclusions; label alternatives; empty queries; and invalid or unsupported syntax.

For isolated native testing, set `NIGHTSHIFT_DATA_DIR` before launching the executable. Test projects then stay separate from normal application data.

The live adapter smoke check verified a local checkout with a `rust-lang/rustlings` remote and fetched open issues without credentials. Run `cargo run --locked --example discover -- /path/to/repository` to repeat the adapter check. Automated coverage verifies that the UI adapter requests ten records and retains GitHub's next-page cursor. Native checks cover search focus, ordinary typing and grouped undo, live filtering, project shortcuts, result navigation, and query preservation when switching projects.

API references: [repository metadata](https://docs.github.com/en/rest/repos/repos#get-a-repository) and [repository issues](https://docs.github.com/en/rest/issues/issues#list-repository-issues).
