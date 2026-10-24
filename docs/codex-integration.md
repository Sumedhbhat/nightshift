# Codex integration

This describes the Codex runtime boundary. The run loop that uses it is described in [workflow-orchestration.md](workflow-orchestration.md). Product rules are in [MVP_CONCEPT.md](../MVP_CONCEPT.md).

## Protocol client

`src/codex/rpc.rs` speaks line-delimited JSON-RPC with `codex app-server` over stdio. Messages omit the `jsonrpc` field, as the installed runtime does.

- **Correlation.** A reader task drains standard output continuously. It matches responses to requests by ID, in any order, and sends notifications and server requests to a bounded channel of 1,024 events. A full channel applies backpressure instead of growing memory.
- **Bounds and errors.** Lines are limited to 16 MiB. A non-JSON or unclassifiable line is a protocol error and ends the turn. End of file fails every pending request.
- **Standard error** is read separately and stored as `stderr` output.

## Turns

`codex::Session` runs one app server per execution:

Executable lookup and the app server's child environment come from [platform services](platform-services.md), including the login-shell `PATH` needed by script-based installations. `NIGHTSHIFT_CODEX` overrides the executable.

1. **Handshake.** `initialize` with client info `nightshift`, then the `initialized` notification. Each request has a 30-second timeout.
2. **Thread.** `thread/start` runs with the worktree as `cwd`, `sandbox: workspace-write`, and `approvalPolicy: never`. A retry with a saved thread ID uses `thread/resume` instead. The thread ID is saved immediately.
3. **Turn.** `turn/start` sends the prompt. Its response only acknowledges the start, and events may arrive before it. The turn ID is saved from `turn/started` or the acknowledgement, whichever comes first.
4. **Outcome.** It comes only from the matching turn's `turn/completed` status: `completed`, `failed` with its message, or `interrupted`. Events for other turns are ignored. Process exit or end of file before completion is an error.

Completed items are stored as readable output: agent messages, plans, commands with exit codes, changed files, and tool calls. Command output deltas are stored as they stream. Output is batched to storage every 33 ms or every 256 blocks.

The prompt names the saved issue as the task and supplies the parent, if any, under a separate "context only" heading. It tells Codex not to commit, push, or open pull requests; Nightshift does that during publication.

## Server requests

With `approvalPolicy: never`, approval requests should not arrive. Any that do are answered immediately and noted in the output (decision D13):

| Request | Response |
| --- | --- |
| `item/commandExecution/requestApproval`, `item/fileChange/requestApproval` | `{"decision": "decline"}` |
| `execCommandApproval`, `applyPatchApproval` (legacy) | `{"decision": "denied"}` |
| `mcpServer/elicitation/request` | `{"action": "decline"}` |
| Anything else, including `item/tool/requestUserInput` | JSON-RPC error `-32601` |

## Process ownership and stopping

The app server runs in its own process group. Codex's Linux sandbox starts commands in new sessions, outside that group, and they outlive the app server. In a real interrupted turn, a `sleep` started by Codex kept running after the server exited.

Stopping therefore works in three steps (decision D14):

1. Snapshot the app server's descendants.
2. Close standard input and wait 5 seconds. Then send SIGTERM to the group and wait 5 seconds, then SIGKILL.
3. Stop every captured descendant, plus any Codex sandbox process whose command line names the worktree, with SIGTERM and then SIGKILL.

`shutdown` returns true only when all of them are gone. Only then may storage release the slot.

An orderly stop first sends `turn/interrupt` and waits up to 10 seconds for the turn to report `interrupted`. After a crash, `stop_orphan` checks whether the recorded PID is still a Codex app server, so a reused PID is never signaled. It then sweeps sandbox processes for the worktree.

## Verification

Fake-server tests (`src/codex/tests.rs`) run over in-memory streams. They cover:

- the handshake, and events arriving before the `turn/start` response;
- out-of-order responses and remote errors;
- a 5,000-event output burst kept in order and batched;
- each terminal status, with a late completion from another turn ignored;
- answers to every unsupported request type;
- end of file mid-turn, malformed output, and exit during the handshake;
- an orderly stop through `turn/interrupt`, and resume with a saved thread;
- the prompt layout.

A process test uses a shell script as the server. It covers a reported abrupt exit, and a stubborn process that ignores stdin closing and SIGTERM and has a child in its own session. All of them are stopped.

Real runtime, codex-cli 0.162.1 on Linux, 2026-10-21, using `cargo run --example codex_turn -- <dir> "<prompt>"`:

| Run | Result |
| --- | --- |
| Create one file | Completed in 7.8 s. The file was created inside the directory, and the process stopped. |
| Long command with `NIGHTSHIFT_STOP_AFTER=8` | Interrupted after 9.0 s. The process stopped, and no command remained after descendant cleanup. |
