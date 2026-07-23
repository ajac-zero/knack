use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Args, Parser, Subcommand};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};

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
use knack_core::{
    IndexedSkill, RegistryIndex, checksum_dir,
    create_skill_archive as create_skill_archive_from_dir, read_skill, unpack_skill_archive,
    validate_skill, validate_skill_metadata, validate_skill_name,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Parser)]
#[command(name = "knack-registry")]
#[command(version, about = "Serve and search a knack registry index")]
#[command(styles = HELP_STYLES)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Materialise the index once and write a static snapshot suitable for
    /// hosting on Cloudflare R2, S3, GCS, or any plain static file host.
    /// The output contains everything a knack CLI client needs to install
    /// from the registry, with no live server required.
    BuildStatic(BuildStaticArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Optional path to a knack registry index TOML file. With --database-url,
    /// omission creates a database-only registry. Otherwise defaults to
    /// knack.index.toml.
    #[arg(long)]
    index: Option<PathBuf>,

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

    /// Directory where skills published via `PUT /skills/{ns}/{name}` are
    /// stored. Unlike --cache-dir (rebuildable scratch), this holds canonical
    /// data: put it on a persistent volume. For multiple replicas, use
    /// --database-url instead. Publishing also requires a publish token.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// PostgreSQL URL for horizontally scaled live publishing. Published
    /// metadata and archives are stored in Postgres so every replica sees
    /// writes immediately. Mutually exclusive with --data-dir.
    #[arg(long, env = "KNACK_DATABASE_URL")]
    database_url: Option<String>,

    /// Bearer token that authorises publishing. Repeatable to allow several
    /// tokens (e.g. one per team, or old+new during rotation). Prefer
    /// --publish-tokens-file where the process list is visible to others.
    #[arg(long = "publish-token")]
    publish_tokens: Vec<String>,

    /// File containing publish tokens, one per line. Blank lines and lines
    /// starting with '#' are ignored. Combined with any --publish-token flags.
    #[arg(long)]
    publish_tokens_file: Option<PathBuf>,

    /// OIDC issuer accepted for self-service human publishing. Must be used
    /// with --oidc-audience, --oidc-client-id, and --database-url.
    #[arg(long, env = "KNACK_OIDC_ISSUER")]
    oidc_issuer: Option<String>,

    /// Audience required in OIDC access tokens presented to this registry.
    #[arg(long, env = "KNACK_OIDC_AUDIENCE")]
    oidc_audience: Option<String>,

    /// Public OAuth client ID used by the knack CLI authorization-code flow.
    #[arg(long, env = "KNACK_OIDC_CLIENT_ID")]
    oidc_client_id: Option<String>,

    /// Optional access-token scope required for direct publishing. When
    /// omitted, a valid token for the configured audience may publish.
    #[arg(long, env = "KNACK_OIDC_REQUIRED_SCOPE")]
    oidc_required_scope: Option<String>,

    /// Claim used as the candidate for an automatically assigned personal
    /// namespace. The stable identity remains the token's issuer + subject.
    #[arg(
        long,
        env = "KNACK_OIDC_NAMESPACE_CLAIM",
        default_value = "preferred_username"
    )]
    oidc_namespace_claim: String,

    /// OAuth scopes advertised to `knack auth login`.
    #[arg(
        long,
        env = "KNACK_OIDC_SCOPES",
        value_delimiter = ',',
        default_value = "openid,offline_access"
    )]
    oidc_scopes: Vec<String>,

    /// Maximum accepted size in bytes for a published skill archive.
    #[arg(long, default_value_t = 50 * 1024 * 1024)]
    publish_max_bytes: usize,
}

#[derive(Debug, Args)]
struct BuildStaticArgs {
    /// Path to a knack registry index TOML file.
    #[arg(long, default_value = "knack.index.toml")]
    index: PathBuf,

    /// Output directory. Existing contents under `skills/` are replaced;
    /// the directory itself is created if needed. After a successful
    /// build, this directory contains `info.json`, `index.json`,
    /// `sha-map.json`, and `skills/<name>.skill.tar.gz` per indexed
    /// skill. Upload it as-is to your static host.
    #[arg(long)]
    output: PathBuf,

    /// Optional registry name written into `info.json` (`{"name": ...}`)
    /// and used to rewrite `index.json` entries' `source` field as
    /// `<name>:<skill>` so clients install via `knack add <name>:<skill>`.
    #[arg(long)]
    name: Option<String>,

    /// Source alias used to resolve backing sources, e.g. tea=git+ssh://git@gitea.example.com.
    #[arg(long = "source-alias")]
    source_aliases: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    /// Combined search index + per-skill location pointer into the
    /// cache. These two move together: a refresh swaps the whole
    /// IndexedState under a single write lock, so any observer sees
    /// either the old (index, locations) pair or the new one — never
    /// a mix where a search hit references a stale cache entry.
    state: Arc<RwLock<IndexedState>>,
    skills_root: Option<PathBuf>,
    name: Option<String>,
    /// Present iff publishing is enabled (a storage backend + tokens). None
    /// keeps the server read-only — the same surface a static
    /// snapshot offers, which is the point: write capability is what
    /// distinguishes a live registry from a baked one.
    uploads: Option<PublishStore>,
    oidc: Option<Arc<OidcValidator>>,
}

#[derive(Clone)]
enum PublishStore {
    Filesystem(Arc<UploadStore>),
    Postgres(Arc<PostgresUploadStore>),
}

impl PublishStore {
    fn tokens(&self) -> &[String] {
        match self {
            Self::Filesystem(store) => &store.tokens,
            Self::Postgres(store) => &store.tokens,
        }
    }
}

struct PostgresUploadStore {
    pool: PgPool,
    tokens: Vec<String>,
}

struct OidcValidator {
    issuer: String,
    audience: String,
    client_id: String,
    required_scope: Option<String>,
    namespace_claim: String,
    scopes: Vec<String>,
    jwks_uri: String,
    client: reqwest::Client,
    jwks: RwLock<CachedJwks>,
    refresh_lock: Mutex<()>,
}

struct CachedJwks {
    set: JwkSet,
    fetched_at: Instant,
}

#[derive(Debug, Deserialize)]
struct OidcDiscovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct AccessTokenClaims {
    iss: String,
    sub: String,
    exp: u64,
    #[serde(default)]
    iat: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scp: Vec<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug)]
struct OidcPrincipal {
    issuer: String,
    subject: String,
    namespace_candidate: String,
}

enum PublishPrincipal {
    ServiceToken,
    Oidc(OidcPrincipal),
}

