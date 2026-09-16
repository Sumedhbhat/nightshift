# Nightshift terminology

**Project**: An existing local repository added to Nightshift, corresponding to one GitHub repository.

**Execution**: Codex work on one issue within the owner's authorized scope, tracked locally with its own worktree and status history.

**Concurrency limit**: The configured maximum number of executions in progress at once across all added projects. It does not limit the total number of issues processed during a run.

**Execution description**: The issue description saved when execution starts and used as its requirements.

**Parent context**: The parent issue content supplied alongside a child's execution description to explain the broader change. The child issue remains the execution's task.

**Ready for agent**: The `ready-for-agent` label required on an issue before execution. If the issue has a parent, the parent must carry the label too. Dependencies must also be resolved or eligible to build on through a stack; sharing a parent does not create a dependency.

**Parent issue**: An issue that supplies the big-picture context for its children rather than a separate execution. Its readiness label gates child eligibility, and its completion depends on all its children completing.

**Issue dependency**: An issue linked as a blocker through GitHub's explicit "blocked by" relationship. Sharing a parent or mentioning an issue in a description or comment does not create a dependency.

**Inherited dependency**: A dependency declared on a parent that also gates its child executions. If the blocker is another parent, all of that parent's children must finish before dependent children can start.

**Resolved dependency**: An issue dependency whose blocker is closed as completed in GitHub, with child completion also required for a parent blocker. Permission to build on finished but unmerged work in a stack is distinct from resolution.

**PR stack**: An ordered chain of pull requests in which each dependent PR targets the branch of its predecessor, allowing work to build on changes that have not merged yet.

**Parent stack**: The sequential PR stack for one parent's child executions. Children run one at a time and build on the preceding child's branch; different parent stacks can progress concurrently.

**Audit history**: The retained record of execution creation and state changes.

**Interrupted execution**: An execution cut short by quitting Nightshift or losing contact with its running work. It continues to occupy an execution slot until that work is confirmed stopped.
