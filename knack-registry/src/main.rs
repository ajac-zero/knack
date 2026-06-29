use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
use clap::builder::styling::{AnsiColor, Effects, Styles};

/// Colour palette for clap's --help renderer. Matches the knack CLI so
/// running `--help` on either binary feels like the same toolchain.
const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Blue.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default());
use flate2::{Compression, write::GzEncoder};
use knack_core::{IndexedSkill, RegistryIndex, collect_files, read_skill, validate_skill_metadata};
use serde::Deserialize;
use tar::{Builder, Header};
use tokio::sync::RwLock;

#[derive(Debug, Parser)]
#[command(name = "knack-registry")]
#[command(version, about = "Serve and search a knack registry index")]
#[command(styles = HELP_STYLES)]
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

    /// Directory to persist cloned backing repos across refreshes and restarts.
    /// When set, refreshes do `git fetch + reset` instead of re-cloning, and
    /// archive requests read from the cache instead of cloning per request.
    /// When omitted, a per-process tempdir is used (legacy behaviour; cache is
    /// rebuilt on every restart). On a platform with persistent volumes
    /// (Fly.io, AWS App Runner with EFS, GCP Cloud Run with a volume), point
    /// this at a mounted volume to keep the cache across container restarts.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    /// Combined search index + per-skill location pointer into the
    /// cache. These two move together: a refresh swaps the whole
    /// IndexedState under a single write lock, so any observer sees
    /// either the old (index, locations) pair or the new one — never
    /// a mix where a search hit references a stale cache entry.
    state: Arc<RwLock<IndexedState>>,
    index_path: PathBuf,
    skills_root: Option<PathBuf>,
    name: Option<String>,
    source_aliases: BTreeMap<String, String>,
}

/// What the registry exposes to clients (`index`) plus how to actually
/// produce each skill's tarball without doing more git work (`locations`).
/// Built atomically by `refresh_index_and_cache`.
#[derive(Debug, Default)]
struct IndexedState {
    index: RegistryIndex,
    locations: HashMap<String, SkillLocation>,
}

/// Points at a specific skill inside a cached backing repo. `cached`
/// is shared (Arc) so multiple skills from the same `[[source]]` entry
/// reuse one clone, one refresh lock, and one captured SHA.
#[derive(Debug, Clone)]
struct SkillLocation {
    cached: Arc<CachedSource>,
    /// Path from `cached.repo_dir` to the skill directory. For a
    /// dynamic `[[source]]` entry pointing at a whole repo (no
    /// subpath), this might be e.g. `skills/pdf`. For a static
    /// `[[skill]]` entry whose source already names a specific
    /// skill, this is the same subpath used in the source URL.
    relative: PathBuf,
}

/// A backing repo on disk that can be refreshed in place. The
/// `refresh_lock` serialises in-place `git fetch + reset` against
/// concurrent archive reads — readers (archive serving) take the
/// read lock, the refresh task takes the write lock briefly while
/// it mutates the working tree.
#[derive(Debug)]
struct CachedSource {
    /// Stable directory on disk. We `git fetch + reset --hard` in
    /// place rather than cloning into a new path; that lets us
    /// reuse the cached objects across refreshes (pack-file deltas
    /// instead of full clones) and means archive readers see a
    /// stable path even while refreshes happen.
    repo_dir: PathBuf,
    /// HEAD SHA captured at the last successful refresh, exposed
    /// via the `X-Knack-Resolved-Sha` archive response header.
    sha: tokio::sync::RwLock<Option<String>>,
    refresh_lock: tokio::sync::RwLock<()>,
}

