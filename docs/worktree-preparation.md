# Worktree preparation

This implements milestone 4 of the [implementation plan](implementation-plan.md). Preparation runs after an execution enters `working` and before Codex starts. Product rules are in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Steps

`git::worktree::prepare` performs these stages and records evidence through a `Recorder`, normally the execution's storage record:

1. **Fetch.** `git fetch --no-tags --no-recurse-submodules <remote> +refs/heads/<default>:refs/remotes/<remote>/<default>` updates only the remote-tracking ref. The fetched commit is resolved and saved with the default branch before anything else happens.
2. **Branch.** The name is `nightshift/<issue>-<title-slug>-<execution-id>`. If a local branch with that name exists, a numeric suffix is added. Owner branches are never reused.
3. **Worktree.** The worktree goes under the application data directory, in `worktrees/<owner>-<repo>/<branch-leaf>`, outside every checkout. An existing directory at that path is an error and is left untouched. The branch and path are recorded before `git worktree add --no-track -b <branch> <path> <commit>` runs, so a partial failure still names them.
4. **Setup.** The project's optional setup command runs with `/bin/sh -c` in the worktree, with standard input closed. Standard output and error stream line by line into the execution's output, truncated at 64 KiB per line. A nonzero exit, or no exit within 30 minutes, is a setup failure. Codex is not started, and the worktree and branch remain.

Git runs with argument arrays and the non-interactive environment described in [public-issue-discovery.md](public-issue-discovery.md#start-eligibility). The owner's checkout is never switched, reset, or edited. Its HEAD, index, working files, and untracked files stay as they were.

## Retries and failures

Failures return the stage and a message, which the orchestrator saves as a step error at `working`. Nothing is cleaned up automatically.

On **Retry working**, recorded evidence is reused. If the worktree path, branch, and base commit are all recorded, preparation checks that the worktree still exists on that branch, then reruns setup. A missing worktree or a different branch is an error for the owner to resolve; Nightshift never recreates or moves it.

## Verification

Tests use temporary bare remotes and clones. They cover:

- the base commit coming from the fetched default branch rather than the owner's HEAD, after another clone pushed;
- the owner's checkout keeping its branch, edits, and untracked files;
- unique branches and paths per execution, including a clash with an existing owner branch;
- a setup failure keeping the worktree, branch, and captured output;
- setup running inside the worktree;
- a fetch failure recording nothing;
- a worktree failure keeping the base commit and an existing directory;
- retry reuse and rejection of a changed worktree;
- safe slugs.
