use std::{
    collections::BTreeMap,
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::Arc,
    time::Duration,
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
use knack_core::{IndexedSkill, RegistryIndex, collect_files, read_skill, validate_skill};
use serde::Deserialize;
use tar::{Builder, Header};
use tokio::sync::RwLock;

#[derive(Debug, Parser)]
#[command(name = "knack-registry")]
#[command(version, about = "Serve and search a knack registry index")]
struct Cli {
    /// Path to a knack registry index TOML file.
    #[arg(long, default_value = "knack.index.toml")]
    index: PathBuf,

    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:7349")]
    bind: SocketAddr,

    /// Optional local root containing skill directories to serve as archives.
    #[arg(long)]
    skills_root: Option<PathBuf>,

    /// Optional name this registry advertises to clients. When set, the
    /// `/info` endpoint returns it and the `/search` endpoint rewrites
    /// install sources as `<name>:<skill>`. Clients that omit the name
    /// argument to `knack registry add` adopt this value automatically.
    #[arg(long)]
    name: Option<String>,

    /// Source alias used to resolve backing sources, e.g. tea=git+ssh://git@gitea.example.com.
    #[arg(long = "source-alias")]
    source_aliases: Vec<String>,

    /// Periodically refresh dynamic sources. Set to 0 to disable background refresh.
    #[arg(long, default_value_t = 300)]
    refresh_interval_seconds: u64,
}

#[derive(Clone)]
struct AppState {
    index: Arc<RwLock<RegistryIndex>>,
    index_path: PathBuf,
    skills_root: Option<PathBuf>,
    name: Option<String>,
    source_aliases: BTreeMap<String, String>,
}

/// Payload returned by GET /info so clients can self-configure on
/// `knack registry add <url>` without having to be told the name out of
/// band. `name` is null when the registry wasn't started with `--name`.
#[derive(serde::Serialize)]
struct RegistryInfo {
    name: Option<String>,
    version: &'static str,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let source_aliases = parse_source_aliases(&cli.source_aliases)?;
    let index = refresh_index(&cli.index, &source_aliases)?;
    let state = AppState {
        index: Arc::new(RwLock::new(index)),
        index_path: cli.index,
        skills_root: cli.skills_root,
        name: cli.name,
        source_aliases,
    };

    if cli.refresh_interval_seconds > 0 {
        spawn_refresh_task(
            state.index.clone(),
            state.index_path.clone(),
            state.source_aliases.clone(),
            Duration::from_secs(cli.refresh_interval_seconds),
        );
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/index", get(get_index))
        .route("/search", get(search))
        .route("/skills/{name}/archive", get(skill_archive))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("failed to bind {}", cli.bind))?;
    println!("knack-registry listening on http://{}", cli.bind);
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

fn refresh_index(
    path: &PathBuf,
    source_aliases: &BTreeMap<String, String>,
) -> Result<RegistryIndex> {
    let mut index = read_index(path)?;
    materialize_dynamic_sources(&mut index, source_aliases)?;
    Ok(index)
}

fn spawn_refresh_task(
    index: Arc<RwLock<RegistryIndex>>,
    index_path: PathBuf,
    source_aliases: BTreeMap<String, String>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match refresh_index(&index_path, &source_aliases) {
                Ok(refreshed) => {
                    let mut index = index.write().await;
                    *index = refreshed;
                    eprintln!("refreshed knack registry index");
                }
                Err(error) => {
                    eprintln!("failed to refresh knack registry index: {error:#}");
                }
            }
        }
    });
}

