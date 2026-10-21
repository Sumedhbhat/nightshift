use super::*;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[derive(Default)]
struct Sink {
    threads: Mutex<Vec<String>>,
    turns: Mutex<Vec<String>>,
    output: Mutex<Vec<(OutputSource, String)>>,
    batches: Mutex<usize>,
}

impl TurnSink for Sink {
    fn thread(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        self.threads.lock().unwrap().push(id);
        Box::pin(async { Ok(()) })
    }
    fn turn(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        self.turns.lock().unwrap().push(id);
        Box::pin(async { Ok(()) })
    }
    fn output(&self, blocks: Vec<(OutputSource, String)>) -> BoxFuture<'_, Result<(), String>> {
        *self.batches.lock().unwrap() += 1;
        self.output.lock().unwrap().extend(blocks);
        Box::pin(async { Ok(()) })
    }
}

impl Sink {
    fn text(&self) -> Vec<String> {
        self.output
            .lock()
            .unwrap()
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }
}

/// The server side of an in-memory connection.
struct Server {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
}

impl Server {
    async fn read(&mut self) -> Value {
        let line = self
            .lines
            .next_line()
            .await
            .unwrap()
            .expect("client message");
        serde_json::from_str(&line).unwrap()
    }

    async fn expect(&mut self, method: &str) -> Value {
        let message = self.read().await;
        assert_eq!(message["method"], method, "{message}");
        message
    }

    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).await.unwrap();
    }

    async fn raw(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).await.unwrap();
    }

    /// Answer the handshake and thread/start, returning the turn/start request ID.
    async fn start_turn(&mut self) -> Value {
        let init = self.expect("initialize").await;
        self.send(json!({"id": init["id"], "result": {"userAgent": "fake"}}))
            .await;
        self.expect("initialized").await;
        let thread = self.expect("thread/start").await;
        assert_eq!(thread["params"]["sandbox"], SANDBOX);
        assert_eq!(thread["params"]["approvalPolicy"], APPROVAL_POLICY);
        self.send(json!({"id": thread["id"], "result": {"thread": {"id": "thr_1"}}}))
            .await;
        let turn = self.expect("turn/start").await;
        assert_eq!(turn["params"]["threadId"], "thr_1");
        turn["id"].clone()
    }

    async fn complete(&mut self, status: &str) {
        self.send(json!({"method": "turn/completed", "params": {"threadId": "thr_1",
            "turn": {"id": "turn_1", "status": status, "items": [], "error": {"message": "boom"}}}}))
            .await;
    }
}

fn pair() -> (Session, Server) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    (
        Session::from_streams(client_read, client_write),
        Server {
            lines: BufReader::new(server_read).lines(),
            writer: server_write,
        },
    )
}

fn request() -> TurnRequest {
    TurnRequest {
        cwd: "/tmp/worktree".into(),
        prompt: "Do the task".into(),
        resume_thread: None,
    }
}

async fn run(
    mut session: Session,
    sink: Arc<Sink>,
    stop: watch::Receiver<bool>,
) -> Result<TurnOutcome, CodexError> {
    session.handshake().await?;
    session.run_turn(&request(), sink.as_ref(), stop).await
}

use std::sync::Arc;

fn never_stop() -> watch::Receiver<bool> {
    let (sender, receiver) = watch::channel(false);
    std::mem::forget(sender);
    receiver
}

