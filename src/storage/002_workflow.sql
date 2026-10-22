-- Schema 2: typed workflow steps, slot attempts, step errors, and settings.
-- Rebuilds tables whose CHECK constraints change. Schema 1 rows become legacy history.

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE executions_v2 (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    github_host TEXT NOT NULL COLLATE NOCASE,
    issue_id TEXT NOT NULL,
    issue_number INTEGER NOT NULL CHECK(issue_number > 0),
    run_owner TEXT NOT NULL,
    -- Immutable snapshots, saved once when the execution enters `working`.
    title TEXT,
    description TEXT,
    parent_issue_id TEXT,
    parent_number INTEGER,
    parent_title TEXT,
    parent_description TEXT,
    snapshot_at TEXT,
    step TEXT NOT NULL CHECK(step IN ('checking', 'working', 'branch_published', 'pull_request_published')),
    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'error', 'interrupted', 'ended')),
    outcome TEXT CHECK(outcome IN ('pull_request_published', 'publication_skipped', 'legacy')),
    -- Working evidence.
    default_branch TEXT,
    base_commit TEXT,
    branch TEXT,
    worktree_path TEXT,
    codex_thread_id TEXT,
    codex_turn_id TEXT,
    work_finished_at TEXT,
    work_result TEXT,
    -- Publication evidence.
    pushed_remote TEXT,
    pushed_ref TEXT,
    pushed_commit TEXT,
    pull_request_number INTEGER,
    pull_request_url TEXT,
    pull_request_id TEXT,
    -- Step error.
    error_transition TEXT,
    error_message TEXT,
    error_detail TEXT,
    error_retryable INTEGER CHECK(error_retryable IN (0, 1)),
    legacy INTEGER NOT NULL DEFAULT 0 CHECK(legacy IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK((parent_issue_id IS NULL AND parent_number IS NULL AND parent_title IS NULL AND parent_description IS NULL)
       OR (parent_issue_id IS NOT NULL AND parent_number > 0 AND parent_title IS NOT NULL AND parent_description IS NOT NULL)),
    CHECK(parent_issue_id IS NULL OR parent_issue_id != issue_id),
    CHECK((title IS NULL) = (description IS NULL) AND (title IS NULL) = (snapshot_at IS NULL)),
    CHECK(step = 'checking' OR legacy = 1 OR snapshot_at IS NOT NULL),
    CHECK((status = 'ended') = (outcome IS NOT NULL)),
    CHECK((status = 'error') = (error_message IS NOT NULL)),
    CHECK(step NOT IN ('branch_published', 'pull_request_published') OR pushed_ref IS NOT NULL),
    CHECK(step != 'pull_request_published' OR pull_request_url IS NOT NULL)
) STRICT;

INSERT INTO executions_v2 (id, project_id, github_host, issue_id, issue_number, run_owner, title, description,
    parent_issue_id, parent_number, parent_title, parent_description, snapshot_at, step, status, outcome,
    error_transition, error_message, error_detail, error_retryable, legacy, created_at, updated_at)
SELECT id, project_id, github_host, issue_id, issue_number, '', title, description,
    parent_issue_id, parent_number, parent_title, parent_description, created_at, 'working',
    CASE WHEN termination_confirmed = 1 THEN 'ended' ELSE 'interrupted' END,
    CASE WHEN termination_confirmed = 1 THEN 'legacy' END,
    NULL, NULL, NULL, NULL, 1, created_at, updated_at
FROM executions;

CREATE TABLE execution_events_v2 (
    execution_id INTEGER NOT NULL REFERENCES executions_v2(id),
    sequence INTEGER NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT NOT NULL,
    detail TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY(execution_id, sequence)
) STRICT;

INSERT INTO execution_events_v2 (execution_id, sequence, step, status, reason, detail, created_at)
SELECT execution_id, sequence, 'working', status, reason,
    'schema 1: ' || coalesce(previous_status, 'none') || ' -> ' || status
        || CASE WHEN termination_confirmed = 1 THEN ', termination confirmed' ELSE '' END,
    created_at