/// Lazy, append-only map from source URL to its cached repo. Entries
/// are created on first access (refresh) and stay until `prune_stale`
/// removes those no longer referenced by the current index.
#[derive(Debug)]
struct SourceCache {
    base_dir: PathBuf,
    /// Held lock-free for reads; only acquired write when registering
    /// a new entry. Once an Arc<CachedSource> is exposed, all
    /// mutation goes through its own refresh_lock.
    entries: std::sync::RwLock<HashMap<String, Arc<CachedSource>>>,
    /// Owned tempdir kept alive for the SourceCache's lifetime so
    /// that, when `--cache-dir` was omitted, the per-process scratch
    /// directory is cleaned up at shutdown rather than leaking.
    _tempdir: Option<tempfile::TempDir>,
}

impl SourceCache {
    fn new(base_dir: PathBuf, tempdir: Option<tempfile::TempDir>) -> Result<Self> {
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create cache dir {}", base_dir.display()))?;
        Ok(Self {
            base_dir,
            entries: std::sync::RwLock::new(HashMap::new()),
            _tempdir: tempdir,
        })
    }

    /// Get an existing entry or register a fresh one. Doesn't touch
    /// the filesystem beyond computing the subdir path — callers
    /// invoke `refresh_cached_source` to actually populate it.
    fn slot(&self, source: &str) -> Arc<CachedSource> {
        if let Some(existing) = self.entries.read().unwrap().get(source) {
            return existing.clone();
        }
        let mut guard = self.entries.write().unwrap();
        if let Some(existing) = guard.get(source) {
            return existing.clone();
        }
        let repo_dir = self.base_dir.join(cache_subdir_name(source));
        let entry = Arc::new(CachedSource {
            repo_dir,
            sha: tokio::sync::RwLock::new(None),
            refresh_lock: tokio::sync::RwLock::new(()),
        });
        guard.insert(source.to_string(), entry.clone());
        entry
    }

