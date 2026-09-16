CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    checkout_path TEXT NOT NULL UNIQUE,
    github_host TEXT NOT NULL COLLATE NOCASE,
    repository_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    remote TEXT NOT NULL,
    setup_command TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE executions (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id),
    github_host TEXT NOT NULL COLLATE NOCASE,
    issue_id TEXT NOT NULL,
    issue_number INTEGER NOT NULL CHECK(issue_number > 0),
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    parent_issue_id TEXT,
    parent_number INTEGER,
    parent_title TEXT,
    parent_description TEXT,
    status TEXT NOT NULL CHECK(status IN ('preparing', 'running', 'interrupted', 'failed')),
    termination_confirmed INTEGER NOT NULL DEFAULT 0 CHECK(termination_confirmed IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK((parent_issue_id IS NULL AND parent_number IS NULL AND parent_title IS NULL AND parent_description IS NULL)
       OR (parent_issue_id IS NOT NULL AND parent_number > 0 AND parent_title IS NOT NULL AND parent_description IS NOT NULL)),
    CHECK(parent_issue_id IS NULL OR parent_issue_id != issue_id),
    CHECK(status NOT IN ('preparing', 'running') OR termination_confirmed = 0),
    CHECK(status != 'failed' OR termination_confirmed = 1)
) STRICT;

CREATE UNIQUE INDEX one_reserved_issue ON executions(github_host, issue_id)
    WHERE termination_confirmed = 0;
CREATE UNIQUE INDEX one_reserved_child ON executions(github_host, parent_issue_id)
    WHERE termination_confirmed = 0 AND parent_issue_id IS NOT NULL;

CREATE TRIGGER immutable_execution_snapshot BEFORE UPDATE OF
    project_id, github_host, issue_id, issue_number, title, description,
    parent_issue_id, parent_number, parent_title, parent_description, created_at
ON executions BEGIN
    SELECT RAISE(ABORT, 'execution snapshots are immutable');
END;

CREATE TABLE execution_events (
    execution_id INTEGER NOT NULL REFERENCES executions(id),
    sequence INTEGER NOT NULL,
    previous_status TEXT,
    status TEXT NOT NULL,
    termination_confirmed INTEGER NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY(execution_id, sequence)
) STRICT;

CREATE TABLE execution_output (
    execution_id INTEGER NOT NULL REFERENCES executions(id),
    sequence INTEGER NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('setup', 'agent', 'stderr')),
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 65536),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY(execution_id, sequence)
) STRICT;

PRAGMA user_version = 1;