fn materialize_dynamic_sources(
    index: &mut RegistryIndex,
    source_aliases: &BTreeMap<String, String>,
) -> Result<()> {
    let static_skill_names: Vec<String> =
        index.skill.iter().map(|skill| skill.name.clone()).collect();
    let dynamic_sources = index.source.clone();
    for source in dynamic_sources {
        let fetched = fetch_source_root(&source.source, source_aliases)?;
        for skill_dir in collect_skill_dirs(&fetched.path)? {
            let skill = read_skill(&skill_dir)?;
            validate_skill(&skill)?;
            if static_skill_names.iter().any(|name| name == &skill.name)
                || index.skill.iter().any(|indexed| indexed.name == skill.name)
            {
                continue;
            }
            let relative = skill_dir.strip_prefix(&fetched.path).with_context(|| {
                format!(
                    "failed to make {} relative to {}",
                    skill_dir.display(),
                    fetched.path.display()
                )
            })?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            let skill_source = if relative.is_empty() {
                source.source.clone()
            } else {
                format!("{}/{}", source.source.trim_end_matches('/'), relative)
            };
            index.skill.push(IndexedSkill {
                name: skill.name,
                description: skill.description,
                source: skill_source,
                tags: source.tags.clone(),
            });
        }
    }
    index
        .skill
        .sort_by(|left, right| left.name.cmp(&right.name));
    index.validate()?;
    Ok(())
}

fn collect_skill_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut skills = Vec::new();
    collect_skill_dirs_inner(root, &mut skills)?;
    skills.sort();
    Ok(skills)
}

fn collect_skill_dirs_inner(path: &Path, skills: &mut Vec<PathBuf>) -> Result<()> {
    if path.join("SKILL.md").is_file() {
        skills.push(path.to_path_buf());
        return Ok(());
    }

    for entry in
        std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !is_ignored_scan_dir(&path) {
            collect_skill_dirs_inner(&path, skills)?;
        }
    }

    Ok(())
}

fn is_ignored_scan_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "node_modules"))
}

async fn health() -> &'static str {
    "ok"
}

