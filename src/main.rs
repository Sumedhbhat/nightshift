#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use nightshift::{platform, storage::Store};
use std::sync::{Arc, OnceLock};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("create background runtime");
    let store = Arc::new(OnceLock::<Store>::new());
    let startup_store = store.clone();
    let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel();
    let startup = runtime.spawn(async move {
        let result = async {
            let directory = platform::data_directory()?;
            let store = Store::open(directory).await?;
            let projects = store.projects().await?;
            let _ = startup_store.set(store.clone());
            Ok((store, projects))
        }
        .await;
        let _ = startup_sender.send(result);
    });
    ui::run(runtime.handle().clone(), startup_receiver);
    // Initialization owns a worker too. Finish it before closing, even on an immediate quit.
    if let Err(error) = runtime.block_on(startup) {
        eprintln!("nightshift: history initialization task failed: {error}");
    }
    if let Some(store) = store.get()
        && let Err(error) = runtime.block_on(store.clone().close())
    {
        eprintln!("nightshift: failed to close history: {error}");
    }
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}