    /// Remove cache entries (and their on-disk directories) whose
    /// source URL isn't in `active`. Called at the end of each
    /// refresh pass so an operator removing a `[[source]]` line
    /// doesn't accumulate orphan clones.
    ///
    /// Cleans both the in-memory map (entries the current process
    /// created) AND the on-disk base_dir (subdirs left behind by a
    /// previous run whose `--index` listed sources we no longer
    /// have). The on-disk sweep is what makes a persistent
    /// `--cache-dir` self-healing across config changes.
    fn prune_stale(&self, active: &BTreeSet<String>) {
        let active_subdirs: BTreeSet<String> =
            active.iter().map(|s| cache_subdir_name(s)).collect();

        let mut guard = self.entries.write().unwrap();
        let stale_keys: Vec<String> = guard
            .keys()
            .filter(|key| !active.contains(*key))
            .cloned()
            .collect();
        for key in stale_keys {
            if let Some(entry) = guard.remove(&key) {
                if let Err(err) = std::fs::remove_dir_all(&entry.repo_dir) {
                    eprintln!(
                        "failed to remove stale cache dir {}: {err:#}",
                        entry.repo_dir.display()
                    );
                }
            }
        }
        drop(guard);

        // Sweep on-disk orphans. A previous run with a different
        // [[source]] set leaves subdirs that the in-memory map
        // never knew about; without this sweep they'd persist
        // forever in a long-lived persistent cache.
        match std::fs::read_dir(&self.base_dir) {
            Ok(iter) => {
                for entry in iter.flatten() {
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if active_subdirs.contains(name) {
                        continue;
                    }
                    if let Err(err) = std::fs::remove_dir_all(&path) {
                        eprintln!(
                            "failed to remove orphan cache dir {}: {err:#}",
                            path.display()
                        );
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "failed to scan cache dir {} for orphans: {err:#}",
                    self.base_dir.display()
                );
            }
        }
    }
}

/// Map a source URL onto a filename-safe subdirectory. Keeps the
/// alphanumerics, replaces everything else with `_`. The result is
/// stable across runs so the persistent cache identifies the same
/// source consistently, but it's not collision-free — two sources
/// differing only by punctuation would clash. The chance of that
/// matters less than the legibility of the resulting paths when an
/// operator inspects the cache dir manually.
fn cache_subdir_name(source: &str) -> String {
    source
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' => c,
            _ => '_',
        })
        .collect()
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

    // Either the operator pointed us at a persistent volume (Fly.io
    // / mounted PV / whatever) or we spin up a tempdir that lives
    // for the process. The latter is the Cloudflare-Containers-style
    // shape: cache benefits within a container's lifetime, rebuilt
    // on every cold start.
    let (cache_base, cache_tempdir) = match cli.cache_dir.clone() {
        Some(path) => (path, None),
        None => {
            let tempdir = tempfile::tempdir().context("failed to create cache tempdir")?;
            (tempdir.path().to_path_buf(), Some(tempdir))
        }
    };
    let source_cache = Arc::new(SourceCache::new(cache_base, cache_tempdir)?);

    let initial = refresh_index_and_cache(&cli.index, &source_aliases, &source_cache).await?;
    let state = AppState {
        state: Arc::new(RwLock::new(initial)),
        index_path: cli.index,
        skills_root: cli.skills_root,
        name: cli.name,
        source_aliases,
    };

    if cli.refresh_interval_seconds > 0 {
        spawn_refresh_task(
            state.state.clone(),
            state.index_path.clone(),
            state.source_aliases.clone(),
            source_cache,
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

fn read_index(path: &Path) -> Result<RegistryIndex> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let index: RegistryIndex =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    index.validate()?;
    Ok(index)
}

async fn refresh_index_and_cache(
    path: &Path,
    source_aliases: &BTreeMap<String, String>,
    cache: &SourceCache,
) -> Result<IndexedState> {
    let mut index = read_index(path)?;
    let mut locations: HashMap<String, SkillLocation> = HashMap::new();
    let mut active_sources: BTreeSet<String> = BTreeSet::new();

    // Static [[skill]] entries first so they win name collisions
    // against dynamic walks — an operator that pinned a specific
    // skill by hand presumably did so deliberately.
    for skill in index.skill.clone() {
        active_sources.insert(skill.source.clone());
        let cached = cache.slot(&skill.source);
        if let Err(err) = refresh_cached_source(&cached, &skill.source, source_aliases).await {
            eprintln!(
                "failed to refresh static entry {} from {}: {err:#}",
                skill.name, skill.source
            );
            continue;
        }
        let relative = source_subpath(&skill.source, source_aliases).unwrap_or_default();
        let skill_md = cached.repo_dir.join(&relative).join("SKILL.md");
        if !skill_md.is_file() {
            eprintln!(
                "static skill {} has no SKILL.md at {}",
                skill.name,
                skill_md.display()
            );
            continue;
        }
        locations.insert(
            skill.name.clone(),
            SkillLocation {
                cached: cached.clone(),
                relative,
            },
        );
    }

    let static_skill_names: BTreeSet<String> =
        index.skill.iter().map(|skill| skill.name.clone()).collect();
    let dynamic_sources = index.source.clone();
    for source in dynamic_sources {
        active_sources.insert(source.source.clone());
        let cached = cache.slot(&source.source);
        if let Err(err) = refresh_cached_source(&cached, &source.source, source_aliases).await {
            eprintln!(
                "failed to refresh dynamic source {}: {err:#}",
                source.source
            );
            continue;
        }
        let subpath = source_subpath(&source.source, source_aliases).unwrap_or_default();
        let walk_root = cached.repo_dir.join(&subpath);

        let skill_dirs = match collect_skill_dirs(&walk_root) {
            Ok(dirs) => dirs,
            Err(err) => {
                eprintln!("failed to walk {} for skills: {err:#}", walk_root.display());
                continue;
            }
        };
        for skill_dir in skill_dirs {
            // One malformed SKILL.md inside a multi-skill repo (an
            // un-filled template, a name/dir mismatch, an empty
            // description) used to kill the entire materialize pass
            // and prevent the registry from starting. That's too
            // strict when the operator is pointing at a third-party
            // repo they don't control. Skip the bad skill, surface
            // the reason on stderr, and keep going.
            let skill = match read_skill(&skill_dir) {
                Ok(skill) => skill,
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to read SKILL.md: {err:#}",
                        skill_dir.display()
                    );
                    continue;
                }
            };
            if let Err(err) = validate_skill_metadata(&skill) {
                eprintln!("skipping {}: {err:#}", skill_dir.display());
                continue;
            }
            if static_skill_names.contains(&skill.name)
                || locations.contains_key(&skill.name)
                || index.skill.iter().any(|indexed| indexed.name == skill.name)
            {
                continue;
            }
            let relative_to_walk = skill_dir.strip_prefix(&walk_root).with_context(|| {
                format!(
                    "failed to make {} relative to {}",
                    skill_dir.display(),
                    walk_root.display()
                )
            })?;
            let relative_for_url = relative_to_walk.to_string_lossy().replace('\\', "/");
            let skill_source = if relative_for_url.is_empty() {
                source.source.clone()
            } else {
                format!(
                    "{}/{}",
                    source.source.trim_end_matches('/'),
                    relative_for_url
                )
            };
            let relative_to_repo = subpath.join(relative_to_walk);
            locations.insert(
                skill.name.clone(),
                SkillLocation {
                    cached: cached.clone(),
                    relative: relative_to_repo,
                },
            );
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

    // Drop cache entries (and their on-disk dirs) for sources the
    // operator removed since the last refresh. Bounded growth.
    cache.prune_stale(&active_sources);

    Ok(IndexedState { index, locations })
}

fn spawn_refresh_task(
    state: Arc<RwLock<IndexedState>>,
    index_path: PathBuf,
    source_aliases: BTreeMap<String, String>,
    cache: Arc<SourceCache>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match refresh_index_and_cache(&index_path, &source_aliases, &cache).await {
                Ok(refreshed) => {
                    let mut guard = state.write().await;
                    *guard = refreshed;
                    eprintln!("refreshed knack registry index");
                }
                Err(error) => {
                    eprintln!("failed to refresh knack registry index: {error:#}");
                }
            }
        }
    });
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
    Json(state.state.read().await.index.clone())
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Json<Vec<IndexedSkill>> {
    let guard = state.state.read().await;
    let mut results: Vec<IndexedSkill> =
        guard.index.search(&params.q).into_iter().cloned().collect();
    drop(guard);
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

    let location = {
        let guard = state.state.read().await;
        guard
            .locations
            .get(name)
            .cloned()
            .with_context(|| format!("skill not found: {name}"))?
    };

    // Hold the cached source's refresh-read lock while we tar the
    // skill directory. If a background refresh is in progress for
    // this source, it acquired the corresponding write lock and we
    // wait briefly — that's better than letting the refresh truncate
    // the working tree out from under us mid-archive.
    let _read_guard = location.cached.refresh_lock.read().await;
    let resolved_sha = location.cached.sha.read().await.clone();
    let skill_dir = location.cached.repo_dir.join(&location.relative);
    Ok(SkillArchive {
        bytes: create_skill_archive_from_dir(&skill_dir)?,
        resolved_sha,
    })
}

