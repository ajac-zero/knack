use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use clap::Parser;
use serde::Deserialize;
use skillhub_core::{IndexedSkill, RegistryIndex};

#[derive(Debug, Parser)]
#[command(name = "skillhub-registry")]
#[command(version, about = "Serve and search a Skillhub registry index")]
struct Cli {
    /// Path to a skillhub registry index TOML file.
    #[arg(long, default_value = "skillhub.index.toml")]
    index: PathBuf,

    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:7349")]
    bind: SocketAddr,
}

#[derive(Clone)]
struct AppState {
    index: Arc<RegistryIndex>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let index = read_index(&cli.index)?;
    let state = AppState {
        index: Arc::new(index),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/index", get(get_index))
        .route("/search", get(search))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("failed to bind {}", cli.bind))?;
    println!("skillhub-registry listening on http://{}", cli.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("registry server failed")?;

    Ok(())
}

fn read_index(path: &PathBuf) -> Result<RegistryIndex> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let index: RegistryIndex =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    index.validate()?;
    Ok(index)
}

async fn health() -> &'static str {
    "ok"
}

async fn get_index(State(state): State<AppState>) -> Json<RegistryIndex> {
    Json((*state.index).clone())
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Json<Vec<IndexedSkill>> {
    Json(state.index.search(&params.q).into_iter().cloned().collect())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