#[test]
fn handshake_then_turn_completes_with_events_before_the_start_response() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let client = tokio::spawn(run(session, sink.clone(), never_stop()));
        let init = server.expect("initialize").await;
        assert_eq!(init["params"]["clientInfo"]["name"], "nightshift");
        assert!(init.get("jsonrpc").is_none());
        server.send(json!({"id": init["id"], "result": {}})).await;
        server.expect("initialized").await;
        let thread = server.expect("thread/start").await;
        assert_eq!(thread["params"]["cwd"], "/tmp/worktree");
        server
            .send(json!({"id": thread["id"], "result": {"thread": {"id": "thr_1"}}}))
            .await;
        let turn = server.expect("turn/start").await;
        assert_eq!(turn["params"]["input"][0]["text"], "Do the task");
        // Events arrive before the acknowledgement.
        server.send(json!({"method": "turn/started", "params": {"threadId": "thr_1", "turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        server.send(json!({"method": "item/completed", "params": {"threadId": "thr_1", "turnId": "turn_1", "completedAtMs": 1,
            "item": {"type": "agentMessage", "id": "i1", "text": "Implemented it."}}})).await;
        server.send(json!({"id": turn["id"], "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        server.complete("completed").await;
        assert_eq!(client.await.unwrap().unwrap(), TurnOutcome::Completed);
        assert_eq!(*sink.threads.lock().unwrap(), ["thr_1"]);
        assert_eq!(sink.turns.lock().unwrap()[0], "turn_1");
        assert!(sink.text().contains(&"Implemented it.".to_string()));
    });
}

#[test]
fn responses_are_correlated_out_of_order() {
    block_on(async {
        let (session, mut server) = pair();
        let first = {
            let c = session.connection.clone();
            tokio::spawn(async move { c.request("a", json!({}), HANDSHAKE_TIMEOUT).await })
        };
        let a = server.read().await;
        let second = {
            let c = session.connection.clone();
            tokio::spawn(async move { c.request("b", json!({}), HANDSHAKE_TIMEOUT).await })
        };
        let b = server.read().await;
        server
            .send(json!({"id": b["id"], "result": "second"}))
            .await;
        server
            .send(json!({"id": a["id"], "error": {"code": 7, "message": "nope"}}))
            .await;
        assert_eq!(second.await.unwrap().unwrap(), "second");
        assert_eq!(
            first.await.unwrap(),
            Err(RpcError::Remote {
                code: 7,
                message: "nope".into()
            })
        );
        drop(session);
    });
}

#[test]
fn output_bursts_are_kept_in_order_and_batched() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let client = tokio::spawn(run(session, sink.clone(), never_stop()));
        let turn = server.start_turn().await;
        server.send(json!({"id": turn, "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        let mut burst = String::new();
        for i in 0..5_000 {
            burst.push_str(&serde_json::to_string(&json!({"method": "item/commandExecution/outputDelta",
                "params": {"threadId": "thr_1", "turnId": "turn_1", "itemId": "c", "delta": format!("line {i}")}})).unwrap());
            burst.push('\n');
        }
        server.raw(&burst).await;
        server.complete("completed").await;
        assert_eq!(client.await.unwrap().unwrap(), TurnOutcome::Completed);
        let lines: Vec<String> = sink
            .text()
            .into_iter()
            .filter(|t| t.starts_with("line "))
            .collect();
        assert_eq!(lines.len(), 5_000);
        assert!(
            lines
                .iter()
                .enumerate()
                .all(|(i, l)| *l == format!("line {i}"))
        );
        let batches = *sink.batches.lock().unwrap();
        assert!(
            batches < 1_000,
            "output must be batched, got {batches} writes"
        );
    });
}