FROM execution_events;

-- Slot attempts hold capacity for steps whose policy consumes an execution slot.
CREATE TABLE slot_attempts (
    execution_id INTEGER NOT NULL REFERENCES executions_v2(id),
    number INTEGER NOT NULL CHECK(number > 0),
    step TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    process_id INTEGER,
    ended_at TEXT,
    end_reason TEXT,
    released_at TEXT,
    PRIMARY KEY(execution_id, number),
    CHECK(released_at IS NULL OR ended_at IS NOT NULL)
) STRICT;

-- Unfinished schema 1 executions keep their capacity until termination is confirmed.
INSERT INTO slot_attempts (execution_id, number, step, started_at, ended_at, end_reason, released_at)
SELECT id, 1, 'working', created_at,
    CASE WHEN status IN ('interrupted', 'failed') THEN updated_at END,
    CASE WHEN status IN ('interrupted', 'failed') THEN 'schema 1 ' || status END,
    CASE WHEN termination_confirmed = 1 THEN updated_at END
FROM executions;

CREATE TABLE execution_output_v2 (
    execution_id INTEGER NOT NULL REFERENCES executions_v2(id),
    sequence INTEGER NOT NULL,
    attempt INTEGER,
    source TEXT NOT NULL CHECK(source IN ('system', 'git', 'setup', 'agent', 'stderr')),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 65536),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY(execution_id, sequence)
) STRICT;

INSERT INTO execution_output_v2 (execution_id, sequence, attempt, source, payload, created_at)
SELECT execution_id, sequence, 1, source, payload, created_at FROM execution_output;

DROP TRIGGER immutable_execution_snapshot;
DROP TABLE execution_output;
DROP TABLE execution_events;
DROP TABLE executions;
ALTER TABLE executions_v2 RENAME TO executions;
ALTER TABLE execution_events_v2 RENAME TO execution_events;
ALTER TABLE execution_output_v2 RENAME TO execution_output;

CREATE UNIQUE INDEX one_execution_per_issue ON executions(github_host, issue_id) WHERE legacy = 0;
CREATE UNIQUE INDEX one_unreleased_attempt ON slot_attempts(execution_id) WHERE released_at IS NULL;
CREATE INDEX unreleased_attempts ON slot_attempts(step) WHERE released_at IS NULL;

-- Snapshots and identity are written once and never change.
CREATE TRIGGER immutable_execution_identity BEFORE UPDATE OF
    project_id, github_host, issue_id, issue_number, run_owner, created_at
ON executions BEGIN
    SELECT RAISE(ABORT, 'execution identity is immutable');
END;

CREATE TRIGGER immutable_execution_snapshot BEFORE UPDATE OF
    title, description, parent_issue_id, parent_number, parent_title, parent_description, snapshot_at
ON executions WHEN OLD.snapshot_at IS NOT NULL BEGIN
    SELECT RAISE(ABORT, 'execution snapshots are immutable');
END;

-- Workflow steps only move forward.
CREATE TRIGGER monotonic_execution_step BEFORE UPDATE OF step ON executions
WHEN (CASE NEW.step WHEN 'checking' THEN 0 WHEN 'working' THEN 1 WHEN 'branch_published' THEN 2 ELSE 3 END)
   < (CASE OLD.step WHEN 'checking' THEN 0 WHEN 'working' THEN 1 WHEN 'branch_published' THEN 2 ELSE 3 END)
BEGIN
    SELECT RAISE(ABORT, 'workflow steps cannot move backward');
END;

CREATE TRIGGER ended_execution_is_final BEFORE UPDATE ON executions
WHEN OLD.status = 'ended' BEGIN
    SELECT RAISE(ABORT, 'ended executions are final');
END;

PRAGMA user_version = 2;
