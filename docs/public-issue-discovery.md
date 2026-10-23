# Issue discovery

The main screen adds local projects and fetches GitHub issues through the authenticated GitHub CLI. It replaces the synthetic replay screen. The broader execution workflow remains in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Project selection

The native folder picker accepts directories. Background Git commands discover the checkout root and require an initialized repository with a commit. The root is canonicalized before storage. GitHub HTTPS, SSH, and Git remotes are recognized; other hosts are ignored. Remotes pointing to the same repository are combined, preferring `origin`. Distinct repositories are presented for owner selection.

`gh api repos/{owner}/{repo}` verifies that the active account can access the repository, public or private. It supplies the stable repository identity and canonical owner/name before the project is saved. Re-adding a saved checkout selects it. Projects reload from SQLite at startup.

## Keyboard controls

- Shortcuts written with ⌘ use Command on macOS and Ctrl on Linux, and the UI labels them that way. Text-field word movement uses Option-arrow on macOS and Ctrl-arrow on Linux.
- `/` or ⌘F focuses the search field. Search has ordinary text editing, selection, copy, cut, and paste, with no Vim modes. Typing `/`, `j`, or `k` inside it inserts text.
- Enter in search or ⌘R refreshes the selected project's first ten GitHub results. **Load more** fetches that project's next page. Esc or Down from search focuses results without clearing the query.
- Up/Down or `j`/`k` browse matching results and scroll the selection into view. Enter opens the selected issue on GitHub.
- ⌘1–⌘9 select the first nine projects in sidebar order, preserving the search query. Each row shows its shortcut suffix. ⌘0 shows All projects. An unassigned number does nothing.
- ⌘O adds a project. Tab and Shift-Tab move focus. Enter activates a focused control; Space also activates project, remote, and issue rows. The folder picker uses the platform's standard keyboard controls.

## Issue search

The search field filters fetched open issues as the owner types. Refresh is scoped to the selected project and replaces only that project's cached rows. **Load more** appends and deduplicates the next page. The **All projects** view combines project pages already fetched during the current session and does not issue network requests. Changing or removing filters does not require another request. Browsing all issues does not change the readiness requirements for future execution.