fn create_skill_archive_from_dir(skill_dir: &Path) -> Result<Vec<u8>> {
    let skill = read_skill(skill_dir)?;
    validate_skill_metadata(&skill)?;

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

/// Decomposed backing-source URL: where to clone from, which ref to
/// pin to, and which subdir within the repo is being targeted (empty
/// when the whole repo is in scope). Same shape for `gh:` and
/// `alias:` sources.
#[derive(Debug)]
struct ParsedSource {
    repo_url: String,
    reference: String,
    subpath: PathBuf,
}

fn parse_source(source: &str, source_aliases: &BTreeMap<String, String>) -> Result<ParsedSource> {
    if let Some(spec) = source.strip_prefix("gh:") {
        let github = parse_github_spec_for_registry(spec)?;
        return Ok(ParsedSource {
            repo_url: format!("https://github.com/{}/{}.git", github.owner, github.repo),
            reference: github.reference,
            subpath: github.skill_path,
        });
    }

    let (alias, rest) = source
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("backing source must be alias:owner/repo[@ref]/path"))?;
    let base_url = source_aliases.get(alias).with_context(|| {
        format!(
            "source alias not configured on registry: {alias} \
             (built-in `gh:` is also accepted for github.com)"
        )
    })?;
    let git = parse_git_host_source(base_url, rest)?;
    Ok(ParsedSource {
        repo_url: git.repo_url,
        reference: git.reference,
        subpath: git.skill_path,
    })
}

