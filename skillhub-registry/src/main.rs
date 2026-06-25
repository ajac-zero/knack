use std::{
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use flate2::{Compression, write::GzEncoder};
use serde::Deserialize;
use skillhub_core::{IndexedSkill, RegistryIndex, collect_files, read_skill, validate_skill};
use tar::{Builder, Header};

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

    /// Optional local root containing skill directories to serve as archives.
    #[arg(long)]
    skills_root: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    index: Arc<RegistryIndex>,
    skills_root: Option<PathBuf>,
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
        skills_root: cli.skills_root,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/index", get(get_index))
        .route("/search", get(search))
        .route("/skills/{name}/archive", get(skill_archive))
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

async fn skill_archive(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    match create_skill_archive(&state, &name) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip"),
                (
                    header::CONTENT_DISPOSITION,
                    &format!("attachment; filename=\"{}.skill.tar.gz\"", name),
                ),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(error) => (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    }
}

fn create_skill_archive(state: &AppState, name: &str) -> Result<Vec<u8>> {
    let skills_root = state
        .skills_root
        .as_ref()
        .context("registry was not started with --skills-root")?;
    let skill_dir = skills_root.join(name);
    let skill = read_skill(&skill_dir)?;
    validate_skill(&skill)?;

    let buffer = Vec::new();
    let encoder = GzEncoder::new(buffer, Compression::default());
    let mut archive = Builder::new(encoder);
    for file in collect_files(&skill_dir)? {
        let relative = file.strip_prefix(&skill_dir).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                file.display(),
                skill_dir.display()
            )
        })?;
        let archive_name = Path::new(&skill.name).join(relative);
        append_file(&mut archive, &file, &archive_name)?;
    }
    archive.finish()?;
    let encoder = archive.into_inner()?;
    Ok(encoder.finish()?)
}

fn append_file(
    archive: &mut Builder<GzEncoder<Vec<u8>>>,
    source: &Path,
    archive_name: &Path,
) -> Result<()> {
    let mut file =
        File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", source.display()))?;
    if !metadata.is_file() {
        bail!("not a file: {}", source.display());
    }

    let mut header = Header::new_gnu();
    header.set_size(metadata.len());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();

    archive
        .append_data(&mut header, archive_name, &mut file)
        .with_context(|| format!("failed to archive {}", source.display()))?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