/// On-disk store for skills accepted via `PUT /skills/{ns}/{name}`.
/// Uploaded skills live at `<data-dir>/skills/<namespace>/<name>/` as
/// plain directories — the same shape every other part of the registry
/// consumes — and are walked back into the index on each refresh, so
/// they survive restarts without any database.
#[derive(Debug)]
struct UploadStore {
    /// `<data-dir>/skills`. Namespace directories live directly below.
    root: PathBuf,
    /// Synthetic cache entry covering the whole upload tree. Reuses
    /// the CachedSource concurrency discipline: archive reads and the
    /// refresh walk take `refresh_lock.read()`, a publish swapping a
    /// skill directory takes `write()`. `sha` stays None — uploads
    /// have no git provenance, clients fall back to checksum-based
    /// change detection.
    cached: Arc<CachedSource>,
    /// Accepted `Authorization: Bearer` values. Non-empty by
    /// construction (see build_upload_store).
    tokens: Vec<String>,
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
    /// True when this skill came from the upload store rather than a
    /// git-backed source. The publish endpoint uses this to decide
    /// between "overwrite the previous upload" (allowed) and "shadow
    /// a git-backed skill" (409 — the git source is config-managed
    /// and an upload silently masking it would be undebuggable).
    from_upload: bool,
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
/// `publish` advertises whether `PUT /skills/{ns}/{name}` is enabled —
/// false for read-only live servers and for static snapshots (whose
/// baked info.json predates or omits the field).
#[derive(Serialize)]
struct RegistryInfo {
    name: Option<String>,
    version: &'static str,
    publish: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    authentication: Option<RegistryAuthentication>,
}

#[derive(Clone, Serialize)]
struct RegistryAuthentication {
    r#type: &'static str,
    issuer: String,
    client_id: String,
    audience: String,
    scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::BuildStatic(args)) => build_static(args).await,
        None => serve(cli.serve).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    validate_oidc_args(&args)?;
    let index_path = args.index.clone().or_else(|| {
        args.database_url
            .is_none()
            .then(|| PathBuf::from("knack.index.toml"))
    });
    let source_aliases = parse_source_aliases(&args.source_aliases)?;
    let publish_tokens =
        load_publish_tokens(&args.publish_tokens, args.publish_tokens_file.as_deref())?;
    let has_publish_tokens = !publish_tokens.is_empty();
    let uploads = build_publish_store(
        args.data_dir.clone(),
        args.database_url.as_deref(),
        publish_tokens,
    )
    .await?;
    let oidc = build_oidc_validator(&args).await?;
    if matches!(uploads, Some(PublishStore::Postgres(_))) && !has_publish_tokens && oidc.is_none() {
        bail!("--database-url requires --publish-token or OIDC configuration");
    }

    let source_cache = if index_path.is_some() {
        // Either the operator pointed us at a persistent volume or we spin up
        // per-process scratch. Database-only deployments need no Git cache.
        let (cache_base, cache_tempdir) = match args.cache_dir.clone() {
            Some(path) => (path, None),
            None => {
                let tempdir = tempfile::tempdir().context("failed to create cache tempdir")?;
                (tempdir.path().to_path_buf(), Some(tempdir))
            }
        };
        Some(Arc::new(SourceCache::new(cache_base, cache_tempdir)?))
    } else {
        None
    };

    let initial = match (index_path.as_deref(), source_cache.as_deref()) {
        (Some(index_path), Some(source_cache)) => {
            refresh_index_and_cache(
                index_path,
                &source_aliases,
                source_cache,
                filesystem_uploads(uploads.as_ref()).map(AsRef::as_ref),
            )
            .await?
        }
        (None, None) => IndexedState::default(),
        _ => unreachable!("index path and source cache are created together"),
    };
    let state = AppState {
        state: Arc::new(RwLock::new(initial)),
        skills_root: args.skills_root,
        name: args.name,
        uploads,
        oidc,
    };