/// Returns the subpath component of a backing source (empty if
/// none). Convenience wrapper around `parse_source` for callers
/// that only need to know which subdir of a cached repo to look at.
fn source_subpath(source: &str, source_aliases: &BTreeMap<String, String>) -> Result<PathBuf> {
    Ok(parse_source(source, source_aliases)?.subpath)
}

/// Bring `cached.repo_dir` up to date with the current `<ref>` of
/// `source`. If the cache already has a usable clone, we do
/// `git fetch + git reset --hard FETCH_HEAD` against it — that
/// transfers pack-file deltas, typically a few KB. If no clone
/// exists yet, or an in-place fetch fails (force-push that rewrote
/// history, corrupted cache, etc.), we fall back to a fresh sparse
/// or full clone into the same directory.
///
/// The whole operation is serialised against archive readers via
/// `refresh_lock`. After success, the new HEAD SHA is published
/// under `cached.sha` so the next archive response advertises it
/// in the `X-Knack-Resolved-Sha` header.
async fn refresh_cached_source(
    cached: &CachedSource,
    source: &str,
    source_aliases: &BTreeMap<String, String>,
) -> Result<()> {
    let _write_guard = cached.refresh_lock.write().await;
    let parsed = parse_source(source, source_aliases)?;
    let has_git = cached.repo_dir.join(".git").is_dir();

    if has_git {
        match incremental_fetch(&cached.repo_dir, &parsed.reference) {
            Ok(()) => {}
            Err(err) => {
                eprintln!(
                    "incremental refresh of {source} failed ({err:#}), \
                     rebuilding from scratch"
                );
                if cached.repo_dir.exists() {
                    std::fs::remove_dir_all(&cached.repo_dir).with_context(|| {
                        format!(
                            "failed to remove stale cache dir {}",
                            cached.repo_dir.display()
                        )
                    })?;
                }
                clone_into_cache_dir(&parsed, &cached.repo_dir)?;
            }
        }
    } else {
        // First-time fetch (cache empty for this source) or partial
        // state left over from an aborted previous attempt.
        if cached.repo_dir.exists() {
            std::fs::remove_dir_all(&cached.repo_dir).with_context(|| {
                format!(
                    "failed to remove partial cache dir {}",
                    cached.repo_dir.display()
                )
            })?;
        }
        clone_into_cache_dir(&parsed, &cached.repo_dir)?;
    }

    let sha = capture_git_head_sha(&cached.repo_dir).ok();
    *cached.sha.write().await = sha;
    Ok(())
}

fn incremental_fetch(repo_dir: &Path, reference: &str) -> Result<()> {
    run_git(
        ["fetch", "--depth=1", "origin", reference],
        Some(repo_dir),
        "incremental fetch",
    )?;
    run_git(
        ["reset", "--hard", "FETCH_HEAD"],
        Some(repo_dir),
        "reset to fetched head",
    )?;
    // Drop any unreferenced objects accumulated across refreshes
    // so the cache doesn't grow unboundedly. Best-effort; ignore
    // errors so a transient git failure here doesn't block serving.
    let _ = run_git(
        ["gc", "--auto"],
        Some(repo_dir),
        "auto gc after incremental fetch",
    );
    Ok(())
}