#[test]
fn terminal_statuses_map_and_other_turns_are_ignored() {
    for (status, expected) in [
        ("completed", TurnOutcome::Completed),
        ("failed", TurnOutcome::Failed("boom".into())),
        ("interrupted", TurnOutcome::Interrupted),
    ] {
        block_on(async {
            let (session, mut server) = pair();
            let sink = Arc::new(Sink::default());
            let client = tokio::spawn(run(session, sink.clone(), never_stop()));
            let turn = server.start_turn().await;
            server.send(json!({"id": turn, "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
            // A late completion for a different turn must not end this one.
            server
                .send(
                    json!({"method": "turn/completed", "params": {"threadId": "thr_1",
                "turn": {"id": "turn_0", "status": "failed", "items": []}}}),
                )
                .await;
            server.complete(status).await;
            assert_eq!(client.await.unwrap().unwrap(), expected, "{status}");
        });
    }
}

#[test]
fn unsupported_server_requests_are_answered_not_left_pending() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let client = tokio::spawn(run(session, sink.clone(), never_stop()));
        let turn = server.start_turn().await;
        server.send(json!({"id": turn, "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        server.send(json!({"id": 900, "method": "item/commandExecution/requestApproval", "params": {"threadId": "thr_1", "turnId": "turn_1", "command": "rm -rf build"}})).await;
        assert_eq!(
            server.read().await,
            json!({"id": 900, "result": {"decision": "decline"}})
        );
        server.send(json!({"id": 901, "method": "item/fileChange/requestApproval", "params": {"threadId": "thr_1", "turnId": "turn_1"}})).await;
        assert_eq!(server.read().await["result"]["decision"], "decline");
        server.send(json!({"id": "s-3", "method": "item/tool/requestUserInput", "params": {"threadId": "thr_1", "turnId": "turn_1"}})).await;
        let refused = server.read().await;
        assert_eq!(refused["id"], "s-3");
        assert_eq!(refused["error"]["code"], UNSUPPORTED);
        server
            .send(json!({"id": 903, "method": "mcpServer/elicitation/request", "params": {}}))
            .await;
        assert_eq!(server.read().await["result"]["action"], "decline");
        server.complete("completed").await;
        assert_eq!(client.await.unwrap().unwrap(), TurnOutcome::Completed);
        let notes = sink.text().join("\n");
        assert!(
            notes.contains("Declined Codex request item/commandExecution/requestApproval"),
            "{notes}"
        );
        assert!(
            notes.contains("Refused unsupported Codex request item/tool/requestUserInput"),
            "{notes}"
        );
    });
}

#[test]
fn abrupt_exit_and_malformed_output_are_errors() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let client = tokio::spawn(run(session, sink.clone(), never_stop()));
        let turn = server.start_turn().await;
        server.send(json!({"id": turn, "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        drop(server);
        assert!(matches!(client.await.unwrap(), Err(CodexError::Exited(_))));

        let (session, mut server) = pair();
        let client = tokio::spawn(run(session, Arc::new(Sink::default()), never_stop()));
        server.start_turn().await;
        server.raw("this is not json\n").await;
        assert!(
            matches!(client.await.unwrap(), Err(CodexError::Protocol(line)) if line.contains("not json"))
        );

        // Exit during the handshake.
        let (session, mut server) = pair();
        let client = tokio::spawn(run(session, Arc::new(Sink::default()), never_stop()));
        server.expect("initialize").await;
        drop(server);
        assert!(matches!(
            client.await.unwrap(),
            Err(CodexError::Handshake(_))
        ));
    });
}

#[test]
fn stop_interrupts_the_turn() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let (stop, stopped) = watch::channel(false);
        let client = tokio::spawn(run(session, sink.clone(), stopped));
        let turn = server.start_turn().await;
        server.send(json!({"id": turn, "result": {"turn": {"id": "turn_1", "status": "inProgress", "items": []}}})).await;
        server.send(json!({"method": "item/completed", "params": {"threadId": "thr_1", "turnId": "turn_1", "completedAtMs": 1,
            "item": {"type": "plan", "id": "p", "text": "step one"}}})).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.send(true).unwrap();
        let interrupt = server.expect("turn/interrupt").await;
        assert_eq!(
            interrupt["params"],
            json!({"threadId": "thr_1", "turnId": "turn_1"})
        );
        server
            .send(json!({"id": interrupt["id"], "result": {}}))
            .await;
        server.complete("interrupted").await;
        assert_eq!(client.await.unwrap().unwrap(), TurnOutcome::Interrupted);
        assert!(sink.text().contains(&"Plan: step one".to_string()));
    });
}

