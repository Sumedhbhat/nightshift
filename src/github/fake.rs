//! In-memory GitHub CLI for tests. Responses are keyed by the exact argument list.
use super::cli::{BoxFuture, CommandOutput, Gh, GhResult, Runner};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct FakeGh {
    responses: Mutex<HashMap<String, Vec<GhResult<CommandOutput>>>>,
    prefixes: Mutex<Vec<(String, GhResult<CommandOutput>)>>,
    calls: Mutex<Vec<String>>,
}

pub fn ok(value: serde_json::Value) -> GhResult<CommandOutput> {
    Ok(CommandOutput {
        success: true,
        stdout: serde_json::to_vec(&value).unwrap(),
        stderr: String::new(),
    })
}

pub fn text(stdout: &str) -> GhResult<CommandOutput> {
    Ok(CommandOutput {
        success: true,
        stdout: stdout.as_bytes().to_vec(),
        stderr: String::new(),
    })
}

pub fn fail(stderr: &str) -> GhResult<CommandOutput> {
    Ok(CommandOutput {
        success: false,
        stdout: vec![],
        stderr: stderr.into(),
    })
}

pub const API_HELP: &str = "Flags: --method --paginate --slurp --input --hostname --include";

pub fn auth_status(login: &str) -> serde_json::Value {
    serde_json::json!({"hosts": {"github.com": [
        {"state": "success", "active": true, "host": "github.com", "login": login, "tokenSource": "keyring"}
    ]}})
}

impl FakeGh {
    /// An installed, compatible CLI signed in as `login`.
    pub fn signed_in(login: &str) -> Arc<Self> {
        let fake = Arc::new(Self::default());
        fake.respond("--version", text("gh version 2.102.0 (2026-09-30)\n"));
        fake.respond("api --help", text(API_HELP));
        fake.respond(
            "auth status --hostname github.com --json hosts",
            ok(auth_status(login)),
        );
        fake
    }

    /// Queue a response. The last queued response for a key repeats.
    pub fn respond(&self, args: &str, response: GhResult<CommandOutput>) {
        let mut responses = self.responses.lock().unwrap();
        let queue = responses.entry(args.to_owned()).or_default();
        queue.push(response);
    }

    /// Replace every queued response for a key.
    pub fn set(&self, args: &str, response: GhResult<CommandOutput>) {
        self.responses
            .lock()
            .unwrap()
            .insert(args.to_owned(), vec![response]);
    }

    /// Answer any call whose arguments start with `prefix` and have no exact response.
    pub fn respond_prefix(&self, prefix: &str, response: GhResult<CommandOutput>) {
        self.prefixes
            .lock()
            .unwrap()
            .push((prefix.to_owned(), response));
    }

    pub fn get(&self, path: &str, value: serde_json::Value) {
        self.respond(
            &format!("api --hostname github.com --method GET {path}"),
            ok(value),
        );
    }

    pub fn get_all(&self, path: &str, pages: serde_json::Value) {
        self.respond(
            &format!("api --hostname github.com --method GET --paginate --slurp {path}"),
            ok(pages),
        );
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn gh(self: &Arc<Self>) -> Gh {
        Gh::new(self.clone())
    }
}

impl Runner for FakeGh {
    fn run<'a>(
        &'a self,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> BoxFuture<'a, GhResult<CommandOutput>> {
        let key = args.join(" ");
        let mut call = key.clone();
        if let Some(stdin) = stdin {
            call.push_str(" <<< ");
            call.push_str(&String::from_utf8_lossy(&stdin));
        }
        self.calls.lock().unwrap().push(call);
        let response = {
            let mut responses = self.responses.lock().unwrap();
            match responses.get_mut(&key) {
                Some(queue) if queue.len() > 1 => queue.remove(0),
                Some(queue) if queue.len() == 1 => queue[0].clone(),
                _ => match self
                    .prefixes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(prefix, _)| key.starts_with(prefix.as_str()))
                {
                    Some((_, response)) => response.clone(),
                    None => fail(&format!(
                        "gh: Not Found (HTTP 404) [fake has no response for `{key}`]"
                    )),
                },
            }
        };
        Box::pin(async move { response })
    }
}
