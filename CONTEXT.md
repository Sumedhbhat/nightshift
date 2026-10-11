# Nightshift terminology

**Primary shortcut modifier**: The platform-appropriate modifier used for common application shortcuts: Command on macOS and Control on Linux and Windows. The UI displays the matching key label.

**Project**: An existing local repository added to Nightshift, corresponding to one GitHub repository.

**Execution**: Codex work on one issue within the owner's authorized scope, tracked locally with its own worktree and status history. Once local work starts, changes or failures in GitHub state do not interrupt it; only explicit owner shutdown or a terminal local process condition can stop it.

**Start eligibility**: Permission to begin local execution after verifying every required GitHub and local gate, including open state, assignment, readiness, parent state, dependencies, run-owner identity, repository access, and push preflight.

**Publication eligibility**: Permission to push an execution branch and open its pull request after local work finishes. Its only workflow gate is that the issue remains open; a closed issue preserves the local result without new remote publication.

**Publication recheck**: An owner-requested retry after Nightshift could not verify publication eligibility or complete publication. Nightshift never retries publication automatically.

**Workflow step**: One persisted position in an execution's ordered lifecycle: checking, working, branch published, or pull request published. A step advances only after its required work and evidence are saved; a failure remains at that step for an explicit owner retry.

**Step policy**: The fixed orchestration metadata attached to a workflow step, including whether occupying that step consumes an execution slot. The orchestrator applies the policy before allowing the workflow to enter the step.

**Step error**: A recoverable failure attached to the current workflow step, including the attempted transition and diagnostic detail. It does not advance the workflow; an explicit owner retry resumes orchestration from that step.

**Concurrency limit**: The configured maximum number of executions in progress at once across all added projects. It does not limit the total number of issues processed during a run.

**Execution description**: The issue description saved when execution starts and used as its requirements.

**Parent context**: The parent issue content supplied alongside a child's execution description to explain the broader change. The child issue remains the execution's task.

**Accountable owner**: The active GitHub CLI account when that account is assigned to an issue. Nightshift uses that account's GitHub access and only executes issues for which it is an assignee.

**Run owner**: The accountable owner's GitHub identity captured when a run starts. Every new execution in that run must use the same active GitHub CLI identity.

**Ready for agent**: The `ready-for-agent` label by which the accountable owner delegates an assigned issue to Nightshift. If the issue has a parent, the parent must carry the label too. Dependencies must also be resolved; sharing a parent does not create a dependency.

**Parent issue**: An open issue that supplies the big-picture context for its children rather than a separate execution. It may be assigned to a different team member; only its readiness label and state gate child eligibility. Closing it makes its entire child pile ineligible for new executions, but does not stop a child execution already in progress.

**Issue dependency**: An issue linked as a blocker through GitHub's explicit "blocked by" relationship. Sharing a parent or mentioning an issue in a description or comment does not create a dependency.

**Inherited dependency**: A dependency declared on an open parent that also gates its child executions. If the blocker closes, the dependency resolves regardless of whether the blocker has open children.

**Resolved dependency**: An issue dependency whose blocker is closed in GitHub. For implemented work, the team closes the blocker after its pull request is merged. Finished local work, an open pull request, or an unmerged branch does not resolve a dependency.

**Audit history**: The retained record of execution creation and state changes.

**Interrupted execution**: An execution cut short by quitting Nightshift or losing contact with its running work. It continues to occupy an execution slot until that work is confirmed stopped.
