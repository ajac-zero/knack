use std::{
    collections::BTreeMap,
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
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

    /// Optional registry alias to return as install sources, e.g. company.
    #[arg(long)]
    public_alias: Option<String>,

    /// Source alias used to resolve backing sources, e.g. tea=git+ssh://git@gitea.example.com.
    #[arg(long = "source-alias")]
    source_aliases: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    index: Arc<RegistryIndex>,
    skills_root: Option<PathBuf>,
    public_alias: Option<String>,
    source_aliases: BTreeMap<String, String>,
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
        public_alias: cli.public_alias,
        source_aliases: parse_source_aliases(&cli.source_aliases)?,
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
    let mut results: Vec<IndexedSkill> =
        state.index.search(&params.q).into_iter().cloned().collect();
    if let Some(alias) = &state.public_alias {
        for skill in &mut results {
            skill.source = format!("{}:{}", alias, skill.name);
        }
    }
    Json(results)
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
    if let Some(skills_root) = &state.skills_root {
        let skill_dir = skills_root.join(name);
        if skill_dir.join("SKILL.md").is_file() {
            return create_skill_archive_from_dir(&skill_dir);
        }
    }

    let indexed = state
        .index
        .skill
        .iter()
        .find(|skill| skill.name == name)
        .with_context(|| format!("skill not found: {name}"))?;
    let fetched = fetch_backing_source(&indexed.source, state)?;
    create_skill_archive_from_dir(&fetched.path)
}

fn create_skill_archive_from_dir(skill_dir: &Path) -> Result<Vec<u8>> {
    let skill = read_skill(skill_dir)?;
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

#[derive(Debug)]
struct FetchedBackingSource {
    path: PathBuf,
    _temp_dir: tempfile::TempDir,
}

fn fetch_backing_source(source: &str, state: &AppState) -> Result<FetchedBackingSource> {
    let (alias, rest) = source
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("backing source must be alias:owner/repo[@ref]/path"))?;
    let base_url = state
        .source_aliases
        .get(alias)
        .with_context(|| format!("source alias not configured on registry: {alias}"))?;
    let git = parse_git_host_source(base_url, rest)?;

    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let repo_dir = temp_dir.path().join("repo");
    let status = ProcessCommand::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(&git.reference)
        .arg(&git.repo_url)
        .arg(&repo_dir)
        .status()
        .with_context(|| "failed to run git clone; is git installed?")?;

    if !status.success() {
        bail!(
            "git clone failed for backing source {} at ref {}",
            git.repo_url,
            git.reference
        );
    }

    let skill_dir = repo_dir.join(git.skill_path);
    let skill = read_skill(&skill_dir)?;
    validate_skill(&skill)?;
    Ok(FetchedBackingSource {
        path: skill_dir,
        _temp_dir: temp_dir,
    })
}

#[derive(Debug)]
struct GitBackingSource {
    repo_url: String,
    reference: String,
    skill_path: PathBuf,
}

fn parse_git_host_source(base_url: &str, rest: &str) -> Result<GitBackingSource> {
    let mut parts = rest.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .context("backing source must include owner")?;
    let repo_with_ref = parts
        .next()
        .filter(|part| !part.is_empty())
        .context("backing source must include repository")?;
    let skill_path = parts
        .next()
        .filter(|part| !part.is_empty())
        .context("backing source must include skill path")?;
    let (repo, reference) = split_repo_ref(repo_with_ref, "main")?;
    let base_url = base_url
        .trim_end_matches('/')
        .strip_prefix("git+")
        .unwrap_or(base_url.trim_end_matches('/'));

    Ok(GitBackingSource {
        repo_url: format!("{base_url}/{owner}/{repo}.git"),
        reference: reference.to_string(),
        skill_path: PathBuf::from(skill_path),
    })
}

fn split_repo_ref<'a>(repo_with_ref: &'a str, default_ref: &'a str) -> Result<(&'a str, &'a str)> {
    let Some(position) = repo_with_ref.rfind('@') else {
        return Ok((repo_with_ref, default_ref));
    };
    let (repo, reference_with_at) = repo_with_ref.split_at(position);
    let reference = &reference_with_at[1..];
    if repo.is_empty() || reference.is_empty() {
        bail!("repository and ref must not be empty");
    }
    Ok((repo, reference))
}

fn parse_source_aliases(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut aliases = BTreeMap::new();
    for value in values {
        let (name, url) = value
            .split_once('=')
            .with_context(|| format!("source alias must be name=url: {value}"))?;
        if name.is_empty() || url.is_empty() {
            bail!("source alias name and url must not be empty: {value}");
        }
        aliases.insert(name.to_string(), url.to_string());
    }
    Ok(aliases)
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
