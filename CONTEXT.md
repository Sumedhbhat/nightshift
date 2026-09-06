# Nightshift terminology

**Project**: An existing local repository added to Nightshift, corresponding to one GitHub repository.

**Execution**: Owner-requested Codex work on one issue, tracked locally with its own worktree and status history.

**Execution description**: The issue description saved when execution starts and used as its requirements.

**Ready for agent**: The `ready-for-agent` label required on an issue before execution. Explicit unresolved dependencies still block it; sharing a parent does not create a dependency.

**Audit history**: The retained record of execution creation and state changes.