    if args.refresh_interval_seconds > 0
        && let (Some(index_path), Some(source_cache)) = (index_path, source_cache)
    {
        spawn_refresh_task(
            state.state.clone(),
            index_path,
            source_aliases,
            source_cache,
            filesystem_uploads(state.uploads.as_ref()).cloned(),
            Duration::from_secs(args.refresh_interval_seconds),
        );
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/index", get(get_index))
        .route("/search", get(search))
        // Namespaced route — canonical for namespacing-aware clients
        // ("knack add public:anthropics/pdf"). Direct (namespace, name)
        // lookup, no ambiguity.
        .route(
            "/skills/{namespace}/{name}/archive",
            get(skill_archive_namespaced),
        )
        // Legacy single-segment route — kept for backward compat with
        // pre-namespacing clients (`knack add public:pdf`). Soft-
        // resolves: 200 with X-Knack-Namespace when exactly one
        // namespaced entry matches the bare name, 409 with a
        // disambiguation hint when several do, 404 otherwise.
        .route("/skills/{name}/archive", get(skill_archive_legacy))
        // OIDC personal publishing omits the namespace. The registry derives
        // and persistently assigns it from the authenticated identity.
        .route("/skills/{name}", put(publish_personal))
        // Publish endpoint. Accepts the exact tarball `knack pack`
        // produces. Returns 403 until the operator opts in with
        // a publish backend + token; this is the live server's
        // key capability over a static snapshot.
        .route("/skills/{namespace}/{name}", put(publish))
        // Raise axum's 2 MB default body cap for skill uploads. GET
        // routes carry no body, so the wider limit is inert there.
        .layer(DefaultBodyLimit::max(args.publish_max_bytes))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    println!("knack-registry listening on http://{}", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("registry server failed")?;

    Ok(())
}

/// Merge `--publish-token` flags with the lines of `--publish-tokens-file`.
/// Blank lines and `#` comments in the file are skipped so it can be a
/// plain hand-maintained list.
fn load_publish_tokens(flags: &[String], file: Option<&Path>) -> Result<Vec<String>> {
    let mut tokens: Vec<String> = flags.to_vec();
    if let Some(path) = file {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read publish tokens file {}", path.display()))?;
        tokens.extend(
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(String::from),
        );
    }
    if tokens.iter().any(|token| token.is_empty()) {
        bail!("publish tokens must not be empty strings");
    }
    Ok(tokens)
}

fn validate_oidc_args(args: &ServeArgs) -> Result<()> {
    let configured = [
        args.oidc_issuer.is_some(),
        args.oidc_audience.is_some(),
        args.oidc_client_id.is_some(),
    ];
    if configured.iter().any(|value| *value) && !configured.iter().all(|value| *value) {
        bail!("--oidc-issuer, --oidc-audience, and --oidc-client-id must be configured together");
    }
    if args.oidc_issuer.is_some() && args.database_url.is_none() {
        bail!("OIDC publishing requires --database-url");
    }
    Ok(())
}

async fn build_oidc_validator(args: &ServeArgs) -> Result<Option<Arc<OidcValidator>>> {
    let Some(issuer) = args.oidc_issuer.as_deref() else {
        return Ok(None);
    };
    let issuer = issuer.trim_end_matches('/').to_string();
    if !issuer.starts_with("https://") {
        bail!("--oidc-issuer must use HTTPS");
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build OIDC HTTP client")?;
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let discovery: OidcDiscovery = client
        .get(&discovery_url)
        .send()
        .await
        .context("failed to fetch OIDC discovery document")?
        .error_for_status()
        .context("OIDC discovery endpoint returned an error")?
        .json()
        .await
        .context("failed to parse OIDC discovery document")?;
    if discovery.issuer.trim_end_matches('/') != issuer {
        bail!("OIDC discovery issuer does not match --oidc-issuer");
    }
    for endpoint in [
        &discovery.authorization_endpoint,
        &discovery.token_endpoint,
        &discovery.jwks_uri,
    ] {
        if !endpoint.starts_with("https://") {
            bail!("OIDC provider endpoints must use HTTPS");
        }
    }
    let set = fetch_jwks(&client, &discovery.jwks_uri).await?;
    Ok(Some(Arc::new(OidcValidator {
        issuer,
        audience: args.oidc_audience.clone().expect("validated OIDC audience"),
        client_id: args
            .oidc_client_id
            .clone()
            .expect("validated OIDC client ID"),
        required_scope: args.oidc_required_scope.clone(),
        namespace_claim: args.oidc_namespace_claim.clone(),
        scopes: {
            let mut scopes = args.oidc_scopes.clone();
            if let Some(required_scope) = &args.oidc_required_scope
                && !scopes.contains(required_scope)
            {
                scopes.push(required_scope.clone());
            }
            scopes
        },
        jwks_uri: discovery.jwks_uri,
        client,
        jwks: RwLock::new(CachedJwks {
            set,
            fetched_at: Instant::now(),
        }),
        refresh_lock: Mutex::new(()),
    })))
}

async fn fetch_jwks(client: &reqwest::Client, uri: &str) -> Result<JwkSet> {
    client
        .get(uri)
        .send()
        .await
        .context("failed to fetch OIDC JWKS")?
        .error_for_status()
        .context("OIDC JWKS endpoint returned an error")?
        .json()
        .await
        .context("failed to parse OIDC JWKS")
}

impl OidcValidator {
    async fn validate(&self, token: &str) -> Result<OidcPrincipal> {
        let header = decode_header(token).context("invalid JWT header")?;
        if header.alg != Algorithm::RS256 {
            bail!("unsupported JWT signing algorithm");
        }
        let kid = header.kid.context("JWT is missing kid")?;
        let key = match self.decoding_key(&kid).await? {
            Some(key) => key,
            None => {
                self.refresh_jwks().await?;
                self.decoding_key(&kid)
                    .await?
                    .context("JWT kid is not present in provider JWKS")?
            }
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 60;
        let claims = decode::<AccessTokenClaims>(token, &key, &validation)
            .context("JWT validation failed")?
            .claims;
        if claims.iss != self.issuer {
            bail!("JWT issuer mismatch");
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_secs();
        if claims.exp <= now.saturating_sub(60) {
            bail!("JWT has expired");
        }
        if claims.iat.is_some_and(|iat| iat > now + 60) {
            bail!("JWT issued-at time is in the future");
        }
        if let Some(required_scope) = &self.required_scope {
            let has_scope = claims.scope.as_deref().is_some_and(|scope| {
                scope
                    .split_ascii_whitespace()
                    .any(|scope| scope == required_scope)
            }) || claims.scp.iter().any(|scope| scope == required_scope);
            if !has_scope {
                bail!("JWT lacks required publish scope");
            }
        }
        let namespace_candidate = personal_namespace_candidate(
            claims
                .extra
                .get(&self.namespace_claim)
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&claims.sub),
        );
        Ok(OidcPrincipal {
            issuer: claims.iss,
            subject: claims.sub,
            namespace_candidate,
        })
    }

    async fn decoding_key(&self, kid: &str) -> Result<Option<DecodingKey>> {
        let guard = self.jwks.read().await;
        guard
            .set
            .find(kid)
            .map(DecodingKey::from_jwk)
            .transpose()
            .context("provider returned an unusable JWK")
    }

    async fn refresh_jwks(&self) -> Result<()> {
        let _guard = self.refresh_lock.lock().await;
        if self.jwks.read().await.fetched_at.elapsed() < Duration::from_secs(5) {
            return Ok(());
        }
        let set = fetch_jwks(&self.client, &self.jwks_uri).await?;
        *self.jwks.write().await = CachedJwks {
            set,
            fetched_at: Instant::now(),
        };
        Ok(())
    }
}

fn personal_namespace_candidate(value: &str) -> String {
    let mut result = String::new();
    let mut previous_dash = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            result.push(character);
            previous_dash = false;
        } else if !result.is_empty() && !previous_dash {
            result.push('-');
            previous_dash = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        result.push_str("user");
    }
    result.truncate(64);
    while result.ends_with('-') {
        result.pop();
    }
    result
}

async fn assign_personal_namespace(
    store: &PostgresUploadStore,
    principal: &OidcPrincipal,
) -> Result<String> {
    if let Some(row) =
        sqlx::query("SELECT namespace FROM knack_publishers WHERE issuer = $1 AND subject = $2")
            .bind(&principal.issuer)
            .bind(&principal.subject)
            .fetch_optional(&store.pool)
            .await?
    {
        return Ok(row.get("namespace"));
    }

    let digest = Sha256::digest(format!("{}\0{}", principal.issuer, principal.subject));
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    let mut suffixed_base = principal.namespace_candidate.clone();
    suffixed_base.truncate(55);
    while suffixed_base.ends_with('-') {
        suffixed_base.pop();
    }
    for namespace in [
        principal.namespace_candidate.clone(),
        format!("{suffixed_base}-{suffix}"),
    ] {
        let row = sqlx::query(
            "INSERT INTO knack_publishers (issuer, subject, namespace) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING RETURNING namespace",
        )
        .bind(&principal.issuer)
        .bind(&principal.subject)
        .bind(&namespace)
        .fetch_optional(&store.pool)
        .await?;
        if let Some(row) = row {
            return Ok(row.get("namespace"));
        }
        if let Some(row) =
            sqlx::query("SELECT namespace FROM knack_publishers WHERE issuer = $1 AND subject = $2")
                .bind(&principal.issuer)
                .bind(&principal.subject)
                .fetch_optional(&store.pool)
                .await?
        {
            return Ok(row.get("namespace"));
        }
    }
    bail!("failed to allocate a unique personal namespace")
}

/// Publishing requires both a place to keep uploads and a way to
/// authorise them; enabling one without the other is always operator
/// error, so fail loudly at startup instead of serving a half-open
/// (or silently disabled) endpoint.
async fn build_publish_store(
    data_dir: Option<PathBuf>,
    database_url: Option<&str>,
    tokens: Vec<String>,
) -> Result<Option<PublishStore>> {
    if data_dir.is_some() && database_url.is_some() {
        bail!("--data-dir and --database-url are mutually exclusive");
    }
    match (data_dir, database_url, tokens.is_empty()) {
        (Some(dir), None, false) => {
            let root = dir.join("skills");
            std::fs::create_dir_all(&root)
                .with_context(|| format!("failed to create upload dir {}", root.display()))?;
            Ok(Some(PublishStore::Filesystem(Arc::new(UploadStore {
                cached: Arc::new(CachedSource {
                    repo_dir: root.clone(),
                    sha: tokio::sync::RwLock::new(None),
                    refresh_lock: tokio::sync::RwLock::new(()),
                }),
                root,
                tokens,
            }))))
        }
        (None, Some(url), false) => {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(url)
                .await
                .context("failed to connect to publish database")?;
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS knack_published_skills (\
                 namespace TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL, \
                 checksum TEXT NOT NULL, archive BYTEA NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 PRIMARY KEY (namespace, name))",
            )
            .execute(&pool)
            .await
            .context("failed to initialise publish database")?;
            initialise_publish_database(&pool)
                .await
                .context("failed to migrate publish database")?;
            Ok(Some(PublishStore::Postgres(Arc::new(
                PostgresUploadStore { pool, tokens },
            ))))
        }
        (Some(_), None, true) => bail!(
            "--data-dir requires at least one publish token \
             (--publish-token or --publish-tokens-file); refusing to \
             enable unauthenticated publishing"
        ),
        (None, Some(url), true) => {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(url)
                .await
                .context("failed to connect to publish database")?;
            initialise_publish_database(&pool).await?;
            Ok(Some(PublishStore::Postgres(Arc::new(
                PostgresUploadStore { pool, tokens },
            ))))
        }
        (None, None, false) => bail!(
            "--publish-token/--publish-tokens-file require --data-dir or --database-url \
             so published skills have somewhere persistent to live"
        ),
        (None, None, true) => Ok(None),
        (Some(_), Some(_), _) => unreachable!("mutual exclusion checked above"),
    }
}

async fn initialise_publish_database(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS knack_published_skills (\
         namespace TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL, \
         checksum TEXT NOT NULL, archive BYTEA NOT NULL, \
         updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (namespace, name))",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS knack_publishers (\
         issuer TEXT NOT NULL, subject TEXT NOT NULL, namespace TEXT NOT NULL UNIQUE, \
         created_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (issuer, subject))",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "ALTER TABLE knack_published_skills \
         ADD COLUMN IF NOT EXISTS publisher_issuer TEXT, \
         ADD COLUMN IF NOT EXISTS publisher_subject TEXT, \
         ADD COLUMN IF NOT EXISTS publisher TEXT, \
         ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT now()",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS knack_publish_events (\
         id BIGSERIAL PRIMARY KEY, namespace TEXT NOT NULL, name TEXT NOT NULL, \
         publisher TEXT NOT NULL, checksum TEXT NOT NULL, action TEXT NOT NULL, \
         published_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )
    .execute(pool)
    .await?;
    Ok(())
}

fn filesystem_uploads(store: Option<&PublishStore>) -> Option<&Arc<UploadStore>> {
    match store {
        Some(PublishStore::Filesystem(store)) => Some(store),
        _ => None,
    }
}

/// One-shot materialise: clone all backing sources into a tempdir,
/// walk for skills, write a static snapshot to `args.output`. The
/// snapshot is everything a knack CLI client needs (info, index,
/// per-skill tarballs, SHA map for X-Knack-Resolved-Sha headers) so
/// it can be uploaded as-is to Cloudflare R2, S3, GCS, or any plain
/// static host. A tiny Worker (or any equivalent edge function) in
/// front maps the four CLI-expected endpoints onto these files;
/// `examples/cloudflare-worker/` is a working starter.
///
/// The cache is intentionally a tempdir here: `build-static` runs as
/// a one-shot CI job, so persistence between runs is pointless — each
/// run does a fresh sparse clone, materialises, dumps, and exits.
async fn build_static(args: BuildStaticArgs) -> Result<()> {
    let source_aliases = parse_source_aliases(&args.source_aliases)?;
    let cache_tempdir = tempfile::tempdir().context("failed to create cache tempdir")?;
    let cache = Arc::new(SourceCache::new(
        cache_tempdir.path().to_path_buf(),
        Some(cache_tempdir),
    )?);

    eprintln!("materialising index from {}...", args.index.display());
    let indexed = refresh_index_and_cache(&args.index, &source_aliases, &cache, None).await?;
    eprintln!(
        "materialised {} skill(s) from {} [[source]] entry(ies)",
        indexed.locations.len(),
        indexed.index.source.len(),
    );

    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("failed to create output dir {}", args.output.display()))?;
    let skills_dir = args.output.join("skills");
    if skills_dir.exists() {
        // Wipe and recreate so we don't leave stale tarballs behind
        // for skills that were removed since the last build. Same
        // self-healing intent as `prune_stale` in the live cache.
        std::fs::remove_dir_all(&skills_dir).with_context(|| {
            format!("failed to clear stale skills dir {}", skills_dir.display())
        })?;
    }
    std::fs::create_dir_all(&skills_dir)
        .with_context(|| format!("failed to create {}", skills_dir.display()))?;

    // info.json — matches the shape served by GET /info on the live
    // registry. `name` is whatever was passed via --name; null when
    // omitted. The CLI's `knack registry add <url>` picks the name
    // up from here.
    let info = RegistryInfo {
        name: args.name.clone(),
        version: env!("CARGO_PKG_VERSION"),
        // A static snapshot has no write path by definition — that's
        // the live server's differentiator.
        publish: false,
        authentication: None,
    };
    let info_path = args.output.join("info.json");
    std::fs::write(&info_path, serde_json::to_string_pretty(&info)?)
        .with_context(|| format!("failed to write {}", info_path.display()))?;

    // index.json — full RegistryIndex, with `source` fields rewritten
    // to `<name>:<qualified>` when --name was set (matches the live
    // /search endpoint's rewrite behaviour, just done at build time).
    // qualified_name() handles both scoped and unscoped entries so
    // legacy unscoped skills serialise as "<name>:<skill>" without a
    // stray "/" while scoped ones get the canonical install command.
    let mut index = indexed.index.clone();
    if let Some(name) = &args.name {
        for skill in &mut index.skill {
            skill.source = format!("{}:{}", name, skill.qualified_name());
        }
    }
    let index_path = args.output.join("index.json");
    std::fs::write(&index_path, serde_json::to_string_pretty(&index)?)
        .with_context(|| format!("failed to write {}", index_path.display()))?;

    // sha-map.json — separate file so the Worker can emit
    // X-Knack-Resolved-Sha headers per-archive without parsing the
    // whole index. Keyed by the same qualified form the Worker uses
    // to map URL → R2 key, so a request for
    // /skills/<ns>/<name>/archive can resolve "<ns>/<name>" against
    // the map with a single string operation. Empty entries are
    // omitted; clients fall back to checksum-based change detection
    // in that case.
    //
    // Tarball layout: skills/<namespace>/<name>.skill.tar.gz when
    // scoped, skills/<name>.skill.tar.gz when not. The intermediate
    // namespace directory is created on demand so the Worker's R2
    // PUT (`wrangler r2 object put`) can use `find skills -type f`
    // to walk the tree without special-casing.
    let mut sha_map: BTreeMap<String, String> = BTreeMap::new();
    let mut archive_count = 0usize;
    for (qualified, location) in &indexed.locations {
        if let Some(sha) = location.cached.sha.read().await.clone() {
            sha_map.insert(qualified.clone(), sha);
        }
        let skill_dir = location.cached.repo_dir.join(&location.relative);
        let tarball = create_skill_archive_from_dir(&skill_dir)
            .with_context(|| format!("failed to archive skill {qualified}"))?;
        let out_path = skills_dir.join(format!("{qualified}.skill.tar.gz"));
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create namespace dir {}", parent.display()))?;
        }
        std::fs::write(&out_path, &tarball)
            .with_context(|| format!("failed to write {}", out_path.display()))?;
        archive_count += 1;
    }
    let sha_map_path = args.output.join("sha-map.json");
    std::fs::write(&sha_map_path, serde_json::to_string_pretty(&sha_map)?)
        .with_context(|| format!("failed to write {}", sha_map_path.display()))?;