#[test]
fn resume_uses_the_saved_thread() {
    block_on(async {
        let (session, mut server) = pair();
        let sink = Arc::new(Sink::default());
        let client = tokio::spawn(async move {
            let mut session = session;
            session.handshake().await?;
            let mut req = request();
            req.resume_thread = Some("thr_saved".into());
            session.run_turn(&req, sink.as_ref(), never_stop()).await
        });
        let init = server.expect("initialize").await;
        server.send(json!({"id": init["id"], "result": {}})).await;
        server.expect("initialized").await;
        let resume = server.expect("thread/resume").await;
        assert_eq!(resume["params"]["threadId"], "thr_saved");
        server
            .send(json!({"id": resume["id"], "result": {"thread": {"id": "thr_saved"}}}))
            .await;
        let turn = server.expect("turn/start").await;
        assert_eq!(turn["params"]["threadId"], "thr_saved");
        server.send(json!({"id": turn["id"], "result": {"turn": {"id": "turn_2", "status": "inProgress", "items": []}}})).await;
        server.send(json!({"method": "turn/completed", "params": {"threadId": "thr_saved", "turn": {"id": "turn_2", "status": "completed", "items": []}}})).await;
        assert_eq!(client.await.unwrap().unwrap(), TurnOutcome::Completed);
    });
}

#[cfg(unix)]
#[test]
fn owned_process_exit_is_reported_and_shutdown_stops_a_stubborn_process() {
    block_on(async {
        // `Session::spawn` runs `<program> app-server`; with /bin/sh that runs ./app-server.
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("app-server"),
            "read line\necho '{\"id\":1,\"result\":{}}'\nread line\nread line\necho crashed >&2\nexit 3\n",
        )
        .unwrap();
        let mut session = Session::spawn(Path::new("/bin/sh"), directory.path()).unwrap();
        assert!(session.process_id().is_some());
        session.handshake().await.unwrap();
        let sink = Sink::default();
        let error = session
            .run_turn(&request(), &sink, never_stop())
            .await
            .unwrap_err();
        assert!(
            matches!(error, CodexError::Rpc(_) | CodexError::Exited(_)),
            "{error:?}"
        );
        assert!(session.shutdown(None).await);

        // A process that ignores standard input closing and SIGTERM is stopped through its
        // process group. It also starts a command in a new session, as Codex's sandbox does.
        std::fs::write(
            directory.path().join("app-server"),
            "perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' sleep 61 &\ntrap '' TERM\nsleep 60 &\nwait\nsleep 60\n",
        )
        .unwrap();
        let mut stubborn = Session::spawn(Path::new("/bin/sh"), directory.path()).unwrap();
        let pid = stubborn.process_id().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let children = descendants(pid).await;
        assert!(children.len() >= 2, "{children:?}");
        let started = Instant::now();
        assert!(
            stubborn.shutdown(None).await,
            "shutdown must confirm the process stopped"
        );
        assert!(started.elapsed() < Duration::from_secs(20));
        let alive = |pid: u32| {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(!alive(pid), "process {pid} must be gone");
        for child in children {
            assert!(
                !alive(child),
                "descendant {child} must be stopped, including one in its own session"
            );
        }
    });
}

#[test]
fn prompt_separates_the_task_from_parent_context() {
    let issue = IssueSnapshot {
        id: "I".into(),
        number: 7,
        title: "Add login".into(),
        description: "Body seven".into(),
    };
    let parent = IssueSnapshot {
        id: "P".into(),
        number: 1,
        title: "Auth".into(),
        description: "Parent body".into(),
    };
    let text = prompt("owner/repo", &issue, Some(&parent));
    let task = text.find("## Task: issue #7").unwrap();
    let context = text.find("## Context only: parent issue #1").unwrap();
    assert!(task < context);
    assert!(text.contains("Body seven") && text.contains("Parent body"));
    assert!(text.contains("implement only issue #7"));
    assert!(!prompt("owner/repo", &issue, None).contains("Context only"));
}

#[test]
fn items_render_readably() {
    assert_eq!(
        render_item(&json!({"type": "commandExecution", "command": "cargo test", "exitCode": 0}))
            .unwrap(),
        "$ cargo test (exit 0)"
    );
    assert_eq!(
        render_item(
            &json!({"type": "fileChange", "changes": [{"path": "a.rs"}, {"path": "b.rs"}]})
        )
        .unwrap(),
        "Changed files: a.rs, b.rs"
    );
    assert_eq!(render_item(&json!({"type": "reasoning"})), None);
}
