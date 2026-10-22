mod ui;

use nightshift::{
    application::{execution::Services, run},
    codex::CodexCommand,
    github::GitHub,
    platform,
    storage::{Error, Store},
};
use std::sync::{Arc, OnceLock};

fn main() {
    let main_started = std::time::Instant::now();
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("nightshift {}", nightshift::VERSION);
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("create background runtime");
    if let Some(config) = ui::PerfConfig::from_env(main_started) {
        ui::run_perf(runtime.handle().clone(), config);
        runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        return;
    }
    let store = Arc::new(OnceLock::<Store>::new());
    let startup_store = store.clone();
    let orchestrator = Arc::new(OnceLock::<run::Handle>::new());
    let startup_orchestrator = orchestrator.clone();
    let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel();
    let startup = runtime.spawn(async move {
        let result = async {
            let platform = platform::initialize().await;
            let directory = platform
                .data_directory()
                .ok_or(Error::InvalidInput("HOME is unavailable"))?;
            let store = Store::open(directory.clone()).await?;
            // Nothing runs before this: interrupt unfinished work and never resume it automatically.
            store.reconcile_after_restart().await?;
            let projects = store.projects().await?;
            let _ = startup_store.set(store.clone());
            let (updates, receiver) = tokio::sync::mpsc::channel(256);
            let handle = run::spawn(
                Services {
                    store: store.clone(),
                    github: GitHub::from_env(),
                    codex: CodexCommand::from_env(),
                    worktrees_root: directory.join("worktrees"),
                },
                updates,
            );
            let _ = startup_orchestrator.set(handle.clone());
            Ok(ui::Started {
                store,
                projects,
                run: handle,
                updates: receiver,
                environment_warning: platform.warning().map(str::to_owned),
            })
        }
        .await;
        let _ = startup_sender.send(result);
    });
    ui::run(runtime.handle().clone(), startup_receiver, main_started);
    // Initialization owns a worker too. Finish it before closing, even on an immediate quit.
    if let Err(error) = runtime.block_on(startup) {
        eprintln!("nightshift: history initialization task failed: {error}");
    }
    // Quitting stops active executions and records them as interrupted before storage closes.
    if let Some(orchestrator) = orchestrator.get() {
        runtime.block_on(orchestrator.shutdown());
    }
    if let Some(store) = store.get()
        && let Err(error) = runtime.block_on(store.clone().close())
    {
        eprintln!("nightshift: failed to close history: {error}");
    }
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}