    eprintln!(
        "wrote {} (info), {} (index), {} archives, {} (sha-map)",
        info_path.display(),
        index_path.display(),
        archive_count,
        sha_map_path.display()
    );
    eprintln!("static snapshot ready at {}", args.output.display());
    Ok(())
}

/// Compose the lookup key used in the locations map and as the URL
/// path segment under /skills/. Same shape that
/// IndexedSkill::qualified_name() produces but available without a
/// full IndexedSkill in hand — used during materialize before the
/// IndexedSkill is constructed.
fn qualified_key(namespace: &Option<String>, name: &str) -> String {
    match namespace {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_string(),
    }
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
    uploads: Option<&UploadStore>,
) -> Result<IndexedState> {
    let mut index = read_index(path)?;
    // Keyed by qualified_name (`<namespace>/<name>` when scoped, bare
    // `<name>` otherwise) so two skills sharing a bare name across
    // namespaces coexist instead of clobbering each other. See
    // log_namespace_collision below for the rejection path on actual
    // same-namespace duplicates.
    let mut locations: HashMap<String, SkillLocation> = HashMap::new();
    let mut active_sources: BTreeSet<String> = BTreeSet::new();

    // Static [[skill]] entries first so they win when an operator
    // hand-pinned a skill and a dynamic walk would otherwise produce
    // the same (namespace, name) tuple. The operator's explicit
    // intent wins; the dynamic copy gets a warn-and-skip.
    for i in 0..index.skill.len() {
        let static_skill = index.skill[i].clone();
        active_sources.insert(static_skill.source.clone());
        // Static entries may set `namespace = "..."` directly. When
        // unset, we infer from the source URL the same way we do for
        // dynamic sources — consistent behaviour across both shapes.
        let resolved_namespace = static_skill
            .namespace
            .clone()
            .or_else(|| infer_namespace_from_source(&static_skill.source));
        let qualified = qualified_key(&resolved_namespace, &static_skill.name);

        let cached = cache.slot(&static_skill.source);
        if let Err(err) = refresh_cached_source(&cached, &static_skill.source, source_aliases).await
        {
            eprintln!(
                "failed to refresh static entry {} from {}: {err:#}",
                qualified, static_skill.source
            );
            continue;
        }
        let relative = source_subpath(&static_skill.source, source_aliases).unwrap_or_default();
        let skill_md = cached.repo_dir.join(&relative).join("SKILL.md");
        if !skill_md.is_file() {
            eprintln!(
                "static skill {} has no SKILL.md at {}",
                qualified,
                skill_md.display()
            );
            continue;
        }
        // Write the resolved namespace back into the IndexedSkill so
        // it surfaces in /index, /search, and downstream rewrites.
        index.skill[i].namespace = resolved_namespace;
        locations.insert(
            qualified,
            SkillLocation {
                cached: cached.clone(),
                relative,
                from_upload: false,
            },
        );
    }

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
        // Effective namespace for every skill materialised under this
        // source: explicit override on the [[source]] entry, falling
        // back to inference from the source URL. Per-skill overrides
        // (e.g. a single SKILL.md inside a multi-vendor repo wanting
        // a different scope) aren't supported on dynamic walks — the
        // operator can move that skill to a static [[skill]] entry
        // with its own `namespace` field if they need that granularity.
        let effective_namespace = source
            .namespace
            .clone()
            .or_else(|| infer_namespace_from_source(&source.source));
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
            let qualified = qualified_key(&effective_namespace, &skill.name);
            if locations.contains_key(&qualified) {
                // First-wins: the earlier source (static or a prior
                // dynamic entry in the TOML order) holds the slot.
                // Operators control conflict resolution by reordering
                // [[source]] entries — deterministic and debuggable.
                eprintln!(
                    "warn: skipped duplicate skill `{qualified}` from {} \
                     (already provided by an earlier source)",
                    source.source
                );
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
                qualified,
                SkillLocation {
                    cached: cached.clone(),
                    relative: relative_to_repo,
                    from_upload: false,
                },
            );
            index.skill.push(IndexedSkill {
                name: skill.name,
                namespace: effective_namespace.clone(),
                description: skill.description,
                source: skill_source,
                tags: source.tags.clone(),
                publisher: None,
                published_at: None,
                updated_at: None,
                score: None,
            });
        }
    }
    // Uploaded skills come last: git-backed sources are declared in
    // the operator-managed index TOML and win any (namespace, name)
    // collision, same first-wins rule that already orders static
    // entries above dynamic ones. Deterministic and debuggable.
    if let Some(uploads) = uploads {
        collect_uploaded_skills(uploads, &mut index, &mut locations).await;
    }

    index.skill.sort_by_key(|skill| skill.qualified_name());
    index.validate()?;

    // Drop cache entries (and their on-disk dirs) for sources the
    // operator removed since the last refresh. Bounded growth.
    cache.prune_stale(&active_sources);

    Ok(IndexedState { index, locations })
}

