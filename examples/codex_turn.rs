//! Run one Codex turn in a directory with Nightshift's runtime policy and report the outcome.
//! Usage: cargo run --example codex_turn -- /path/to/worktree "prompt"
//! Set NIGHTSHIFT_STOP_AFTER=<seconds> to test an orderly stop.
use nightshift::{
    codex::{Session, TurnRequest, TurnSink},
    domain::OutputSource,
    github::cli::BoxFuture,
};
use std::{path::PathBuf, time::Instant};

struct Print;

impl TurnSink for Print {
    fn thread(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        println!("[thread] {id}");
        Box::pin(async { Ok(()) })
    }
    fn turn(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        println!("[turn] {id}");
        Box::pin(async { Ok(()) })
    }
    fn output(&self, blocks: Vec<(OutputSource, String)>) -> BoxFuture<'_, Result<(), String>> {
        for (source, text) in blocks {
            println!("[{}] {text}", source.as_str());
        }
        Box::pin(async { Ok(()) })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let cwd = PathBuf::from(args.next().ok_or("missing directory")?);
    let prompt = args.next().ok_or("missing prompt")?;
    let program = std::env::var_os("NIGHTSHIFT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| "codex".into());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let started = Instant::now();
        let mut session = Session::spawn(&program, &cwd)?;
        println!("[process] pid {:?}", session.process_id());
        session.handshake().await?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        // NIGHTSHIFT_STOP_AFTER=<seconds> requests an orderly stop, as quitting would.
        if let Some(seconds) = std::env::var("NIGHTSHIFT_STOP_AFTER")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                println!("[stop] requested");
                let _ = stop.send(true);
            });
        }
        let outcome = session
            .run_turn(
                &TurnRequest {
                    cwd: cwd.clone(),
                    prompt,
                    resume_thread: None,
                },
                &Print,
                stopped,
            )
            .await;
        let stopped = session.shutdown(Some(&cwd)).await;
        println!(
            "[outcome] {outcome:?} after {:.1}s; process stopped: {stopped}",
            started.elapsed().as_secs_f64()
        );
        outcome?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
