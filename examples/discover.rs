//! Read-only GitHub smoke check. Saved projects go into a temporary database.
use nightshift::{
    github::{GitHub, inspect_checkout},
    storage::Store,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let checkout = std::env::args_os()
        .nth(1)
        .ok_or("Usage: cargo run --example discover -- /path/to/repository")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().into()).await?;
        let remotes = inspect_checkout(checkout.into()).await?;
        if remotes.len() != 1 {
            return Err(
                "Choose a repository with one distinct GitHub remote for this smoke check.".into(),
            );
        }
        let github = GitHub::new()?;
        let project = store
            .add_project(github.verify(remotes.into_iter().next().unwrap()).await?)
            .await?;
        assert_eq!(store.projects().await?, vec![project.clone()]);
        let results = github.issues(&project, false).await;
        println!(
            "{}/{}: {} open issues",
            project.owner,
            project.name,
            results.issues.len()
        );
        for issue in results.issues.iter().take(5) {
            println!("#{} {}", issue.number, issue.title);
        }
        store.close().await?;
        if let Some(error) = results.error {
            return Err(error.into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
