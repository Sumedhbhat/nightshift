mod ui;

use nightshift::storage::{Error, Store};
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
            let home =
                std::env::var_os("HOME").ok_or(Error::InvalidInput("HOME is unavailable"))?;
            let directory = std::env::var_os("NIGHTSHIFT_DATA_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| default_data_directory(home.into()));
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

#[cfg(target_os = "macos")]
fn default_data_directory(home: std::path::PathBuf) -> std::path::PathBuf {
    home.join("Library/Application Support/Nightshift")
}

#[cfg(not(target_os = "macos"))]
fn default_data_directory(home: std::path::PathBuf) -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|directory| !directory.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("nightshift")
}