/// Walk `<data-dir>/skills/<namespace>/<name>/` and fold every valid
/// uploaded skill into the index being built. Malformed entries are
/// warn-and-skip, mirroring how dynamic sources tolerate one bad
/// skill without taking the whole refresh down. Holds the upload
/// store's read lock so a concurrent publish can't swap a directory
/// out from under the walk.
async fn collect_uploaded_skills(
    uploads: &UploadStore,
    index: &mut RegistryIndex,
    locations: &mut HashMap<String, SkillLocation>,
) {
    let _read_guard = uploads.cached.refresh_lock.read().await;
    let namespace_dirs = match sorted_child_dirs(&uploads.root) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!(
                "failed to walk upload dir {}: {err:#}",
                uploads.root.display()
            );
            return;
        }
    };
    for namespace_dir in namespace_dirs {
        let Some(namespace) = namespace_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(String::from)
        else {
            continue;
        };
        // Dot-prefixed entries are in-flight publish tempdirs
        // (tempfile's `.tmp*`); everything else must be a valid
        // namespace or it never came from the publish endpoint.
        if namespace.starts_with('.') {
            continue;
        }
        if let Err(err) = validate_skill_name(&namespace) {
            eprintln!(
                "skipping uploaded namespace dir {}: {err:#}",
                namespace_dir.display()
            );
            continue;
        }
        let skill_dirs = match sorted_child_dirs(&namespace_dir) {
            Ok(dirs) => dirs,
            Err(err) => {
                eprintln!("failed to walk {}: {err:#}", namespace_dir.display());
                continue;
            }
        };
        for skill_dir in skill_dirs {
            let skill = match read_skill(&skill_dir) {
                Ok(skill) => skill,
                Err(err) => {
                    eprintln!(
                        "skipping uploaded skill {}: failed to read SKILL.md: {err:#}",
                        skill_dir.display()
                    );
                    continue;
                }
            };
            // Strict validation (dir name == frontmatter name): the
            // publish endpoint enforces it on the way in, so any
            // mismatch here means the store was edited by hand.
            if let Err(err) = validate_skill(&skill) {
                eprintln!("skipping uploaded skill {}: {err:#}", skill_dir.display());
                continue;
            }
            let qualified = format!("{namespace}/{}", skill.name);
            if locations.contains_key(&qualified) {
                eprintln!(
                    "warn: skipped uploaded skill `{qualified}` \
                     (already provided by a git-backed source)"
                );
                continue;
            }
            locations.insert(
                qualified.clone(),
                SkillLocation {
                    cached: uploads.cached.clone(),
                    relative: Path::new(&namespace).join(&skill.name),
                    from_upload: true,
                },
            );
            index.skill.push(IndexedSkill {
                name: skill.name,
                namespace: Some(namespace.clone()),
                description: skill.description,
                // Uploads have no backing URL; the install-command
                // suffix is the natural source identity, matching the
                // rewrite /search performs for named registries.
                source: qualified,
                tags: Vec::new(),
                publisher: None,
                published_at: None,
                updated_at: None,
                score: None,
            });
        }
    }
}