The header keeps the search field, result count, and navigation hints visible without a permanent filter-examples line. **Load more** appears after the last matching issue in the scrollable list; it remains available below the empty state when filters match none of the loaded issues. The setup command editor opens only on request; see [execution controls](workflow-orchestration.md#executions-view). The compact bottom status bar reports the latest operation without repeating the issue view's shortcut hints.

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

## GitHub CLI adapter

Every GitHub request runs the installed `gh` with structured JSON output (`src/github/cli.rs`). There is no anonymous fallback, and the anonymous HTTP client and its TLS dependencies are removed.

- **Prerequisites.** Before repository verification, `gh` must be installed and signed in to github.com. Nightshift never installs it, runs `gh auth login`, or requests scopes. A missing or signed-out CLI produces a message asking the owner to fix it outside the app.
- **Capability probe.** Instead of trusting a version number, Nightshift checks that `gh --version` answers and that `gh api --help` lists `--method`, `--paginate`, `--slurp`, `--input`, and `--hostname`. It also requires `gh auth status --json hosts` to return the active account's `state` and `login`. Anything missing is reported as an incompatible CLI with an update instruction.
- **Token handling.** Nightshift never passes `--show-token` and never reads tokens or credential files. `gh auth status --json hosts` does not include the token.
- **Environment.** Commands use the [platform service's](platform-services.md) login-shell environment so desktop launches can find installed tools. Prompts, pagers, colors, and update notices are disabled. Each command has a 30-second timeout and 16 MiB of output, and is terminated if its task is dropped. `NIGHTSHIFT_GH` overrides the executable for isolated testing.
- **Errors.** Failures are classified from `gh`'s stderr: not found or inaccessible, forbidden, rate-limited, signed out, incompatible, or failed. Rate limits, timeouts, and generic failures are marked transient so an active run can back off.

The browse view asks for at most ten records per page from the selected repository with `gh api --include` and follows the `rel="next"` link. Links must stay on `https://api.github.com`. Pull requests are excluded. A failed page keeps earlier results and shows a per-project error. Failures never appear as empty searches, and the UI allows one fetch at a time. Results stay in memory until the project is refreshed or the app quits. Discovery never creates execution records.

## Start eligibility

Execution candidates are separate from browsing. `eligibility::candidates` lists open issues assigned to the active account and labeled `ready-for-agent` in every added project. Pull requests are excluded, and closed issues are never listed. A failing project reports its own error without hiding the others.

`application::start::check` evaluates every start gate immediately before reservation. It stops at the first project-level failure, so nothing is consumed:

1. **Run owner.** The active `gh` account must still be the login captured when the run started. A logout or account switch blocks new starts.
2. **Repository access.** The repository must be readable, must still have the enrolled node ID, must not be archived, and must grant the account push permission. A read-only repository is ineligible; forks are not used.
3. **Push preflight.** `git push --dry-run` to the selected remote authenticates and negotiates without sending objects or updating refs. Prompts are disabled with `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`. If the owner has not configured an SSH command, OpenSSH runs in batch mode. Git configuration is never modified.
4. **The issue itself.** It is re-read and must be open, not a pull request, assigned to the run owner, labeled `ready-for-agent`, and not a parent with sub-issues.
5. **Parent.** If a parent exists, it must be readable, open, and labeled. It may be assigned to anyone. Its title and body are saved as parent context.
6. **Dependencies.** The issue's and the open parent's "blocked by" lists come from `GET …/issues/{n}/dependencies/blocked_by`. A blocker resolves only when GitHub reports it closed, whatever the closure reason, including none. If GitHub's `total_blocked_by` count exceeds the blockers the account can read, the hidden ones block. A failed lookup also blocks. Inherited blockers are marked as such.

Only GitHub state counts. A finished local execution of a blocker never resolves it. `start::Generations` gives each request a number, so a late result never replaces a newer one.

## Verification

Tests use an in-memory fake `gh` (`src/github/fake.rs`) and temporary Git repositories. They cover:

- remote formats, checkout validation, and remote deduplication;
- a missing, incompatible, or signed-out CLI, and account switches;
- the subprocess environment, private-repository admission, and inaccessible repositories;
- read-only, archived, and replaced repositories;
- `Link` pagination, foreign-host links, and failed pages;
- candidate filtering with a partial failure;
- each issue gate, parent rules, inherited and cross-repository blockers, and every closure reason, including an absent one;
- inaccessible dependencies and failed lookups;
- a finished local execution not resolving an open blocker;
- ordered start gates, push preflight against writable and unreachable remotes, and stale generations. Storage tests also verify that projects reload after reopening. Query tests cover combined title, label, and repository filters; quoted phrases; exclusions; label alternatives; empty queries; and invalid or unsupported syntax.

For isolated native testing, set `NIGHTSHIFT_DATA_DIR` before launching the executable. Test projects then stay separate from normal application data.

The live smoke check ran on 2026-10-21 with `gh` 2.102.0. It verified this repository and `cli/cli` through the signed-in account, fetched a page of open issues, and evaluated eligibility for one public issue, including its dependency list. Repeat it with `cargo run --locked --example discover -- /path/to/repository [issue-number]`. Automated coverage verifies that the UI adapter requests ten records and retains GitHub's next-page cursor. Native checks cover search focus, ordinary typing and grouped undo, live filtering, project shortcuts, result navigation, and query preservation when switching projects.

API references: [repository metadata](https://docs.github.com/en/rest/repos/repos#get-a-repository) and [repository issues](https://docs.github.com/en/rest/issues/issues#list-repository-issues).