async fn info(State(state): State<AppState>) -> Json<RegistryInfo> {
    Json(RegistryInfo {
        name: state.name.clone(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn get_index(State(state): State<AppState>) -> Json<RegistryIndex> {
    Json(state.index.read().await.clone())
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Json<Vec<IndexedSkill>> {
    let index = state.index.read().await;
    let mut results: Vec<IndexedSkill> = index.search(&params.q).into_iter().cloned().collect();
    drop(index);
    if let Some(name) = &state.name {
        for skill in &mut results {
            skill.source = format!("{}:{}", name, skill.name);
        }
    }
    Json(results)
}

async fn skill_archive(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    match create_skill_archive(&state, &name).await {
        Ok(archive) => {
            let disposition = format!("attachment; filename=\"{name}.skill.tar.gz\"");
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/gzip"),
            );
            if let Ok(value) = axum::http::HeaderValue::from_str(&disposition) {
                headers.insert(header::CONTENT_DISPOSITION, value);
            }
            if let Some(sha) = archive.resolved_sha {
                // Clients (knack CLI) use this to pin their lockfile's
                // `resolved` field. Header is omitted when the backing
                // source has no SHA (local skills_root, archives we
                // couldn't rev-parse).
                if let Ok(value) = axum::http::HeaderValue::from_str(&sha) {
                    headers.insert(
                        axum::http::HeaderName::from_static("x-knack-resolved-sha"),
                        value,
                    );
                }
            }
            (headers, Body::from(archive.bytes)).into_response()
        }
        Err(error) => (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    }
}

struct SkillArchive {
    bytes: Vec<u8>,
    resolved_sha: Option<String>,
}

async fn create_skill_archive(state: &AppState, name: &str) -> Result<SkillArchive> {
    if let Some(skills_root) = &state.skills_root {
        let skill_dir = skills_root.join(name);
        if skill_dir.join("SKILL.md").is_file() {
            // Local skills_root has no upstream git history to capture,
            // so no SHA. Clients fall back to checksum-based change
            // detection for these.
            return Ok(SkillArchive {
                bytes: create_skill_archive_from_dir(&skill_dir)?,
                resolved_sha: None,
            });
        }
    }

    let index = state.index.read().await;
    let source = index
        .skill
        .iter()
        .find(|skill| skill.name == name)
        .map(|skill| skill.source.clone())
        .with_context(|| format!("skill not found: {name}"))?;
    drop(index);
    let fetched = fetch_backing_source(&source, state)?;
    Ok(SkillArchive {
        bytes: create_skill_archive_from_dir(&fetched.path)?,
        resolved_sha: fetched.resolved_sha,
    })
}

fn create_skill_archive_from_dir(skill_dir: &Path) -> Result<Vec<u8>> {
    let skill = read_skill(skill_dir)?;
    validate_skill(&skill)?;

    let buffer = Vec::new();
    let encoder = GzEncoder::new(buffer, Compression::default());
    let mut archive = Builder::new(encoder);
    for file in collect_files(skill_dir)? {
        let relative = file.strip_prefix(skill_dir).with_context(|| {
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
    /// Commit SHA captured from `git rev-parse HEAD` in the cloned
    /// backing repo. Surfaced to clients via the X-Knack-Resolved-Sha
    /// response header so they can pin their lockfiles to specific
    /// content. None when capture fails (treated as 'no SHA available').
    resolved_sha: Option<String>,
}

fn fetch_backing_source(source: &str, state: &AppState) -> Result<FetchedBackingSource> {
    fetch_source_root(source, &state.source_aliases).and_then(|fetched| {
        let skill = read_skill(&fetched.path)?;
        validate_skill(&skill)?;
        Ok(fetched)
    })
}

fn fetch_source_root(
    source: &str,
    source_aliases: &BTreeMap<String, String>,
) -> Result<FetchedBackingSource> {
    let (alias, rest) = source
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("backing source must be alias:owner/repo[@ref]/path"))?;
    let base_url = source_aliases
        .get(alias)
        .with_context(|| format!("source alias not configured on registry: {alias}"))?;
    let git = parse_git_host_source(base_url, rest)?;

    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let repo_dir = temp_dir.path().join("repo");
    let repo_dir_str = repo_dir.to_str().unwrap_or_default();
    let action = format!("clone {} at ref {}", git.repo_url, git.reference);
    run_git(
        [
            "clone",
            "--depth",
            "1",
            "--branch",
            &git.reference,
            &git.repo_url,
            repo_dir_str,
        ],
        None,
        &action,
    )?;

    let resolved_sha = capture_git_head_sha(&repo_dir).ok();
    let skill_dir = repo_dir.join(git.skill_path);
    Ok(FetchedBackingSource {
        path: skill_dir,
        _temp_dir: temp_dir,
        resolved_sha,
    })
}

/// Run `git rev-parse HEAD` in `repo_dir` and return the full 40-char
/// SHA. Mirrors the CLI's helper of the same name. Returns Err on any
/// failure; callers treat that as 'no SHA available'.
fn capture_git_head_sha(repo_dir: &Path) -> Result<String> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .context("failed to invoke git rev-parse HEAD")?;
    if !output.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    let sha = String::from_utf8(output.stdout)
        .context("git rev-parse HEAD returned non-UTF-8")?
        .trim()
        .to_string();
    if !looks_like_sha(&sha) {
        bail!("git rev-parse HEAD returned a non-SHA-shaped value: {sha}");
    }
    Ok(sha)
}

/// Heuristic SHA detector — 7 to 40 ASCII hex chars. Used as a sanity
/// check on git's output. Tags and branches don't match and never will.
fn looks_like_sha(s: &str) -> bool {
    matches!(s.len(), 7..=40) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Run git with stdout+stderr captured so the registry's logs aren't
/// polluted with git's progress output on every archive request. On
/// failure, attach the captured stderr to the error so operators still
/// see what git was trying to say. Mirrors the CLI's run_git helper.
fn run_git<'a>(
    args: impl IntoIterator<Item = &'a str>,
    cwd: Option<&Path>,
    action: &str,
) -> Result<()> {
    let mut command = ProcessCommand::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run git for {action}; is git installed?"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            bail!("git failed to {action}");
        }
        bail!("git failed to {action}: {detail}");
    }
    Ok(())
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
    let skill_path = parts.next().unwrap_or("");
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
