//! One `codex app-server` child per execution, driven over stdio. Nightshift owns the process
//! and its process group; Codex owns its stored threads.
pub mod rpc;

use crate::{
    domain::{IssueSnapshot, OutputSource},
    github::cli::BoxFuture,
    storage::MAX_OUTPUT_BYTES,
};
use rpc::{Connection, Event, RpcError};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, watch},
};

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an interrupted turn may take to report completion before the process is stopped.
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(10);
/// How long the process may take to exit after standard input closes, then after SIGTERM.
pub const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Output is flushed to storage at most this often, or when a batch fills.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(33);
pub const FLUSH_BLOCKS: usize = 256;
/// JSON-RPC "method not found", used for server requests the MVP cannot handle.
pub const UNSUPPORTED: i64 = -32601;

/// Runtime policy (decision D13): writes stay inside the worktree, no approval prompts.
pub const SANDBOX: &str = "workspace-write";
pub const APPROVAL_POLICY: &str = "never";

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodexError {
    #[error("Could not start Codex: {0}")]
    Spawn(String),
    #[error("Codex handshake failed: {0}")]
    Handshake(String),
    #[error("Codex sent an invalid message: {0}")]
    Protocol(String),
    #[error("Codex exited before the turn finished: {0}")]
    Exited(String),
    #[error("Could not save Codex output: {0}")]
    Sink(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// Terminal turn status reported by `turn/completed` for the matching turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Failed(String),
    Interrupted,
}

/// Receives thread and turn IDs and output as they arrive, normally the execution's storage.
pub trait TurnSink: Send + Sync {
    fn thread(&self, thread_id: String) -> BoxFuture<'_, Result<(), String>>;
    fn turn(&self, turn_id: String) -> BoxFuture<'_, Result<(), String>>;
    fn output(&self, blocks: Vec<(OutputSource, String)>) -> BoxFuture<'_, Result<(), String>>;
}

#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub cwd: PathBuf,
    pub prompt: String,
    /// A saved thread from an earlier attempt, continued with `thread/resume`.
    pub resume_thread: Option<String>,
}

/// How to start the app server: `codex app-server` normally; a script in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl CodexCommand {
    /// `NIGHTSHIFT_CODEX` overrides the executable; otherwise use the platform command search path.
    pub fn from_env() -> Self {
        Self {
            program: crate::platform::current()
                .var_os("NIGHTSHIFT_CODEX")
                .map(PathBuf::from)
                .unwrap_or_else(|| "codex".into()),
            args: vec!["app-server".into()],
        }
    }
}

/// A connection plus, for real runs, the owned child process.
pub struct Session {
    connection: Connection,
    events: mpsc::Receiver<Event>,
    child: Option<tokio::process::Child>,
}