/// Immediate child directories of `path`, sorted for deterministic
/// walk order across refreshes.
fn sorted_child_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn spawn_refresh_task(
    state: Arc<RwLock<IndexedState>>,
    index_path: PathBuf,
    source_aliases: BTreeMap<String, String>,
    cache: Arc<SourceCache>,
    uploads: Option<Arc<UploadStore>>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match refresh_index_and_cache(&index_path, &source_aliases, &cache, uploads.as_deref())
                .await
            {
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
        publish: state.uploads.is_some(),
        authentication: state.oidc.as_ref().map(|oidc| RegistryAuthentication {
            r#type: "oidc",
            issuer: oidc.issuer.clone(),
            client_id: oidc.client_id.clone(),
            audience: oidc.audience.clone(),
            scopes: oidc.scopes.clone(),
        }),
    })
}

async fn get_index(State(state): State<AppState>) -> Json<RegistryIndex> {
    Json(effective_index(&state).await)
}

async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Json<Vec<IndexedSkill>> {
    let index = effective_index(&state).await;
    // search() already returns results ranked best-match-first (see
    // RegistryIndex::search); we just attach each result's score onto
    // the cloned IndexedSkill so it survives the JSON round-trip to
    // the client, which merges results across multiple registries and
    // needs the score to re-rank the merged set.
    let mut results: Vec<IndexedSkill> = index
        .search(&params.q)
        .into_iter()
        .map(|(skill, score)| {
            let mut skill = skill.clone();
            skill.score = Some(score);
            skill
        })
        .collect();
    if let Some(name) = &state.name {
        // Rewrite to the install-command form the user can paste:
        //   public:anthropics/pdf   ← when scoped
        //   public:pdf              ← legacy unscoped
        // qualified_name() handles both cases so we don't branch
        // here on Option<namespace>.
        for skill in &mut results {
            skill.source = format!("{}:{}", name, skill.qualified_name());
        }
    }
    Json(results)
}

async fn effective_index(state: &AppState) -> RegistryIndex {
    let mut index = state.state.read().await.index.clone();
    let Some(PublishStore::Postgres(store)) = &state.uploads else {
        return index;
    };
    match postgres_indexed_skills(store).await {
        Ok(skills) => {
            let existing: BTreeSet<String> = index
                .skill
                .iter()
                .map(IndexedSkill::qualified_name)
                .collect();
            index.skill.extend(
                skills
                    .into_iter()
                    .filter(|skill| !existing.contains(&skill.qualified_name())),
            );
            index.skill.sort_by_key(IndexedSkill::qualified_name);
        }
        Err(error) => eprintln!("failed to read published skills from Postgres: {error:#}"),
    }
    index
}

async fn postgres_indexed_skills(store: &PostgresUploadStore) -> Result<Vec<IndexedSkill>> {
    let rows = sqlx::query(
        "SELECT namespace, name, description, publisher, created_at::TEXT AS published_at, \
         updated_at::TEXT AS updated_at FROM knack_published_skills ORDER BY namespace, name",
    )
    .fetch_all(&store.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let namespace: String = row.get("namespace");
            let name: String = row.get("name");
            IndexedSkill {
                source: format!("{namespace}/{name}"),
                namespace: Some(namespace),
                name,
                description: row.get("description"),
                tags: Vec::new(),
                publisher: row.get("publisher"),
                published_at: row.get("published_at"),
                updated_at: row.get("updated_at"),
                score: None,
            }
        })
        .collect())
}

/// Namespaced archive route: `/skills/<namespace>/<name>/archive`.
/// Direct lookup against the qualified key, no ambiguity. 404 if
/// no such (namespace, name) exists.
async fn skill_archive_namespaced(
    State(state): State<AppState>,
    AxumPath((namespace, name)): AxumPath<(String, String)>,
) -> Response {
    // Defend against URL-encoded slashes or other shenanigans that
    // would let a caller smuggle a path segment into either field.
    if validate_skill_name(&namespace).is_err() || validate_skill_name(&name).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "namespace and name must be kebab-case identifiers",
        )
            .into_response();
    }
    let qualified = format!("{namespace}/{name}");
    archive_response(&state, &qualified, Some(&namespace), &name).await
}

/// Legacy archive route: `/skills/<name>/archive`. Soft-resolves a
/// bare name against the index: 200 + X-Knack-Namespace when
/// exactly one namespaced (or unscoped) entry matches, 409 when
/// several do (with a hint listing the alternatives), 404
/// otherwise. Lets pre-namespacing knack CLIs and pre-migration
/// manifests/lockfiles keep working after the registry upgrade.
async fn skill_archive_legacy(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if validate_skill_name(&name).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "skill name must be a kebab-case identifier",
        )
            .into_response();
    }
    // Scan the index for any entry whose bare name matches.
    let matches: Vec<(Option<String>, String)> = {
        effective_index(&state)
            .await
            .skill
            .iter()
            .filter(|skill| skill.name == name)
            .map(|skill| (skill.namespace.clone(), skill.qualified_name()))
            .collect()
    };
    match matches.len() {
        0 => (StatusCode::NOT_FOUND, format!("skill not found: {name}")).into_response(),
        1 => {
            let (namespace, qualified) = matches.into_iter().next().expect("len checked");
            archive_response(&state, &qualified, namespace.as_deref(), &name).await
        }
        _ => {
            // Disambiguation hint lists each available qualified
            // identifier so the user can copy-paste the one they
            // want into a namespaced install command.
            let qualifieds: Vec<String> = matches.into_iter().map(|(_, q)| q).collect();
            let hint = format!(
                "skill `{name}` is ambiguous across namespaces: [{}]; \
                 retry as one of the namespaced forms above",
                qualifieds.join(", ")
            );
            (StatusCode::CONFLICT, hint).into_response()
        }
    }
}

