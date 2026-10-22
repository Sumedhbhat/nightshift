//! Line-delimited JSON-RPC as spoken by `codex app-server` over stdio. Messages omit the
//! `jsonrpc` field. The reader task drains output continuously, so a slow consumer applies
//! backpressure through a bounded channel instead of growing memory.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc, oneshot},
};

/// Largest single protocol line accepted. Larger lines are a protocol error.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const EVENT_CAPACITY: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("Codex returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("the Codex connection closed")]
    Closed,
    #[error("Codex did not answer {0} in time")]
    Timeout(String),
    #[error("could not write to Codex: {0}")]
    Write(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Notification {
        method: String,
        params: Value,
    },
    /// A request from the server. It must be answered; Nightshift never leaves one pending.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A line that is not a valid protocol message.
    Malformed(String),
    Stderr(String),
    /// Standard output reached end of file.
    Closed,
    /// The owned process exited, with its status description.
    Exited(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Clone)]
pub struct Connection {
    writer: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
}

impl Connection {
    /// Start reading `reader`; events go to `events`.
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        events: mpsc::Sender<Event>,
    ) -> Self {
        let pending: Pending = Arc::default();
        let reading = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = read_bounded_line(&mut reader, &mut line).await;
                let event = match read {
                    Ok(0) => break,
                    Ok(_) if line.iter().all(u8::is_ascii_whitespace) => continue,
                    Ok(_) => match classify(&line) {
                        Classified::Response { id, result } => {
                            if let Some(sender) = reading.lock().await.remove(&id) {
                                let _ = sender.send(result);
                            }
                            continue;
                        }
                        Classified::Event(event) => event,
                    },
                    Err(message) => Event::Malformed(message),
                };
                let fatal = matches!(event, Event::Malformed(_));
                if events.send(event).await.is_err() || fatal {
                    break;
                }
            }
            for (_, sender) in reading.lock().await.drain() {
                let _ = sender.send(Err(RpcError::Closed));
            }
            let _ = events.send(Event::Closed).await;
        });
        Self {
            writer: Arc::new(Mutex::new(Some(Box::new(writer)))),
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
        }
    }

    async fn write(&self, message: Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(&message).expect("serialize message");
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        let writer = writer.as_mut().ok_or(RpcError::Closed)?;
        writer
            .write_all(&line)
            .await
            .map_err(|e| RpcError::Write(e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| RpcError::Write(e.to_string()))
    }

    /// Send a request and wait for its matching response, in any order relative to others.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if let Err(error) = self
            .write(json!({"id": id, "method": method, "params": params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcError::Timeout(method.to_owned()))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write(json!({"method": method, "params": params}))
            .await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.write(json!({"id": id, "result": result})).await
    }

    pub async fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.write(json!({"id": id, "error": {"code": code, "message": message}}))
            .await
    }

    /// Close standard input, which asks the app server to exit.
    pub async fn close(&self) {
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.shutdown().await;
        }
    }
}

async fn read_bounded_line(
    reader: &mut (impl AsyncBufReadExt + Unpin),
    line: &mut Vec<u8>,
) -> Result<usize, String> {
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .map_err(|e| format!("read failed: {e}"))?;
        if buffer.is_empty() {
            return Ok(line.len());
        }
        let (chunk, done) = match buffer.iter().position(|b| *b == b'\n') {
            Some(end) => (&buffer[..=end], true),
            None => (buffer, false),
        };
        if line.len() + chunk.len() > MAX_LINE_BYTES {
            return Err(format!("protocol line exceeded {MAX_LINE_BYTES} bytes"));
        }
        line.extend_from_slice(chunk);
        let used = chunk.len();
        reader.consume(used);
        if done {
            return Ok(line.len());
        }
    }
}

enum Classified {
    Response {
        id: i64,
        result: Result<Value, RpcError>,
    },
    Event(Event),
}

fn classify(line: &[u8]) -> Classified {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim();
    let Ok(Value::Object(mut message)) = serde_json::from_str::<Value>(trimmed) else {
        return Classified::Event(Event::Malformed(trimmed.chars().take(500).collect()));
    };
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = message.remove("params").unwrap_or(Value::Null);
    match (method, message.remove("id")) {
        (Some(method), Some(id)) => Classified::Event(Event::Request { id, method, params }),
        (Some(method), None) => Classified::Event(Event::Notification { method, params }),
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                return Classified::Event(Event::Malformed(trimmed.chars().take(500).collect()));
            };
            let result = match (message.remove("result"), message.remove("error")) {
                (Some(result), None) => Ok(result),
                (_, Some(error)) => Err(RpcError::Remote {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_owned(),
                }),
                (None, None) => Ok(Value::Null),
            };
            Classified::Response { id, result }
        }
        (None, None) => Classified::Event(Event::Malformed(trimmed.chars().take(500).collect())),
    }
}