impl Session {
    /// Spawn the app server in its own process group so stop can reach its children.
    pub fn spawn(codex: &CodexCommand, cwd: &Path) -> Result<Self, CodexError> {
        let program = &codex.program;
        let mut command = crate::platform::command(program);
        command
            .args(&codex.args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| CodexError::Spawn(format!("{}: {error}", program.display())))?;
        let (sender, events) = mpsc::channel(rpc::EVENT_CAPACITY);
        let stderr = child.stderr.take().expect("piped");
        let stderr_events = sender.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stderr_events.send(Event::Stderr(line)).await.is_err() {
                    break;
                }
            }
        });
        let connection = Connection::new(
            child.stdout.take().expect("piped"),
            child.stdin.take().expect("piped"),
            sender,
        );
        Ok(Self {
            connection,
            events,
            child: Some(child),
        })
    }

    /// A session over in-memory streams, for tests.
    pub fn from_streams(
        reader: impl tokio::io::AsyncRead + Send + Unpin + 'static,
        writer: impl tokio::io::AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        let (sender, events) = mpsc::channel(rpc::EVENT_CAPACITY);
        Self {
            connection: Connection::new(reader, writer, sender),
            events,
            child: None,
        }
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(tokio::process::Child::id)
    }

    pub async fn handshake(&self) -> Result<(), CodexError> {
        self.connection
            .request(
                "initialize",
                json!({"clientInfo": {"name": "nightshift", "title": "Nightshift", "version": crate::VERSION}}),
                HANDSHAKE_TIMEOUT,
            )
            .await
            .map_err(|e| CodexError::Handshake(e.to_string()))?;
        self.connection
            .notify("initialized", json!({}))
            .await
            .map_err(|e| CodexError::Handshake(e.to_string()))
    }

    /// Run one turn to its terminal status. `stop` asks for an orderly interruption.
    pub async fn run_turn(
        &mut self,
        request: &TurnRequest,
        sink: &dyn TurnSink,
        mut stop: watch::Receiver<bool>,
    ) -> Result<TurnOutcome, CodexError> {
        let cwd = request.cwd.to_string_lossy();
        let thread_params =
            json!({"cwd": cwd, "sandbox": SANDBOX, "approvalPolicy": APPROVAL_POLICY});
        let thread = match &request.resume_thread {
            Some(thread_id) => {
                let mut params = thread_params;
                params["threadId"] = json!(thread_id);
                self.connection
                    .request("thread/resume", params, HANDSHAKE_TIMEOUT)
                    .await?
            }
            None => {
                self.connection
                    .request("thread/start", thread_params, HANDSHAKE_TIMEOUT)
                    .await?
            }
        };
        let thread_id = thread["thread"]["id"]
            .as_str()
            .ok_or_else(|| CodexError::Protocol("thread response has no thread.id".into()))?
            .to_owned();
        sink.thread(thread_id.clone())
            .await
            .map_err(CodexError::Sink)?;
        // The turn/start response only acknowledges the start; it may arrive after events.
        let start = {
            let connection = self.connection.clone();
            let params =
                json!({"threadId": thread_id, "input": [{"type": "text", "text": request.prompt}]});
            tokio::spawn(async move {
                connection
                    .request("turn/start", params, HANDSHAKE_TIMEOUT)
                    .await
            })
        };
        let mut start = Some(start);
        let mut turn_id: Option<String> = None;
        let mut output = Batch::default();
        let mut interrupting: Option<Instant> = None;
        // Resolve the turn ID from the turn/start acknowledgement.
        async fn acknowledged(
            start: &mut Option<tokio::task::JoinHandle<Result<Value, RpcError>>>,
            turn_id: &mut Option<String>,
            sink: &dyn TurnSink,
        ) -> Result<(), CodexError> {
            let Some(handle) = start.take() else {
                return Ok(());
            };
            let response = handle
                .await
                .map_err(|e| CodexError::Protocol(e.to_string()))??;
            let id = response["turn"]["id"]
                .as_str()
                .ok_or_else(|| CodexError::Protocol("turn/start response has no turn.id".into()))?
                .to_owned();
            if turn_id.is_none() {
                *turn_id = Some(id.clone());
                sink.turn(id).await.map_err(CodexError::Sink)?;
            }
            Ok(())
        }
        loop {
            if start.as_ref().is_some_and(|handle| handle.is_finished()) {
                acknowledged(&mut start, &mut turn_id, sink).await?;
            }
            if *stop.borrow() && interrupting.is_none() {
                interrupting = Some(Instant::now());
                output.push(
                    OutputSource::System,
                    "Stopping: interrupting the Codex turn.".into(),
                );
                if let Some(turn) = &turn_id {
                    let connection = self.connection.clone();
                    let params = json!({"threadId": thread_id, "turnId": turn});
                    tokio::spawn(async move {
                        connection
                            .request("turn/interrupt", params, HANDSHAKE_TIMEOUT)
                            .await
                    });
                }
            }
            if interrupting.is_some_and(|since| since.elapsed() > INTERRUPT_GRACE) {
                output.flush(sink).await?;
                return Ok(TurnOutcome::Interrupted);
            }
            let event = select::next(&mut self.events, &mut stop, output.due()).await;
            let Some(event) = event else {
                output.flush_if_due(sink).await?;
                continue;
            };
            match event {
                Event::Notification { method, params } => {
                    // Responses are read in order, so once a turn-scoped event arrives without a
                    // known turn, the acknowledgement (if sent first) is already available.
                    let turn_scoped = method == "turn/completed" || params.get("turnId").is_some();
                    if turn_id.is_none() && turn_scoped && method != "turn/started" {
                        acknowledged(&mut start, &mut turn_id, sink).await?;
                    }
                    let for_turn = params.get("turnId").and_then(Value::as_str);
                    if let (Some(known), Some(other)) = (&turn_id, for_turn)
                        && known != other
                    {
                        continue; // Late events from another turn never affect this one.
                    }
                    match method.as_str() {
                        "turn/started" => {
                            if let Some(id) = params["turn"]["id"].as_str()
                                && turn_id.is_none()
                            {
                                turn_id = Some(id.to_owned());
                                sink.turn(id.to_owned()).await.map_err(CodexError::Sink)?;
                            }
                        }
                        "item/completed" => {
                            if let Some(text) = render_item(&params["item"]) {
                                output.push(OutputSource::Agent, text);
                            }
                        }
                        "item/commandExecution/outputDelta" => {
                            if let Some(delta) = params["delta"].as_str() {
                                output.push(OutputSource::Agent, delta.to_owned());
                            }
                        }
                        "error" => {
                            let message = params["error"]["message"]
                                .as_str()
                                .unwrap_or("unknown error");
                            let retry = params["willRetry"].as_bool().unwrap_or(false);
                            output.push(
                                OutputSource::Stderr,
                                format!(
                                    "Codex error: {message}{}",
                                    if retry { " (retrying)" } else { "" }
                                ),
                            );
                        }
                        "turn/completed" => {
                            let turn = &params["turn"];
                            let id = turn["id"].as_str().unwrap_or_default();
                            if turn_id.as_ref().is_some_and(|known| known != id) {
                                continue;
                            }
                            if turn_id.is_none() {
                                sink.turn(id.to_owned()).await.map_err(CodexError::Sink)?;
                            }
                            let outcome = match turn["status"].as_str() {
                                Some("completed") => TurnOutcome::Completed,
                                Some("interrupted") => TurnOutcome::Interrupted,
                                Some("failed") => TurnOutcome::Failed(
                                    turn["error"]["message"]
                                        .as_str()
                                        .unwrap_or("the turn failed")
                                        .to_owned(),
                                ),
                                other => {
                                    return Err(CodexError::Protocol(format!(
                                        "unexpected terminal status {other:?}"
                                    )));
                                }
                            };
                            output.push(
                                OutputSource::System,
                                format!(
                                    "Codex turn finished: {}",
                                    turn["status"].as_str().unwrap_or("")
                                ),
                            );
                            output.flush(sink).await?;
                            return Ok(outcome);
                        }
                        _ => {}
                    }
                }
                Event::Request { id, method, params } => {
                    let note = self.refuse(id, &method, &params).await?;
                    output.push(OutputSource::System, note);
                }
                Event::Stderr(line) => output.push(OutputSource::Stderr, line),
                Event::Malformed(line) => {
                    output.flush(sink).await?;
                    return Err(CodexError::Protocol(line));
                }
                Event::Closed | Event::Exited(_) => {
                    output.flush(sink).await?;
                    let status = self.wait_exit(Duration::from_secs(1)).await;
                    return Err(CodexError::Exited(
                        status.unwrap_or_else(|| "output closed".into()),
                    ));
                }
            }
            output.flush_if_due(sink).await?;
        }
    }

    /// Answer a server request the MVP cannot satisfy. Approvals are declined so Codex can
    /// continue without the action; everything else gets "method not supported".
    async fn refuse(&self, id: Value, method: &str, params: &Value) -> Result<String, CodexError> {
        let decline = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Some(json!({"decision": "decline"}))
            }
            "execCommandApproval" | "applyPatchApproval" => Some(json!({"decision": "denied"})),
            "mcpServer/elicitation/request" => Some(json!({"action": "decline"})),
            _ => None,
        };
        let detail = params
            .get("command")
            .or_else(|| params.get("reason"))
            .map(|v| v.to_string())
            .unwrap_or_default();
        match decline {
            Some(result) => {
                self.connection.respond(id, result).await?;
                Ok(format!(
                    "Declined Codex request {method} {detail}: Nightshift runs without interactive approvals."
                ))
            }
            None => {
                self.connection
                    .respond_error(
                        id,
                        UNSUPPORTED,
                        "Nightshift does not support this request during unattended work.",
                    )
                    .await?;
                Ok(format!("Refused unsupported Codex request {method}."))
            }
        }
    }

    async fn wait_exit(&mut self, timeout: Duration) -> Option<String> {
        let child = self.child.as_mut()?;
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => Some(status.to_string()),
            _ => None,
        }
    }

    /// Stop the owned process and everything it started. Codex's sandbox launches commands in
    /// their own sessions, outside Nightshift's process group, so descendants are captured before
    /// signaling and any sandbox wrapper still naming `worktree` is swept afterwards.
    /// Returns true once the app server and every captured process are confirmed gone.
    pub async fn shutdown(&mut self, worktree: Option<&Path>) -> bool {
        let pid = self.child.as_ref().and_then(tokio::process::Child::id);
        let descendants = match pid {
            Some(pid) => descendants(pid).await,
            None => vec![],
        };
        self.connection.close().await;
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        let mut exited = matches!(
            tokio::time::timeout(EXIT_GRACE, child.wait()).await,
            Ok(Ok(_))
        );
        if !exited && let Some(pid) = pid {
            signal_group(pid, "TERM").await;
            exited = matches!(
                tokio::time::timeout(EXIT_GRACE, child.wait()).await,
                Ok(Ok(_))
            );
            if !exited {
                signal_group(pid, "KILL").await;
            }
        }
        if !exited {
            let _ = child.start_kill();
            exited = matches!(
                tokio::time::timeout(EXIT_GRACE, child.wait()).await,
                Ok(Ok(_))
            );
        }
        let mut leftovers = descendants;
        if let Some(worktree) = worktree {
            leftovers.extend(sandbox_processes(worktree).await);
        }
        exited && stop_processes(&leftovers).await
    }
}