/// Shared response builder for both namespaced and legacy archive
/// routes. Looks up the qualified key in the locations map, streams
/// the tarball, and sets the response headers (Content-Type,
/// Content-Disposition, X-Knack-Resolved-Sha, X-Knack-Namespace).
async fn archive_response(
    state: &AppState,
    qualified: &str,
    namespace: Option<&str>,
    name: &str,
) -> Response {
    match create_skill_archive(state, qualified, name).await {
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
                if let Ok(value) = axum::http::HeaderValue::from_str(&sha) {
                    headers.insert(
                        axum::http::HeaderName::from_static("x-knack-resolved-sha"),
                        value,
                    );
                }
            }
            // X-Knack-Namespace lets a CLI that hit the legacy
            // single-segment URL learn which namespace served it so
            // it can persist that into the lockfile and use the
            // namespaced URL on subsequent syncs. Omitted when the
            // resolved skill has no namespace.
            if let Some(ns) = namespace {
                if let Ok(value) = axum::http::HeaderValue::from_str(ns) {
                    headers.insert(
                        axum::http::HeaderName::from_static("x-knack-namespace"),
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

async fn create_skill_archive(
    state: &AppState,
    qualified: &str,
    bare_name: &str,
) -> Result<SkillArchive> {
    if let Some(skills_root) = &state.skills_root {
        // Local --skills-root layouts don't carry namespacing on disk
        // — they predate the concept and are typically a single-vendor
        // operator dropping SKILL.md trees alongside the binary. Look
        // up by bare name so that flow keeps working.
        let skill_dir = skills_root.join(bare_name);
        if skill_dir.join("SKILL.md").is_file() {
            return Ok(SkillArchive {
                bytes: create_skill_archive_from_dir(&skill_dir)?,
                resolved_sha: None,
            });
        }
    }

    let location = {
        let guard = state.state.read().await;
        guard.locations.get(qualified).cloned()
    };
    if let Some(location) = location {
        // Hold the cached source's refresh-read lock while we tar the
        // skill directory. If a background refresh is in progress for
        // this source, it acquired the corresponding write lock and we
        // wait briefly — that's better than letting the refresh truncate
        // the working tree out from under us mid-archive.
        let _read_guard = location.cached.refresh_lock.read().await;
        let resolved_sha = location.cached.sha.read().await.clone();
        let skill_dir = location.cached.repo_dir.join(&location.relative);
        return Ok(SkillArchive {
            bytes: create_skill_archive_from_dir(&skill_dir)?,
            resolved_sha,
        });
    }

    if let Some(PublishStore::Postgres(store)) = &state.uploads {
        let (namespace, name) = qualified
            .split_once('/')
            .with_context(|| format!("skill not found: {qualified}"))?;
        if let Some(row) = sqlx::query(
            "SELECT archive FROM knack_published_skills WHERE namespace = $1 AND name = $2",
        )
        .bind(namespace)
        .bind(name)
        .fetch_optional(&store.pool)
        .await?
        {
            return Ok(SkillArchive {
                bytes: row.get("archive"),
                resolved_sha: None,
            });
        }
    }

    bail!("skill not found: {qualified}")
}

/// Publish endpoint: `PUT /skills/{namespace}/{name}` with a
/// `knack pack` tarball as the body. This is the live server's
/// differentiator over a static snapshot: skills land here directly,
/// without having to pass through a git repository first.
///
/// Disabled (403) unless the operator opted in with a storage backend
/// plus a publish token. Authenticated uploads are validated before
/// being stored and made visible to index, search, and archive reads.
async fn publish(
    State(state): State<AppState>,
    AxumPath((namespace, name)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(uploads) = state.uploads.clone() else {
        return (
            StatusCode::FORBIDDEN,
            "publishing is not enabled on this registry; the operator must \
             configure --data-dir or --database-url with --publish-token",
        )
            .into_response();
    };
    let principal = match authenticate_publish(&headers, &uploads, state.oidc.as_deref()).await {
        Ok(principal) => principal,
        Err((status, message)) => return (status, message).into_response(),
    };
    if matches!(principal, PublishPrincipal::Oidc(_)) {
        return (
            StatusCode::FORBIDDEN,
            "OIDC users publish to their automatically assigned personal namespace; omit --namespace",
        )
            .into_response();
    }
    if validate_skill_name(&namespace).is_err() || validate_skill_name(&name).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "namespace and name must be kebab-case identifiers",
        )
            .into_response();
    }
    publish_to_namespace(state, uploads, namespace, name, body, None).await
}

async fn publish_personal(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(uploads) = state.uploads.clone() else {
        return (StatusCode::FORBIDDEN, "publishing is not enabled").into_response();
    };
    if validate_skill_name(&name).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "name must be a kebab-case identifier",
        )
            .into_response();
    }
    let principal = match authenticate_publish(&headers, &uploads, state.oidc.as_deref()).await {
        Ok(PublishPrincipal::Oidc(principal)) => principal,
        Ok(PublishPrincipal::ServiceToken) => {
            return (
                StatusCode::BAD_REQUEST,
                "service-token publishes must include an explicit namespace",
            )
                .into_response();
        }
        Err((status, message)) => return (status, message).into_response(),
    };
    let PublishStore::Postgres(store) = &uploads else {
        return (StatusCode::FORBIDDEN, "OIDC publishing requires Postgres").into_response();
    };
    let namespace = match assign_personal_namespace(store, &principal).await {
        Ok(namespace) => namespace,
        Err(error) => {
            eprintln!("failed to assign personal namespace: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to assign personal namespace",
            )
                .into_response();
        }
    };
    let publisher = format!("{}#{}", principal.issuer, principal.subject);
    publish_to_namespace(state, uploads, namespace, name, body, Some(publisher)).await
}

async fn authenticate_publish(
    headers: &HeaderMap,
    uploads: &PublishStore,
    oidc: Option<&OidcValidator>,
) -> Result<PublishPrincipal, (StatusCode, &'static str)> {
    let Some(token) = bearer_token(headers) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "publishing requires an `Authorization: Bearer <token>` header",
        ));
    };
    if token_authorised(token, uploads.tokens()) {
        return Ok(PublishPrincipal::ServiceToken);
    }
    let Some(oidc) = oidc else {
        return Err((StatusCode::FORBIDDEN, "publish token not recognised"));
    };
    oidc.validate(token)
        .await
        .map(PublishPrincipal::Oidc)
        .map_err(|error| {
            eprintln!("OIDC publish authentication failed: {error:#}");
            (StatusCode::UNAUTHORIZED, "OIDC access token is invalid")
        })
}