/// Initial population (or rebuild) of a cache entry's working tree.
/// When the source specifies a subpath we use partial+sparse clone
/// (only blobs we'll actually checkout get transferred); for whole-
/// repo sources we use a plain shallow clone. Partial clone needs
/// `uploadpack.allowFilter=true` on the server — GitHub and modern
/// Gitea/GitLab have it. If the host rejects the partial flags we
/// fall back transparently to a full shallow clone.
fn clone_into_cache_dir(parsed: &ParsedSource, repo_dir: &Path) -> Result<()> {
    let subpath = parsed.subpath.to_str().unwrap_or("");
    if !subpath.is_empty() {
        match sparse_clone(&parsed.repo_url, &parsed.reference, subpath, repo_dir) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!(
                    "sparse clone of {} at {} (subpath {subpath}) failed, \
                     falling back to full clone: {err:#}",
                    parsed.repo_url, parsed.reference
                );
                if repo_dir.exists() {
                    std::fs::remove_dir_all(repo_dir).with_context(|| {
                        format!(
                            "failed to remove partial clone at {} before fallback",
                            repo_dir.display()
                        )
                    })?;
                }
            }
        }
    }
    full_clone(&parsed.repo_url, &parsed.reference, repo_dir)
}

fn sparse_clone(repo_url: &str, reference: &str, subpath: &str, repo_dir: &Path) -> Result<()> {
    let repo_dir_str = repo_dir.to_str().unwrap_or_default();
    let action = format!("sparse-clone {repo_url} at ref {reference}");
    run_git(
        [
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            "--depth=1",
            "--branch",
            reference,
            "--sparse",
            repo_url,
            repo_dir_str,
        ],
        None,
        &action,
    )?;
    run_git(
        ["sparse-checkout", "set", subpath],
        Some(repo_dir),
        "configure sparse-checkout pattern",
    )?;
    run_git(
        ["checkout", reference],
        Some(repo_dir),
        "materialize sparse working tree",
    )
}

fn full_clone(repo_url: &str, reference: &str, repo_dir: &Path) -> Result<()> {
    let repo_dir_str = repo_dir.to_str().unwrap_or_default();
    let action = format!("clone {repo_url} at ref {reference}");
    run_git(
        [
            "clone",
            "--depth",
            "1",
            "--branch",
            reference,
            repo_url,
            repo_dir_str,
        ],
        None,
        &action,
    )
}

/// Mirror of the CLI's `parse_github_spec`. Duplicated rather than
/// moved to knack-core so this commit is scoped to just the registry —
/// once we hit a third call site we should hoist the spec types into
/// knack-core (alongside SkillFrontmatter and Lockfile).
/// Parses a `gh:` source like the CLI's parser, but with one
/// difference: an empty skill path is allowed. `[[source]]` entries
/// in `knack.index.toml` point at a whole repo to be walked by
/// `materialize_dynamic_sources`; `[[skill]]` entries point at a
/// specific path inside a repo. We accept both shapes; the caller
/// (materialize vs. archive serving) interprets the resulting path
/// accordingly.
fn parse_github_spec_for_registry(spec: &str) -> Result<GithubSpecLite> {
    let mut parts = spec.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("gh: source must be gh:owner/repo[@ref][/path/to/skill]"))?;
    let repo_with_ref = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("gh: source must include a repository"))?;
    let skill_path = parts.next().unwrap_or("");
    let (repo, reference) = repo_with_ref
        .split_once('@')
        .unwrap_or((repo_with_ref, "main"));
    if repo.is_empty() || reference.is_empty() {
        bail!("gh: source repository and ref must not be empty");
    }
    Ok(GithubSpecLite {
        owner: owner.to_string(),
        repo: repo.to_string(),
        reference: reference.to_string(),
        skill_path: PathBuf::from(skill_path),
    })
}

#[derive(Debug)]
struct GithubSpecLite {
    owner: String,
    repo: String,
    reference: String,
    skill_path: PathBuf,
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