/// Every live descendant of `root`, from one `ps` snapshot.
pub async fn descendants(root: u32) -> Vec<u32> {
    let Ok(output) = crate::platform::command("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
        .await
    else {
        return vec![];
    };
    let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut found = vec![];
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &pairs {
            if *ppid == parent && !found.contains(pid) {
                found.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    found
}

/// Codex sandbox wrappers whose command line names this worktree, and their descendants.
/// They survive their app server, so this finds them after a crash too.
pub async fn sandbox_processes(worktree: &Path) -> Vec<u32> {
    let Ok(output) = crate::platform::command("ps")
        .args(["-A", "-o", "pid=,args="])
        .output()
        .await
    else {
        return vec![];
    };
    let needle = worktree.to_string_lossy().to_string();
    let mut found = vec![];
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, args)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pid != std::process::id()
            && args.contains("codex")
            && args.contains("sandbox")
            && args.contains(&needle)
        {
            found.push(pid);
            found.extend(descendants(pid).await);
        }
    }
    found
}

async fn alive(pid: u32) -> bool {
    crate::platform::command("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// TERM, then KILL, each process and its group. Returns true when none remain.
pub async fn stop_processes(pids: &[u32]) -> bool {
    for signal in ["TERM", "KILL"] {
        let mut remaining = vec![];
        for pid in pids {
            if alive(*pid).await {
                remaining.push(*pid);
            }
        }
        if remaining.is_empty() {
            return true;
        }
        for pid in &remaining {
            signal_group(*pid, signal).await;
            signal_pid(*pid, signal).await;
        }
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut any = false;
            for pid in &remaining {
                any |= alive(*pid).await;
            }
            if !any {
                return true;
            }
        }
    }
    false
}

async fn signal_pid(pid: u32, signal: &str) {
    let _ = crate::platform::command("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

/// Send a signal to a process group with the system `kill`, avoiding unsafe FFI.
async fn signal_group(pid: u32, signal: &str) {
    let _ = crate::platform::command("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pid}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

/// Whether `pid` is a live Codex app server, used after a crash to decide if work is stopped.
/// A reused PID that belongs to another program counts as stopped.
pub async fn is_live_app_server(pid: u32) -> bool {
    let output = crate::platform::command("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => {
            let command = String::from_utf8_lossy(&output.stdout);
            command.contains("codex") && command.contains("app-server")
        }
        _ => false,
    }
}

/// Stop an orphaned app server from a previous Nightshift process, plus any sandboxed command
/// still working in the worktree. Returns true when nothing owned remains.
pub async fn stop_orphan(pid: Option<u32>, worktree: Option<&Path>) -> bool {
    let mut targets = vec![];
    if let Some(pid) = pid
        && is_live_app_server(pid).await
    {
        targets.push(pid);
        targets.extend(descendants(pid).await);
    }
    if let Some(worktree) = worktree {
        targets.extend(sandbox_processes(worktree).await);
    }
    stop_processes(&targets).await
}

/// Wait for the next event, a stop request, or the output flush deadline.
mod select {
    use super::*;
    use std::future::Future;
    pub async fn next(
        events: &mut mpsc::Receiver<Event>,
        stop: &mut watch::Receiver<bool>,
        flush_in: Option<Duration>,
    ) -> Option<Event> {
        let event = std::pin::pin!(events.recv());
        let stopped = std::pin::pin!(async {
            if *stop.borrow() {
                std::future::pending::<()>().await;
            }
            let _ = stop.changed().await;
        });
        let wait = flush_in.unwrap_or(Duration::from_millis(250));
        let timer = std::pin::pin!(tokio::time::sleep(wait));
        let mut event = event;
        let mut stopped = stopped;
        let mut timer = timer;
        std::future::poll_fn(move |cx| {
            use std::task::Poll;
            if let Poll::Ready(event) = event.as_mut().poll(cx) {
                return Poll::Ready(Some(event.unwrap_or(Event::Closed)));
            }
            if stopped.as_mut().poll(cx).is_ready() || timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }
}

/// Batches output so storage sees few, larger writes; blocks are split at 64 KiB.
#[derive(Default)]
struct Batch {
    blocks: Vec<(OutputSource, String)>,
    since: Option<Instant>,
}

impl Batch {
    fn push(&mut self, source: OutputSource, mut text: String) {
        self.since.get_or_insert_with(Instant::now);
        while text.len() > MAX_OUTPUT_BYTES {
            let mut end = MAX_OUTPUT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let rest = text.split_off(end);
            self.blocks.push((source, text));
            text = rest;
        }
        self.blocks.push((source, text));
    }

    fn due(&self) -> Option<Duration> {
        self.since
            .map(|since| FLUSH_INTERVAL.saturating_sub(since.elapsed()))
    }

    async fn flush_if_due(&mut self, sink: &dyn TurnSink) -> Result<(), CodexError> {
        if self.blocks.len() >= FLUSH_BLOCKS || self.due().is_some_and(|d| d.is_zero()) {
            self.flush(sink).await?;
        }
        Ok(())
    }

    async fn flush(&mut self, sink: &dyn TurnSink) -> Result<(), CodexError> {
        self.since = None;
        while !self.blocks.is_empty() {
            let rest = self
                .blocks
                .split_off(self.blocks.len().min(crate::storage::MAX_OUTPUT_BATCH));
            let batch = std::mem::replace(&mut self.blocks, rest);
            sink.output(batch).await.map_err(CodexError::Sink)?;
        }
        Ok(())
    }
}

/// A readable summary of a completed thread item, or None for items not worth storing.
pub fn render_item(item: &Value) -> Option<String> {
    let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
    match item["type"].as_str()? {
        "agentMessage" => Some(text("text")),
        "plan" => Some(format!("Plan: {}", text("text"))),
        "commandExecution" => Some(format!(
            "$ {} (exit {})",
            text("command"),
            item["exitCode"]
                .as_i64()
                .map_or("unknown".into(), |c| c.to_string())
        )),
        "fileChange" => {
            let paths: Vec<&str> = item["changes"]
                .as_array()
                .map(|changes| changes.iter().filter_map(|c| c["path"].as_str()).collect())
                .unwrap_or_default();
            Some(format!("Changed files: {}", paths.join(", ")))
        }
        "mcpToolCall" | "dynamicToolCall" => Some(format!(
            "Tool call {} ({})",
            item["tool"].as_str().unwrap_or_default(),
            item["status"].as_str().unwrap_or_default()
        )),
        "webSearch" => Some(format!("Web search: {}", text("query"))),
        _ => None,
    }
}

/// The execution prompt: the saved issue is the task; the parent is context only.
pub fn prompt(repository: &str, issue: &IssueSnapshot, parent: Option<&IssueSnapshot>) -> String {
    let mut prompt = format!(
        "You are working unattended on GitHub issue #{number} in {repository}. Implement the issue \
         in this working directory. Nobody will answer questions, so make reasonable decisions and \
         finish the work. Do not push, commit, or open pull requests; Nightshift commits and \
         publishes your changes after you finish.\n\n## Task: issue #{number}: {title}\n\n{body}\n",
        number = issue.number,
        title = issue.title,
        body = issue.description,
    );
    if let Some(parent) = parent {
        prompt.push_str(&format!(
            "\n## Context only: parent issue #{}: {}\n\nThis parent describes the broader change. \
             Do not implement the parent or sibling issues; implement only issue #{}.\n\n{}\n",
            parent.number, parent.title, issue.number, parent.description
        ));
    }
    prompt
}

#[cfg(test)]
mod tests;