async fn publish_to_namespace(
    state: AppState,
    uploads: PublishStore,
    namespace: String,
    name: String,
    body: Bytes,
    publisher: Option<String>,
) -> Response {
    let qualified = format!("{namespace}/{name}");

    {
        let guard = state.state.read().await;
        if let Some(existing) = guard.locations.get(&qualified)
            && !existing.from_upload
        {
            return (
                StatusCode::CONFLICT,
                format!(
                    "skill `{qualified}` is provided by a git-backed source in the registry index"
                ),
            )
                .into_response();
        }
    }

    let accepted = match &uploads {
        PublishStore::Filesystem(uploads) => accept_upload(uploads, &namespace, &name, &body).await,
        PublishStore::Postgres(uploads) => {
            accept_postgres_upload(uploads, &namespace, &name, &body, publisher.as_deref()).await
        }
    };
    let accepted = match accepted {
        Ok(accepted) => accepted,
        Err((status, message)) => return (status, message).into_response(),
    };

    if let PublishStore::Filesystem(uploads) = &uploads {
        let mut guard = state.state.write().await;
        guard.locations.insert(
            qualified.clone(),
            SkillLocation {
                cached: uploads.cached.clone(),
                relative: Path::new(&namespace).join(&name),
                from_upload: true,
            },
        );
        guard
            .index
            .skill
            .retain(|skill| skill.qualified_name() != qualified);
        guard.index.skill.push(IndexedSkill {
            name: name.clone(),
            namespace: Some(namespace.clone()),
            description: accepted.description,
            source: qualified,
            tags: Vec::new(),
            publisher: None,
            published_at: None,
            updated_at: None,
            score: None,
        });
        guard.index.skill.sort_by_key(IndexedSkill::qualified_name);
    }

    let status = if accepted.replaced {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    (
        status,
        Json(serde_json::json!({
            "name": name,
            "namespace": namespace,
            "publisher": publisher,
            "checksum": accepted.checksum,
            "replaced": accepted.replaced,
        })),
    )
        .into_response()
}

struct AcceptedUpload {
    description: String,
    checksum: String,
    replaced: bool,
}

/// Validate and store an uploaded skill archive. Unpacks into a
/// tempdir *inside the upload root* so the final rename is
/// same-filesystem, validates the skill (shape, SKILL.md, names
/// agree), then swaps it into `<root>/<namespace>/<name>/` under the
/// store's write lock — the same lock archive reads and refresh walks
/// take as readers, so nobody observes a half-moved directory.
async fn accept_upload(
    uploads: &UploadStore,
    namespace: &str,
    name: &str,
    body: &[u8],
) -> Result<AcceptedUpload, (StatusCode, String)> {
    fn internal(context: &str, err: impl std::fmt::Display) -> (StatusCode, String) {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{context}: {err}"),
        )
    }
    let staging = tempfile::tempdir_in(&uploads.root)
        .map_err(|err| internal("failed to create staging dir", err))?;
    let skill_root =
        unpack_skill_archive(std::io::Cursor::new(body), staging.path()).map_err(|err| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid skill archive: {err:#}"),
            )
        })?;
    let skill = read_skill(&skill_root).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid skill archive: {err:#}"),
        )
    })?;
    validate_skill(&skill).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid skill archive: {err:#}"),
        )
    })?;
    if skill.name != name {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "archive contains skill `{}` but the request URL names `{name}`; \
                 re-pack the skill or fix the URL",
                skill.name
            ),
        ));
    }

    let _write_guard = uploads.cached.refresh_lock.write().await;
    let namespace_dir = uploads.root.join(namespace);
    std::fs::create_dir_all(&namespace_dir)
        .map_err(|err| internal("failed to create namespace dir", err))?;
    let destination = namespace_dir.join(name);
    let replaced = destination.exists();
    if replaced {
        std::fs::remove_dir_all(&destination)
            .map_err(|err| internal("failed to replace previous upload", err))?;
    }
    std::fs::rename(&skill_root, &destination)
        .map_err(|err| internal("failed to store uploaded skill", err))?;
    let checksum = checksum_dir(&destination)
        .map_err(|err| internal("failed to checksum upload", format!("{err:#}")))?;

    Ok(AcceptedUpload {
        description: skill.description,
        checksum,
        replaced,
    })
}

async fn accept_postgres_upload(
    uploads: &PostgresUploadStore,
    namespace: &str,
    name: &str,
    body: &[u8],
    publisher: Option<&str>,
) -> Result<AcceptedUpload, (StatusCode, String)> {
    fn internal(context: &str, err: impl std::fmt::Display) -> (StatusCode, String) {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{context}: {err}"),
        )
    }
    let staging =
        tempfile::tempdir().map_err(|err| internal("failed to create staging dir", err))?;
    let skill_root =
        unpack_skill_archive(std::io::Cursor::new(body), staging.path()).map_err(|err| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid skill archive: {err:#}"),
            )
        })?;
    let skill = read_skill(&skill_root).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid skill archive: {err:#}"),
        )
    })?;
    validate_skill(&skill).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid skill archive: {err:#}"),
        )
    })?;
    if skill.name != name {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "archive contains skill `{}` but the request URL names `{name}`; \
                 re-pack the skill or fix the URL",
                skill.name
            ),
        ));
    }
    let checksum = checksum_dir(&skill_root)
        .map_err(|err| internal("failed to checksum upload", format!("{err:#}")))?;
    let mut transaction = uploads
        .pool
        .begin()
        .await
        .map_err(|err| internal("failed to begin publish transaction", err))?;
    let row = sqlx::query(
        "INSERT INTO knack_published_skills \
         (namespace, name, description, checksum, archive, publisher) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (namespace, name) DO UPDATE SET \
         description = EXCLUDED.description, checksum = EXCLUDED.checksum, \
         archive = EXCLUDED.archive, publisher = EXCLUDED.publisher, updated_at = now() \
         RETURNING (xmax <> 0) AS replaced",
    )
    .bind(namespace)
    .bind(name)
    .bind(&skill.description)
    .bind(&checksum)
    .bind(body)
    .bind(publisher)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|err| internal("failed to store uploaded skill", err))?;
    let replaced: bool = row.get("replaced");
    if let Some(publisher) = publisher {
        sqlx::query(
            "INSERT INTO knack_publish_events \
             (namespace, name, publisher, checksum, action) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(namespace)
        .bind(name)
        .bind(publisher)
        .bind(&checksum)
        .bind(if replaced { "updated" } else { "created" })
        .execute(&mut *transaction)
        .await
        .map_err(|err| internal("failed to record publish event", err))?;
    }
    transaction
        .commit()
        .await
        .map_err(|err| internal("failed to commit publish", err))?;

    Ok(AcceptedUpload {
        description: skill.description,
        checksum,
        replaced,
    })
}

/// Extract the token from an `Authorization: Bearer <token>` header.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn token_authorised(provided: &str, tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| constant_time_eq(token.as_bytes(), provided.as_bytes()))
}

/// Timing-safe byte comparison so token checks don't leak how many
/// leading bytes matched. Length is still observable, which is fine —
/// token length isn't the secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_personal_namespace_candidates() {
        assert_eq!(personal_namespace_candidate("Alice Smith"), "alice-smith");
        assert_eq!(
            personal_namespace_candidate("alice@example.com"),
            "alice-example-com"
        );
        assert_eq!(personal_namespace_candidate("---"), "user");
        assert!(personal_namespace_candidate(&"a".repeat(100)).len() <= 64);
    }

    #[test]
    fn extracts_only_bearer_authorization() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert_eq!(bearer_token(&headers), Some("secret"));
    }

    #[test]
    fn compares_service_tokens() {
        assert!(token_authorised("secret", &["secret".to_string()]));
        assert!(!token_authorised("other", &["secret".to_string()]));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
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

/// Derive a default namespace from the source URL when the
/// `[[source]] namespace = "..."` override isn't set in the registry
/// index TOML.
///
/// For `gh:owner/repo[@ref]/path` the namespace is `owner`. For
/// `<alias>:owner/repo[@ref]/path` (git-host registry alias form)
/// the namespace is likewise `owner`. Returns None when no
/// reasonable owner can be extracted, or when the extracted owner
/// doesn't satisfy validate_skill_name (e.g. an org with uppercase
/// characters can't safely round-trip through the URL path); the
/// operator must set an explicit override in those cases.
///
/// This is best-effort intentionally — namespacing is a curator's
/// responsibility, not the parser's. An override in TOML always
/// trumps inference.
fn infer_namespace_from_source(source: &str) -> Option<String> {
    let rest = if let Some(spec) = source.strip_prefix("gh:") {
        spec
    } else {
        // alias:owner/repo[/path]
        let (_alias, rest) = source.split_once(':')?;
        rest
    };
    let owner = rest.split('/').next()?;
    // Strip an @ref attached to the owner segment defensively;
    // real-world specs put the ref on the repo segment, not the
    // owner, but this guards against malformed input.
    let owner = owner.split_once('@').map_or(owner, |(o, _)| o);
    if owner.is_empty() {
        return None;
    }
    // Must satisfy the kebab-case rules to be URL-safe and to
    // round-trip through validate_skill_name on the client side.
    validate_skill_name(owner).ok()?;
    Some(owner.to_string())
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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
