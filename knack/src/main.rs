use std::{
    fmt,
    fs::{self, File},
    io::Cursor,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anstyle::{AnsiColor, Effects, Style};
use anyhow::{Context, Result, anyhow, bail};
use clap::builder::styling::Styles;
use clap::{Parser, Subcommand};

/// Colour palette for clap's --help renderer. Matches the runtime
/// success/accent/label palette used by the status() helper so the
/// help text feels like part of the same program rather than a
/// dropped-in stock template.
const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Blue.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default());

/// The public knack registry bootstrapped by `knack init` so users
/// have skills available immediately, without having to discover and
/// add a registry as a separate first step. Opt out with
/// `knack init --no-public-registry`; remove later with
/// `knack registry remove public`.
///
/// Hard-coded rather than fetched from /info at init time so init
/// works offline. The HTTP scheme is fixed (RegistryKind::Http);
/// changing the URL here changes the bootstrap default, not the
/// runtime contract — existing manifests keep whatever URL they were
/// written with.
const PUBLIC_REGISTRY_NAME: &str = "public";
const PUBLIC_REGISTRY_URL: &str = "https://knack.ajac-zero.com";

use flate2::{Compression, write::GzEncoder};
use knack_core::{
    IndexedSkill, LockedSkill, Lockfile, Manifest, RegistryConfig, RegistryIndex, RegistryKind,
    checksum_dir, collect_files, read_skill, validate_skill, validate_skill_name,
};
use tar::{Builder, Header};
use tempfile::TempDir;

#[derive(Debug, Parser)]
#[command(name = "knack")]
#[command(version, about = "Package, share, and install Agent Skills")]
#[command(styles = HELP_STYLES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a knack.toml manifest.
    Init {
        /// Path where the manifest should be written.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Directory where project skills should be installed.
        #[arg(long)]
        target: Option<PathBuf>,

        /// Initialize the global manifest (~/.agents/knack.toml) instead of the current project's .agents/knack.toml.
        #[arg(short = 'g', long)]
        global: bool,

        /// Skip seeding the public knack registry (https://knack.ajac-zero.com)
        /// into the new manifest. By default `knack init` registers it as
        /// `public` so common skills are available via `knack add public:<name>`
        /// out of the box. Pass this flag if you want a strictly empty
        /// manifest, or if you intend to use a private/internal registry only.
        #[arg(long)]
        no_public_registry: bool,
    },

    /// Install a skill source and record it in the project manifest.
    Add {
        /// Path, archive, or gh: source to add.
        source: String,

        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,
    },

    /// Install missing skills using the lockfile for reproducibility.
    ///
    /// Skills that already exist on disk are left untouched. Skills missing
    /// from the install target are installed via the lockfile's
    /// `resolved` source — which is SHA-pinned where possible — so every
    /// run produces the same content. Use `knack update` to pick up
    /// upstream changes.
    Sync {
        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,

        /// Verify that the install dir and lockfile match the manifest
        /// without installing anything. Fails with a non-zero exit code
        /// on any inconsistency. Suitable for CI: pair with `git diff
        /// --exit-code` to assert reproducibility.
        #[arg(long)]
        check: bool,
    },

    /// Re-resolve every skill from its manifest source and reinstall
    /// anything that has changed upstream.
    ///
    /// Pass one or more skill names to update only those skills,
    /// leaving the rest at their current lockfile state. Without
    /// arguments, update touches every skill in the manifest.
    ///
    /// Sources pinned to a SHA-shaped ref are skipped — they're immutable
    /// by definition, so there's nothing to update. Pass --force to
    /// re-fetch them anyway (useful if upstream force-pushed).
    Update {
        /// Names of skills to update. When omitted, every skill in the
        /// manifest is updated. Skills not present in the manifest are
        /// rejected upfront before any network is touched.
        skills: Vec<String>,

        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,

        /// Re-resolve and reinstall sources that would otherwise be
        /// skipped because their ref is SHA-shaped (and therefore
        /// immutable). Useful when a force-push has rewritten history
        /// under a SHA you previously installed, or when you just want
        /// to retry the install.
        #[arg(short = 'f', long)]
        force: bool,

        /// Report what would change without modifying the install target
        /// or lockfile. Still hits the network to fetch fresh content
        /// so the report is accurate.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Find skills to install from configured registries.
    ///
    /// Searches the merged set of registries from project, global, and
    /// system configs, so a global registry is reachable from a directory
    /// with no project manifest. No scope flag needed.
    Find {
        /// Search query.
        query: String,

        /// Path to a specific manifest to read instead of the default
        /// project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Maximum number of matches to display, ranked best-match
        /// first. Registries may return many results for a broad
        /// query; this caps output volume without losing the
        /// highest-relevance hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    /// Publish a skill to a git-backed skill repository.
    Publish {
        /// Path to the skill directory to publish.
        path: PathBuf,

        /// Git-host registry alias to publish through.
        #[arg(long)]
        registry: String,

        /// Repository in owner/repo form under the registry host.
        #[arg(long)]
        repo: String,

        /// Directory inside the repository where skills live.
        #[arg(long, default_value = "skills")]
        skills_dir: PathBuf,

        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,

        /// Do not push the generated commit. The clone is preserved on
        /// disk and its path is printed so you can inspect the result
        /// and push manually when ready.
        #[arg(long)]
        no_push: bool,
    },

    /// Manage registry aliases.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },

    /// Manage registry indexes.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },

    /// Create a new skill directory.
    New {
        /// Skill name. Must be lowercase letters, numbers, and hyphens.
        name: String,

        /// Directory where the skill should be created.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },

    /// Validate an Agent Skill directory.
    Validate {
        /// Path to the skill directory.
        path: PathBuf,
    },

    /// Package a skill into a distributable archive.
    Pack {
        /// Path to the skill directory.
        path: PathBuf,

        /// Directory where the archive should be written.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },

    /// Install a skill from a local directory or packaged archive.
    Install {
        /// Path to a skill directory/package archive, or gh:owner/repo[@ref]/path/to/skill.
        source: String,

        /// Directory where skills should be installed.
        #[arg(long)]
        target: Option<PathBuf>,

        /// Install into the global skill directory (~/.agents/skills/) instead of the current project's .agents/skills/.
        #[arg(short = 'g', long)]
        global: bool,
    },

    /// List installed skills.
    ///
    /// Shows skills from both the current project (.agents/skills/) and the
    /// user's global directory (~/.agents/skills/) as separate sections.
    /// Empty sections are suppressed. Pass --target to inspect a specific
    /// directory instead.
    List {
        /// Inspect a specific skill directory instead of the default project
        /// and global directories.
        #[arg(long)]
        target: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    /// Generate a registry index from a local tree of skills.
    Generate {
        /// Root directory to scan for skill directories.
        root: PathBuf,

        /// Source prefix used to build installable sources for indexed skills.
        #[arg(long)]
        source_prefix: String,

        /// Output index file.
        #[arg(short, long, default_value = "knack.index.toml")]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Add or update a registry alias.
    ///
    /// The registry kind is inferred from the URL scheme: `git+ssh://` and
    /// `git+https://` are git-host registries; `http://` and `https://` are
    /// HTTP knack registries.
    ///
    /// `<name>` is optional for HTTP registries: when omitted, the CLI
    /// fetches `GET <url>/info` and adopts the registry's advertised
    /// name. Git-host registries always require an explicit `<name>`.
    Add {
        /// Base URL. Examples: `git+ssh://git@gitea.example.com`,
        /// `git+https://github.com`, `http://127.0.0.1:7349`,
        /// `https://knack.example.com`.
        url: String,

        /// Local alias name, e.g. tea. When omitted and the URL points to
        /// an HTTP registry, the name is read from the registry's /info
        /// endpoint so every client of the registry adopts the same alias.
        name: Option<String>,

        /// Default Git ref for git-host registries. Ignored for HTTP
        /// registries.
        #[arg(long, default_value = "main")]
        default_ref: String,

        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,
    },

    /// List registry aliases.
    ///
    /// Shows the merged set of aliases from project, global, and system
    /// configs, so a global alias is listed even from a directory with no
    /// project manifest. No scope flag needed.
    List {
        /// Path to a specific manifest to read instead of the default
        /// project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

    /// Remove a registry alias.
    Remove {
        /// Alias name to remove.
        name: String,

        /// Path to the project manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,

        /// Use global scope (~/.agents/) instead of the current project.
        #[arg(short = 'g', long)]
        global: bool,
    },
}

/// Infer the registry backend kind from the URL scheme. `git+` URLs are
/// git-host registries; `http(s)://` URLs are HTTP knack registries. Any
/// other scheme is an error so users get an actionable diagnostic instead
/// of a silently wrong configuration.
fn infer_registry_kind(url: &str) -> Result<RegistryKind> {
    if url.starts_with("git+") {
        Ok(RegistryKind::GitHost)
    } else if url.starts_with("http://") || url.starts_with("https://") {
        Ok(RegistryKind::Http)
    } else {
        bail!(
            "cannot determine registry kind from URL `{url}`; \
             expected a scheme of `git+ssh://`, `git+https://`, `http://`, or `https://`"
        );
    }
}

/// Resolve the local alias name for a new registry. If the user passed an
/// explicit name we use it. Otherwise, for HTTP registries we fetch
/// `GET <url>/info` and adopt whatever name the registry advertises —
/// this gives parity across clients without anyone having to coordinate
/// out of band. Git-host registries don't have an `/info` endpoint, so
/// the name remains required for them.
fn resolve_registry_name(
    provided: Option<String>,
    url: &str,
    kind: RegistryKind,
) -> Result<String> {
    if let Some(name) = provided {
        return Ok(name);
    }
    match kind {
        RegistryKind::Http => fetch_advertised_registry_name(url),
        RegistryKind::GitHost => bail!(
            "git-host registries don't advertise a name; \
             pass an explicit alias as the second argument, e.g. \
             `knack registry add {url} <name>`"
        ),
    }
}

/// Fetch GET <base>/info and return the advertised name. Errors loudly
/// when the registry didn't set one — there's no sensible default we
/// can pick on the user's behalf.
fn fetch_advertised_registry_name(base_url: &str) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    let info_url = format!("{base}/info");
    let response = reqwest::blocking::Client::new()
        .get(&info_url)
        .header(reqwest::header::USER_AGENT, "knack")
        .send()
        .with_context(|| format!("failed to fetch {info_url}"))?
        .error_for_status()
        .with_context(|| format!("registry returned an error for {info_url}"))?;
    #[derive(serde::Deserialize)]
    struct RegistryInfo {
        name: Option<String>,
    }
    let info: RegistryInfo = response
        .json()
        .with_context(|| format!("failed to parse {info_url} as RegistryInfo JSON"))?;
    info.name.ok_or_else(|| {
        anyhow!(
            "registry at {base_url} doesn't advertise a name; \
             pass an explicit alias as the second argument, e.g. \
             `knack registry add {base_url} <name>`"
        )
    })
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Project,
    Global,
    System,
}

impl Scope {
    /// Convert a CLI `-g`/`--global` boolean into a Scope. System scope is not
    /// exposed through any per-command flag: admins edit /etc/knack/knack.toml
    /// directly, and the layered registry inheritance in effective_registries
    /// reads it from there.
    fn from_global_flag(global: bool) -> Self {
        if global { Self::Global } else { Self::Project }
    }

    fn manifest_path(self) -> Result<PathBuf> {
        match self {
            Self::Project => Ok(PathBuf::from(".agents/knack.toml")),
            Self::Global => Ok(home_dir()?.join(".agents/knack.toml")),
            Self::System => Ok(PathBuf::from("/etc/knack/knack.toml")),
        }
    }

    fn install_target(self) -> Result<PathBuf> {
        match self {
            Self::Project => Ok(PathBuf::from(".agents/skills")),
            Self::Global => Ok(home_dir()?.join(".agents/skills")),
            Self::System => Ok(PathBuf::from("/usr/local/share/knack/skills")),
        }
    }
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; cannot resolve global skill directory"))
}

fn resolve_manifest_path(manifest: Option<PathBuf>, scope: Scope) -> Result<PathBuf> {
    match manifest {
        Some(path) => Ok(path),
        None => scope.manifest_path(),
    }
}

fn resolve_target_path(target: Option<PathBuf>, scope: Scope) -> Result<PathBuf> {
    match target {
        Some(path) => Ok(path),
        None => scope.install_target(),
    }
}

fn success_style() -> Style {
    AnsiColor::Green.on_default() | Effects::BOLD
}

fn accent_style() -> Style {
    AnsiColor::Cyan.on_default() | Effects::BOLD
}

/// Dimmed, uncoloured — used for field labels ("registry:", "from",
/// etc.) so they read as secondary/structural text rather than
/// competing for attention with the actual values. Deliberately not
/// tied to a specific ANSI colour: colour choices like blue read
/// poorly on some terminal themes, and stacking a bold colour on
/// every label next to a bold colour on every value (the previous
/// blue-label/cyan-value pairing) made output feel like a wall of
/// colour with no visual hierarchy.
fn label_style() -> Style {
    Style::new().dimmed()
}

fn status(action: &str, value: impl fmt::Display) {
    let action_style = success_style();
    let value_style = accent_style();
    anstream::println!(
        "{action_style}{action}{action_style:#} {value_style}{value}{value_style:#}"
    );
}

fn notice(message: &str) {
    let message_style = success_style();
    anstream::println!("{message_style}{message}{message_style:#}");
}

fn warn_style() -> Style {
    AnsiColor::Yellow.on_default() | Effects::BOLD
}

/// Prints a non-fatal warning to stderr (not stdout, so scripts piping
/// `knack find`'s result lines aren't polluted). Used for degraded
/// conditions the user should know about but that don't abort the
/// command — e.g. one registry among several being unreachable.
fn warn(message: &str) {
    let message_style = warn_style();
    anstream::eprintln!("{message_style}warning:{message_style:#} {message}");
}

/// Prints `text` word-wrapped to `width` columns, each line indented
/// by `indent` spaces and with no label — used for free-form prose
/// (a skill's description in `find`'s output) where a "label: value"
/// row per field reads as a bulleted list rather than the compact
/// name/description/command card `find` aims for. Real skill
/// descriptions regularly run several sentences, which overflows the
/// terminal mid-word if printed as a single unbroken line.
fn print_wrapped(text: &str, width: usize, indent: usize) {
    for line in wrap_text(text, width.saturating_sub(indent).max(20)) {
        anstream::println!("{:indent$}{line}", "");
    }
}

/// Greedy word-wrap: packs whitespace-separated words onto each line
/// up to `width` columns. Doesn't attempt hyphenation or
/// unicode-width awareness (a plain `.len()` byte count) — adequate
/// for the mostly-ASCII skill descriptions this is used for, and
/// avoids pulling in a wrapping crate for one call site.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Best-effort terminal width for wrapping decisions. Honours
/// `COLUMNS` when a shell exports it; otherwise falls back to a
/// conservative 100-column default rather than pulling in a
/// terminal-size detection crate for this one heuristic. Clamped to a
/// sane minimum so a garbage/tiny `COLUMNS` value can't produce
/// unreadable one-word-per-line output.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&w| w > 0)
        .unwrap_or(100)
        .max(40)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Init {
            manifest,
            target,
            global,
            no_public_registry,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest = resolve_manifest_path(manifest, scope)?;
            let target = resolve_target_path(target, scope)?;
            init_manifest(&manifest, &target, !no_public_registry)?;
        }
        Command::Add {
            source,
            manifest,
            global,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest = resolve_manifest_path(manifest, scope)?;
            let default_target = scope.install_target()?;
            add_skill(&manifest, &source, &default_target)?;
        }
        Command::Sync {
            manifest,
            global,
            check,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest = resolve_manifest_path(manifest, scope)?;
            if check {
                check_skills(&manifest)?;
            } else {
                sync_skills(&manifest)?;
            }
        }
        Command::Update {
            skills,
            manifest,
            global,
            force,
            dry_run,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest = resolve_manifest_path(manifest, scope)?;
            update_skills(&manifest, force, dry_run, &skills)?;
        }
        Command::Find {
            query,
            manifest,
            limit,
        } => {
            find_registry_skills(manifest.as_deref(), &query, limit)?;
        }
        Command::Publish {
            path,
            registry,
            repo,
            skills_dir,
            manifest,
            global,
            no_push,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest = resolve_manifest_path(manifest, scope)?;
            publish_skill(&manifest, &path, &registry, &repo, &skills_dir, no_push)?;
        }
        Command::Registry { command } => {
            handle_registry_command(command)?;
        }
        Command::Index { command } => {
            handle_index_command(command)?;
        }
        Command::New { name, dir } => {
            new_skill(&name, dir)?;
        }
        Command::Validate { path } => {
            let skill = read_skill(&path)?;
            validate_skill(&skill)?;
            status("valid skill:", skill.name);
        }
        Command::Pack { path, output } => {
            let archive = pack_skill(&path, &output)?;
            status("packed skill:", archive.display());
        }
        Command::Install {
            source,
            target,
            global,
        } => {
            let scope = Scope::from_global_flag(global);
            let target = resolve_target_path(target, scope)?;
            let installed = install_skill(&source, &target)?;
            status("installed skill:", installed.path.display());
        }
        Command::List { target } => {
            list_skills(target.as_deref())?;
        }
    }

    Ok(())
}

fn handle_registry_command(command: RegistryCommand) -> Result<()> {
    match command {
        RegistryCommand::Add {
            url,
            name,
            default_ref,
            manifest,
            global,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest_path = resolve_manifest_path(manifest, scope)?;
            let default_target = scope.install_target()?;
            let kind = infer_registry_kind(&url)?;
            let resolved_name = resolve_registry_name(name, &url, kind)?;
            registry_add(
                &manifest_path,
                &resolved_name,
                RegistryConfig {
                    kind,
                    url,
                    default_ref,
                },
                &default_target,
            )?;
        }
        RegistryCommand::List { manifest } => {
            registry_list(manifest.as_deref())?;
        }
        RegistryCommand::Remove {
            name,
            manifest,
            global,
        } => {
            let scope = Scope::from_global_flag(global);
            let manifest_path = resolve_manifest_path(manifest, scope)?;
            registry_remove(&manifest_path, &name)?;
        }
    }

    Ok(())
}

fn handle_index_command(command: IndexCommand) -> Result<()> {
    match command {
        IndexCommand::Generate {
            root,
            source_prefix,
            output,
        } => {
            generate_index(&root, &source_prefix, &output)?;
            status("generated index:", output.display());
        }
    }

    Ok(())
}

fn generate_index(root: &Path, source_prefix: &str, output: &Path) -> Result<()> {
    let mut index = RegistryIndex::default();
    for skill_dir in collect_skill_dirs(root)? {
        let skill = read_skill(&skill_dir)?;
        validate_skill(&skill)?;
        let relative = skill_dir.strip_prefix(root).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                skill_dir.display(),
                root.display()
            )
        })?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        index.skill.push(IndexedSkill {
            name: skill.name,
            // Static-index generation doesn't know a namespace —
            // operators authoring a knack.index.toml can add scoping
            // by hand if they need it. Dynamic-source materialize
            // (the registry side) does derive namespaces; that lands
            // in a subsequent commit.
            namespace: None,
            description: skill.description,
            source: format!("{}/{}", source_prefix.trim_end_matches('/'), relative),
            tags: Vec::new(),
            score: None,
        });
    }
    index
        .skill
        .sort_by(|left, right| left.name.cmp(&right.name));
    index.validate()?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(&index).context("failed to serialize index")?;
    fs::write(output, contents).with_context(|| format!("failed to write {}", output.display()))?;
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

    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
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

fn registry_add(
    manifest_path: &Path,
    name: &str,
    config: RegistryConfig,
    default_target: &Path,
) -> Result<()> {
    validate_registry_name(name)?;
    ensure_manifest_exists(manifest_path, default_target)?;
    let mut manifest = read_manifest(manifest_path)?;
    let prior = manifest.registries.insert(name.to_string(), config.clone());
    write_manifest(manifest_path, &manifest)?;
    match prior {
        None => status("registered registry:", name),
        Some(ref old) if old == &config => {
            notice(&format!("registry already configured: {name}"));
        }
        Some(old) => {
            status("updated registry:", name);
            print_registry_diff(&old, &config);
        }
    }
    Ok(())
}

fn registry_kind_label(kind: RegistryKind) -> &'static str {
    match kind {
        RegistryKind::GitHost => "git-host",
        RegistryKind::Http => "http",
    }
}

/// Hint suffix appended to "no registry registered as X" diagnostics.
/// Lists the aliases the user does have, or tells them how to add one if
/// the manifest is bare. Kept separate from the diagnostic itself so that
/// callers can prepend whatever context is locally meaningful (e.g. the
/// source string being resolved on `knack add`).
fn unknown_registry_hint(
    registries: &std::collections::BTreeMap<String, RegistryConfig>,
) -> String {
    if registries.is_empty() {
        "no registries are configured; register one with \
         `knack registry add <url> [<name>]`"
            .to_string()
    } else {
        let known: Vec<&str> = registries.keys().map(String::as_str).collect();
        format!(
            "known aliases: {}; run `knack registry list` for details",
            known.join(", "),
        )
    }
}

/// Print per-field changes for an updated registry alias. Only fields that
/// actually changed are shown so the diff stays focused on what the user
/// just did.
fn print_registry_diff(old: &RegistryConfig, new: &RegistryConfig) {
    if old.kind != new.kind {
        anstream::println!(
            "  kind:        {} -> {}",
            registry_kind_label(old.kind),
            registry_kind_label(new.kind),
        );
    }
    if old.url != new.url {
        anstream::println!("  url:         {} -> {}", old.url, new.url);
    }
    if old.default_ref != new.default_ref {
        anstream::println!("  default-ref: {} -> {}", old.default_ref, new.default_ref,);
    }
}

fn registry_list(explicit_manifest: Option<&Path>) -> Result<()> {
    let (manifest, manifest_path) = read_manifest_for_read(explicit_manifest)?;
    for (name, registry) in effective_registries(&manifest, &manifest_path)? {
        let name_style = accent_style();
        let label_style = label_style();
        anstream::println!(
            "{name_style}{name}{name_style:#}\t{label_style}{}{label_style:#}\t{}",
            registry_kind_label(registry.kind),
            registry.url
        );
    }
    Ok(())
}

fn registry_remove(manifest_path: &Path, name: &str) -> Result<()> {
    let mut manifest = read_manifest(manifest_path)?;
    if manifest.registries.remove(name).is_none() {
        bail!("registry not found: {name}");
    }
    write_manifest(manifest_path, &manifest)?;
    status("removed registry:", name);
    Ok(())
}

fn validate_registry_name(name: &str) -> Result<()> {
    validate_skill_name(name).context("registry aliases use the same naming rules as skills")
}

/// One skill match aggregated across registries, ready for ranking
/// and display. `score` comes from the registry's `/search` response
/// (see `RegistryIndex::search` in knack-core); `None` only when
/// talking to a registry that hasn't been upgraded to send it, in
/// which case the match sorts after every scored match instead of
/// crashing or defaulting to first place.
struct FindMatch {
    skill_name: String,
    namespace: Option<String>,
    description: String,
    registry_name: String,
    score: Option<f64>,
}

fn find_registry_skills(explicit_manifest: Option<&Path>, query: &str, limit: usize) -> Result<()> {
    let query = query.trim();
    if query.is_empty() {
        bail!("find query must not be empty");
    }

    let (manifest, manifest_path) = read_manifest_for_read(explicit_manifest)?;
    let registries = effective_registries(&manifest, &manifest_path)?;
    // The registry a match came from is only worth displaying when
    // there's more than one candidate it could have come from — with
    // a single configured registry it's the same value on every
    // single card, which is pure redundancy rather than information.
    // When it IS shown, it's shown unconditionally (not just for
    // non-default registries): the explicit `<registry>:<name>` form
    // always works regardless of which registry happens to be the
    // configured default (see resolve_source_alias's alias:rest
    // branch), so once a card shows both the qualified name and its
    // registry, the exact install command is always inferable as
    // `<registry>:<qualified>` — no separate install line needed. A
    // single configured registry needs no prefix at all; the bare
    // qualified name shown in the header is already the exact command.
    let show_registry = registries
        .values()
        .filter(|registry| matches!(registry.kind, RegistryKind::Http))
        .count()
        > 1;
    let mut matches = Vec::new();
    // Two registries can surface the literal same (registry, skill)
    // pair (e.g. a registry's index listing an entry twice). Dedup on
    // that pair — the closest thing to "this is the same suggestion
    // twice" now that there's no separate formatted install string.
    let mut seen = std::collections::HashSet::new();
    let mut failed_registries = Vec::new();

    for (name, registry) in &registries {
        if !matches!(registry.kind, RegistryKind::Http) {
            continue;
        }

        // A single unreachable registry shouldn't blank out results
        // from every other configured registry — warn and keep going
        // rather than aborting the whole command.
        let results = match search_http_registry(&registry.url, query) {
            Ok(results) => results,
            Err(err) => {
                warn(&format!("registry {name} unavailable: {err:#}"));
                failed_registries.push(name.clone());
                continue;
            }
        };
        for skill in results {
            let qualified = skill.qualified_name();
            if !seen.insert((name.clone(), qualified)) {
                continue;
            }
            matches.push(FindMatch {
                skill_name: skill.name,
                namespace: skill.namespace,
                description: skill.description,
                registry_name: name.clone(),
                score: skill.score,
            });
        }
    }

    if matches.is_empty() {
        notice("no matching skills found");
        if !failed_registries.is_empty() {
            warn(&format!(
                "registries unreachable: {}",
                failed_registries.join(", ")
            ));
        }
        return Ok(());
    }

    // Rank best-match-first. Registries already return their own
    // results pre-sorted by score, but results from multiple
    // registries need a global re-sort on the merged set. Unscored
    // matches (older registry) sort after every scored match rather
    // than defaulting to 0.0, which would otherwise rank them above
    // any negative... there are no negative scores today, but this
    // keeps "unscored" visibly distinct from "scored zero" if that
    // ever changes. Ties break alphabetically by skill name for
    // stable, predictable output.
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.skill_name.cmp(&b.skill_name))
    });

    let total = matches.len();
    matches.truncate(limit);

    let count_style = accent_style();
    let label_style_hint = label_style();
    // "knack add" is spelled out once here rather than repeated on
    // every card — every card's header IS the exact thing to paste
    // after it, so there's nothing left to repeat per card.
    anstream::println!(
        "{count_style}{}{count_style:#} skill{} found {label_style_hint}— install with `knack add <name>`{label_style_hint:#}\n",
        total,
        if total == 1 { "" } else { "s" }
    );
    let wrap_width = terminal_width();
    // Each match renders as a compact two-line card — header
    // (identity, and simultaneously the exact install command),
    // description (why it matched) — with no per-field labels and no
    // blank line inside a card, only between cards. The original
    // layout gave every field ("namespace:", "description:",
    // "registry:", "install:") its own labelled row, which read as a
    // nested bulleted list; a later pass collapsed that to a header
    // plus a separate install line, but the header already contains
    // everything (qualified name, and registry when it's not the
    // sole one configured) needed to construct the install command,
    // making a separate line pure repetition. `<registry>:<name>`
    // always resolves correctly regardless of which registry happens
    // to be the configured default (resolve_source_alias's
    // `alias:rest` branch doesn't consult the default at all), so
    // there's no correctness reason to special-case the default
    // registry's entries either — every card's header is exactly the
    // command to paste after `knack add`.
    for (
        i,
        FindMatch {
            skill_name,
            namespace,
            description,
            registry_name,
            score: _,
        },
    ) in matches.into_iter().enumerate()
    {
        if i > 0 {
            anstream::println!();
        }
        // Namespace folds into the header instead of its own row —
        // "<namespace>/<name>" is exactly the form users type to
        // install, so showing it there does double duty as both
        // attribution and a preview of the install source's shape.
        let qualified = match &namespace {
            Some(ns) => format!("{ns}/{skill_name}"),
            None => skill_name,
        };
        let skill_style = accent_style();
        if show_registry {
            // Registry prefix dimmed, qualified name accented — the
            // registry is necessary information (which server this
            // came from) but the skill itself is what the reader
            // actually cares about distinguishing card-to-card, so it
            // gets the stronger accent while the prefix stays
            // secondary/structural, same as every other dimmed label.
            let label_style = label_style();
            anstream::println!(
                "{label_style}{registry_name}:{label_style:#}{skill_style}{qualified}{skill_style:#}"
            );
        } else {
            anstream::println!("{skill_style}{qualified}{skill_style:#}");
        }
        // Wrapped, unlabelled: skill descriptions are written for an
        // LLM's benefit and often run to several sentences, which
        // overflows most terminals mid-word if left unwrapped. No
        // label needed — it's the only paragraph in the card.
        print_wrapped(&description, wrap_width, 2);
    }

    if total > limit {
        anstream::println!();
        notice(&format!(
            "showing {limit} of {total} matches — pass --limit N to see more"
        ));
    }
    if !failed_registries.is_empty() {
        warn(&format!(
            "registries unreachable: {}",
            failed_registries.join(", ")
        ));
    }

    Ok(())
}

fn publish_skill(
    manifest_path: &Path,
    skill_path: &Path,
    registry_name: &str,
    repo: &str,
    skills_dir: &Path,
    no_push: bool,
) -> Result<()> {
    let skill = read_skill(skill_path)?;
    validate_skill(&skill)?;

    let manifest = read_manifest(manifest_path)?;
    let registries = effective_registries(&manifest, manifest_path)?;
    let registry = match registries.get(registry_name) {
        Some(registry) => registry,
        None => bail!(
            "no registry registered as `{registry_name}`; {}",
            unknown_registry_hint(&registries),
        ),
    };
    match registry.kind {
        RegistryKind::GitHost => {}
        RegistryKind::Http => bail!(
            "registry `{registry_name}` is configured as `http`, but \
             `knack publish` only supports git-host registries; register \
             a git-host registry with `knack registry add git+ssh://... <name>` \
             and pass that alias to `--registry`"
        ),
    }

    let repo_url = git_host_repo_url(&registry.url, repo)?;
    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let checkout = temp_dir.path().join("repo");
    run_git(
        ["clone", &repo_url, checkout.to_str().unwrap_or_default()],
        None,
        "clone publish repository",
    )?;

    let destination = checkout.join(skills_dir).join(&skill.name);
    if destination.exists() {
        fs::remove_dir_all(&destination)
            .with_context(|| format!("failed to replace {}", destination.display()))?;
    }
    copy_dir(skill_path, &destination)?;

    let source_prefix = format!(
        "{}:{}/{}",
        registry_name,
        repo.trim_matches('/'),
        skills_dir.to_string_lossy().replace('\\', "/")
    );
    generate_index(
        &checkout.join(skills_dir),
        &source_prefix,
        &checkout.join("knack.index.toml"),
    )?;

    run_git(["add", "."], Some(&checkout), "stage published skill")?;
    let status_output = ProcessCommand::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(&checkout)
        .output()
        .context("failed to inspect publish repository status")?;
    if status_output.stdout.is_empty() {
        status("nothing to publish:", skill.name);
        return Ok(());
    }

    let message = format!("Publish skill {}", skill.name);
    run_git(
        ["commit", "-m", &message],
        Some(&checkout),
        "commit published skill",
    )?;
    if no_push {
        // Persist the temp dir so the user can inspect or push the
        // generated commit manually. Without this, --no-push was nearly
        // a no-op: the working tree was deleted on function return and
        // the commit went with it.
        let persisted = temp_dir.keep();
        let persisted_checkout = persisted.join("repo");
        status("prepared publish for:", &skill.name);
        notice(&format!(
            "commit left unpushed; inspect or push from: {}",
            persisted_checkout.display(),
        ));
    } else {
        run_git(["push"], Some(&checkout), "push published skill")?;
        status("published skill:", skill.name);
    }
    Ok(())
}

fn git_host_repo_url(base_url: &str, repo: &str) -> Result<String> {
    let repo = repo.trim_matches('/');
    if repo.split('/').count() != 2 {
        bail!("--repo must be in owner/repo form");
    }

    let base_url = base_url.trim_end_matches('/');
    let base_url = base_url.strip_prefix("git+").unwrap_or(base_url);
    Ok(format!("{base_url}/{repo}.git"))
}

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
    // Capture stdout+stderr so they don't leak into the user's terminal on
    // success (git's "Cloning into 'X'..." progress and informational
    // warnings are noise for a wrapper tool). On failure, attach the
    // captured stderr to the error so the user still sees what git was
    // trying to tell them.
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

fn search_http_registry(base_url: &str, query: &str) -> Result<Vec<IndexedSkill>> {
    let base_url = base_url.trim_end_matches('/');
    let response = reqwest::blocking::Client::new()
        .get(format!("{base_url}/search"))
        .query(&[("q", query)])
        .header(reqwest::header::USER_AGENT, "knack")
        .send()
        .with_context(|| format!("failed to query {base_url}/search"))?
        .error_for_status()
        .with_context(|| format!("registry returned an error for {base_url}/search"))?;

    response
        .json()
        .context("failed to decode registry search results")
}

fn init_manifest(manifest_path: &Path, target: &Path, bootstrap_public: bool) -> Result<()> {
    if manifest_path.exists() {
        bail!("manifest already exists: {}", manifest_path.display());
    }
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut manifest = Manifest::new(target.to_path_buf());
    if bootstrap_public {
        manifest.registries.insert(
            PUBLIC_REGISTRY_NAME.to_string(),
            RegistryConfig {
                kind: RegistryKind::Http,
                url: PUBLIC_REGISTRY_URL.to_string(),
                // Ignored for HTTP registries (see RegistryConfig docs);
                // matches the default emitted by `knack registry add`.
                default_ref: "main".to_string(),
            },
        );
        // Set the seeded public registry as the default so bare
        // install commands (`knack add anthropics/pdf`) resolve
        // without users having to type the `public:` prefix. Matches
        // the cargo/npm mental model where the well-known registry
        // is the implicit default.
        manifest.install.default_registry = Some(PUBLIC_REGISTRY_NAME.to_string());
    }
    write_manifest(manifest_path, &manifest)?;
    status("created manifest:", manifest_path.display());
    if bootstrap_public {
        status(
            "seeded registry:",
            format!("{PUBLIC_REGISTRY_NAME} ({PUBLIC_REGISTRY_URL})"),
        );
        status(
            "default registry:",
            format!("{PUBLIC_REGISTRY_NAME} (bare `knack add ns/name` resolves via this)"),
        );
    }
    Ok(())
}

/// Create a default manifest at `manifest_path` with the given default target
/// when one does not already exist. No-op when the file is already present.
/// Prints a `created manifest:` status line when it creates a new file so the
/// side effect is visible to the caller.
fn ensure_manifest_exists(manifest_path: &Path, default_target: &Path) -> Result<()> {
    if manifest_path.exists() {
        return Ok(());
    }

    let manifest = Manifest::new(default_target.to_path_buf());
    write_manifest(manifest_path, &manifest)?;
    status("created manifest:", manifest_path.display());
    Ok(())
}

fn read_manifest(manifest_path: &Path) -> Result<Manifest> {
    let contents = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))
}

fn read_optional_manifest(manifest_path: &Path) -> Result<Option<Manifest>> {
    if !manifest_path.exists() {
        return Ok(None);
    }

    read_manifest(manifest_path).map(Some)
}

/// Returns a manifest suitable for read-only commands (find, registry list).
/// When the caller passed an explicit `--manifest`, the file must exist or it
/// is an error. When no `--manifest` was given, fall back to the project's
/// default path and return an empty manifest if it does not exist yet, so the
/// caller can still surface registries inherited from global and system scopes
/// via `effective_registries`. Returns the path the manifest was loaded from
/// (or would have been, if missing) so callers can pass it to
/// `effective_registries` for canonical-layer deduplication.
fn read_manifest_for_read(explicit_path: Option<&Path>) -> Result<(Manifest, PathBuf)> {
    match explicit_path {
        Some(path) => Ok((read_manifest(path)?, path.to_path_buf())),
        None => {
            let default = Scope::Project.manifest_path()?;
            let manifest =
                read_optional_manifest(&default)?.unwrap_or_else(|| Manifest::new(PathBuf::new()));
            Ok((manifest, default))
        }
    }
}

fn write_manifest(manifest_path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(manifest).context("failed to serialize manifest")?;
    fs::write(manifest_path, contents)
        .with_context(|| format!("failed to write {}", manifest_path.display()))
}

fn lockfile_path_for(manifest_path: &Path) -> PathBuf {
    manifest_path.with_file_name("knack.lock")
}

fn read_lockfile(lockfile_path: &Path) -> Result<Lockfile> {
    if !lockfile_path.exists() {
        return Ok(Lockfile::default());
    }

    let contents = fs::read_to_string(lockfile_path)
        .with_context(|| format!("failed to read {}", lockfile_path.display()))?;
    let lockfile: Lockfile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", lockfile_path.display()))?;
    // Fail loudly if the lockfile was written by a newer knack — we
    // can't safely round-trip fields we don't know about. The
    // ensure_supported_version check returns a plain String error
    // so we attach the file path context here for the user.
    lockfile
        .ensure_supported_version()
        .map_err(|message| anyhow::anyhow!("{} in {}", message, lockfile_path.display()))?;
    Ok(lockfile)
}

fn write_lockfile(lockfile_path: &Path, lockfile: &Lockfile) -> Result<()> {
    if let Some(parent) = lockfile_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(lockfile).context("failed to serialize lockfile")?;
    fs::write(lockfile_path, contents)
        .with_context(|| format!("failed to write {}", lockfile_path.display()))
}

fn upsert_lock(lockfile: &mut Lockfile, locked_skill: LockedSkill) {
    if let Some(existing) = lockfile
        .skill
        .iter_mut()
        .find(|skill| skill.name == locked_skill.name)
    {
        *existing = locked_skill;
    } else {
        lockfile.skill.push(locked_skill);
    }
    lockfile
        .skill
        .sort_by(|left, right| left.name.cmp(&right.name));
}

fn add_skill(manifest_path: &Path, source: &str, default_target: &Path) -> Result<()> {
    ensure_manifest_exists(manifest_path, default_target)?;
    let mut manifest = read_manifest(manifest_path)?;
    let lockfile_path = lockfile_path_for(manifest_path);
    let mut lockfile = read_lockfile(&lockfile_path)?;
    let resolved_source = resolve_source_alias(source, &manifest, manifest_path)?;
    let installed = install_skill(&resolved_source, &manifest.install.target)?;
    manifest
        .skills
        .insert(installed.name.clone(), source.to_string());
    let locked_resolved = installed
        .resolved_sha
        .as_deref()
        .and_then(|sha| pin_resolved_with_sha(&resolved_source, sha))
        .unwrap_or(resolved_source);
    upsert_lock(
        &mut lockfile,
        LockedSkill {
            name: installed.name.clone(),
            // Captured from the registry's X-Knack-Namespace response
            // header (set by namespacing-aware registries). None for
            // legacy registries and gh:/git+/local installs — both
            // round-trip fine through the lockfile (skip-serialize).
            namespace: installed.namespace.clone(),
            source: source.to_string(),
            resolved: locked_resolved,
            checksum: checksum_dir(&installed.path)?,
        },
    );
    write_manifest(manifest_path, &manifest)?;
    write_lockfile(&lockfile_path, &lockfile)?;
    let action_style = success_style();
    let value_style = accent_style();
    let label_style = label_style();
    anstream::println!(
        "{action_style}added skill:{action_style:#} {value_style}{}{value_style:#} {label_style}from{label_style:#} {value_style}{}{value_style:#}",
        installed.name,
        source
    );
    Ok(())
}

/// Verify the install directory and lockfile match the manifest
/// without modifying anything. Used by CI to assert that a checkout
/// of a project is in a reproducible state before running tests.
///
/// Reports each skill's status and exits non-zero on any of:
/// - Manifest entry has no matching lockfile entry.
/// - Manifest entry has a lockfile entry but the skill isn't installed.
/// - Installed skill's checksum differs from the lockfile's.
///
/// Multiple problems are collected before bailing so users can fix
/// everything in one pass instead of one round-trip per problem.
fn check_skills(manifest_path: &Path) -> Result<()> {
    let manifest = read_manifest(manifest_path)?;
    let lockfile_path = lockfile_path_for(manifest_path);
    let lockfile = read_lockfile(&lockfile_path)?;

    let mut problems: Vec<String> = Vec::new();

    for (name, source) in &manifest.skills {
        let lock_entry = lockfile
            .skill
            .iter()
            .find(|skill| skill.name == *name && skill.source == *source);
        let Some(lock_entry) = lock_entry else {
            problems.push(format!(
                "skill `{name}` is in the manifest but has no lockfile entry; \
                 run `knack sync` to regenerate"
            ));
            continue;
        };

        let install_dir = manifest.install.target.join(name);
        if !install_dir.join("SKILL.md").is_file() {
            problems.push(format!(
                "skill `{name}` is locked but not installed at {}; \
                 run `knack sync`",
                install_dir.display()
            ));
            continue;
        }

        let actual_checksum = checksum_dir(&install_dir).with_context(|| {
            format!(
                "failed to checksum installed skill at {}",
                install_dir.display()
            )
        })?;
        if actual_checksum != lock_entry.checksum {
            problems.push(format!(
                "skill `{name}` checksum drifted: lockfile says {} but install has {}",
                lock_entry.checksum, actual_checksum
            ));
            continue;
        }

        status("ok:", name);
    }

    if !problems.is_empty() {
        for problem in &problems {
            anstream::eprintln!("knack sync --check: {problem}");
        }
        bail!(
            "{} skill(s) failed sync --check; run `knack sync` to repair",
            problems.len()
        );
    }

    Ok(())
}

fn sync_skills(manifest_path: &Path) -> Result<()> {
    let manifest = read_manifest(manifest_path)?;
    let lockfile_path = lockfile_path_for(manifest_path);
    let mut lockfile = read_lockfile(&lockfile_path)?;
    fs::create_dir_all(&manifest.install.target)
        .with_context(|| format!("failed to create {}", manifest.install.target.display()))?;

    for (name, source) in &manifest.skills {
        if is_skill_installed(name, &manifest.install.target) {
            status("already installed:", name);
            continue;
        }

        // Prefer the lockfile's resolved source over re-resolving the
        // manifest source — the lockfile is the source of truth for
        // reproducibility. Fall back to fresh resolution only when there
        // is no matching lock entry (i.e. a manifest entry added by
        // hand or a brand-new clone of someone else's project).
        let from_lockfile = lockfile
            .skill
            .iter()
            .find(|skill| skill.name == *name && skill.source == *source)
            .map(|skill| skill.resolved.clone());
        let from_manifest = resolve_source_alias(source, &manifest, manifest_path)?;
        let primary = from_lockfile.unwrap_or_else(|| from_manifest.clone());

        let (installed, source_used) = install_skill_with_sha_fallback(
            &primary,
            &from_manifest,
            &manifest.install.target,
            name,
        )?;
        // Pin the lockfile against whichever URL we actually installed
        // from — that's the URL the captured SHA is meaningful relative
        // to. When the fallback fired, that's the manifest's URL with
        // its moving ref; when it didn't, it's primary unchanged.
        let locked_resolved = installed
            .resolved_sha
            .as_deref()
            .and_then(|sha| pin_resolved_with_sha(source_used, sha))
            .unwrap_or_else(|| source_used.to_string());
        upsert_lock(
            &mut lockfile,
            LockedSkill {
                name: installed.name.clone(),
                namespace: installed.namespace.clone(),
                source: source.clone(),
                resolved: locked_resolved,
                checksum: checksum_dir(&installed.path)?,
            },
        );
        status("synced skill:", installed.name);
    }

    write_lockfile(&lockfile_path, &lockfile)?;
    Ok(())
}

fn update_skills(
    manifest_path: &Path,
    force: bool,
    dry_run: bool,
    skill_filter: &[String],
) -> Result<()> {
    let manifest = read_manifest(manifest_path)?;
    let lockfile_path = lockfile_path_for(manifest_path);
    let mut lockfile = read_lockfile(&lockfile_path)?;
    fs::create_dir_all(&manifest.install.target)
        .with_context(|| format!("failed to create {}", manifest.install.target.display()))?;

    // Validate the targeted-update filter against the manifest upfront so we
    // can reject typos before doing any network work. A user running
    // `knack update deplyo-app` should see "unknown skill" immediately, not
    // a "0 skills updated" silent success.
    if !skill_filter.is_empty() {
        let unknown: Vec<&str> = skill_filter
            .iter()
            .filter(|name| !manifest.skills.contains_key(name.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let known: Vec<&str> = manifest.skills.keys().map(String::as_str).collect();
            bail!(
                "skill(s) not in manifest: {}; manifest declares: {}",
                unknown.join(", "),
                if known.is_empty() {
                    "(no skills)".to_string()
                } else {
                    known.join(", ")
                },
            );
        }
    }

    for (name, source) in &manifest.skills {
        if !skill_filter.is_empty() && !skill_filter.iter().any(|wanted| wanted == name) {
            continue;
        }
        let installed_locally = is_skill_installed(name, &manifest.install.target);

        // Always re-resolve from the manifest source — that's the whole
        // point of update — so alias-routed sources actually hit the
        // registry/git again rather than reusing the cached resolution.
        let resolved = resolve_source_alias(source, &manifest, manifest_path)?;

        // Honor pinned refs: a SHA-shaped ref is content-addressed and
        // re-fetching can't produce a different result. --force is the
        // escape hatch for 'upstream force-pushed' or 'just retry'.
        if installed_locally && !force && is_pinned_source(&resolved) {
            status("pinned skill:", name);
            continue;
        }

        let prior_checksum = lockfile
            .skill
            .iter()
            .find(|skill| skill.name == *name)
            .map(|skill| skill.checksum.clone());

        if dry_run {
            // Install into a scratch tempdir to see what we'd get, then
            // throw it away. Network cost is the same as a real update;
            // the only thing we skip is touching the install target and
            // the lockfile.
            let scratch = tempfile::tempdir()
                .context("failed to create temporary directory for --dry-run")?;
            let scratch_target = scratch.path().to_path_buf();
            let installed = install_skill(&resolved, &scratch_target)?;
            let new_checksum = checksum_dir(&installed.path)?;
            if !installed_locally {
                status("would install skill:", name);
            } else if prior_checksum.as_ref() == Some(&new_checksum) {
                status("unchanged skill:", name);
            } else {
                status("would update skill:", name);
            }
            continue;
        }

        // install_skill_dir refuses to overwrite an existing directory,
        // so clear the slate first.
        if installed_locally {
            let existing = manifest.install.target.join(name);
            fs::remove_dir_all(&existing)
                .with_context(|| format!("failed to remove {}", existing.display()))?;
        }

        let installed = install_skill(&resolved, &manifest.install.target)?;
        let new_checksum = checksum_dir(&installed.path)?;
        let locked_resolved = installed
            .resolved_sha
            .as_deref()
            .and_then(|sha| pin_resolved_with_sha(&resolved, sha))
            .unwrap_or(resolved);
        upsert_lock(
            &mut lockfile,
            LockedSkill {
                name: installed.name.clone(),
                namespace: installed.namespace.clone(),
                source: source.clone(),
                resolved: locked_resolved,
                checksum: new_checksum.clone(),
            },
        );

        if !installed_locally {
            status("synced skill:", installed.name);
        } else if prior_checksum.as_ref() == Some(&new_checksum) {
            status("unchanged skill:", installed.name);
        } else {
            status("updated skill:", installed.name);
        }
    }

    if !dry_run {
        write_lockfile(&lockfile_path, &lockfile)?;
    }
    Ok(())
}

/// Returns true when the string is shaped like a git SHA. Used by
/// fetch_git_skill to choose between a shallow `--branch` clone and a
/// full clone + checkout, and by is_pinned_source to decide whether
/// the sync loop should honor the source as pinned. Tags like `v1.0`
/// and branches like `main` deliberately do not match — they're
/// mutable references and should not be treated as pinning evidence.
fn looks_like_sha(s: &str) -> bool {
    matches!(s.len(), 7..=40) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Returns true when the source's ref pins it to immutable content
/// (i.e. is SHA-shaped). Sources without a ref concept — HTTP knack
/// registry aliases, local paths — return false: there's nothing to
/// pin against, and the sync loop should re-fetch them under
/// --update like any other moving target.
fn is_pinned_source(resolved: &str) -> bool {
    extract_source_ref(resolved)
        .as_deref()
        .is_some_and(looks_like_sha)
}

/// Returns the ref component of a gh: or git+ source. Other source
/// schemes (http+knack:, paths) have no ref concept and return None.
/// Defaults like "main" inserted by the parsers come through as
/// Some("main") rather than None, so the caller can distinguish
/// "no @ref written" from "no ref concept at all".
fn extract_source_ref(source: &str) -> Option<String> {
    if let Some(spec) = source.strip_prefix("gh:") {
        return parse_github_spec(spec).ok().map(|s| s.reference);
    }
    if source.starts_with("git+") {
        return parse_git_source(source).ok().map(|s| s.reference);
    }
    None
}

/// Rewrite a resolved source string to embed a content-addressed
/// commit SHA. The lockfile uses the rewritten form so `knack sync`
/// reinstalls from the exact commit a teammate had — even when the
/// manifest source points at a moving ref like `main`.
///
/// For `http+knack:` sources the SHA is appended as a URL fragment
/// (`#sha=...`) since the HTTP knack archive endpoint isn't
/// content-addressed at the URL level — install_http_skill_archive
/// strips the fragment before the request. The fragment is purely
/// client-side metadata: it lets `knack update` detect when the
/// registry's backing source has moved.
///
/// Returns None for local paths and other schemes with no SHA concept;
/// the caller falls back to the unpinned resolved string in that case.
fn pin_resolved_with_sha(resolved: &str, sha: &str) -> Option<String> {
    if let Some(spec_str) = resolved.strip_prefix("gh:") {
        let spec = parse_github_spec(spec_str).ok()?;
        let path = spec.skill_path.to_string_lossy().replace('\\', "/");
        return Some(format!("gh:{}/{}@{sha}/{path}", spec.owner, spec.repo));
    }
    if resolved.starts_with("git+") {
        let spec = parse_git_source(resolved).ok()?;
        let path = spec.skill_path.to_string_lossy().replace('\\', "/");
        return Some(format!("git+{}@{sha}//{path}", spec.repo_url));
    }
    if let Some(url) = resolved.strip_prefix("http+knack:") {
        // Replace any existing fragment rather than appending — running
        // `knack update` twice in a row should be idempotent rather than
        // accumulating fragments.
        let base = url.split_once('#').map(|(b, _)| b).unwrap_or(url);
        return Some(format!("http+knack:{base}#sha={sha}"));
    }
    None
}

fn resolve_source_alias(source: &str, manifest: &Manifest, manifest_path: &Path) -> Result<String> {
    if source.starts_with("gh:") || source.starts_with("git+") || Path::new(source).exists() {
        return Ok(source.to_string());
    }

    let registries = effective_registries(manifest, manifest_path)?;

    if let Some((alias, rest)) = source.split_once(':') {
        let Some(registry) = registries.get(alias) else {
            // Reject rather than falling through to install_local_skill,
            // whose "install source does not exist" diagnostic gives no
            // hint that the user probably meant a registry alias. Only
            // trigger when the prefix actually looks like a valid alias
            // name; that lets exotic local paths containing ':' continue
            // to pass through.
            if validate_registry_name(alias).is_ok() {
                bail!(
                    "no registry registered as `{alias}` (resolving source `{source}`); {}",
                    unknown_registry_hint(&registries),
                );
            }
            return Ok(source.to_string());
        };
        return match registry.kind {
            RegistryKind::GitHost => resolve_git_host_alias(registry, rest),
            RegistryKind::Http => resolve_http_alias(registry, rest),
        };
    }

    // No scheme, no `:`, not a local path — try the default registry.
    // Two accepted shapes here:
    //   `namespace/name` → resolves as `<default>:namespace/name`
    //   `name`           → resolves as `<default>:name` (registry
    //                      soft-resolves against its own namespace(s))
    // This is what enables the cargo-style ergonomic:
    //   knack add anthropics/pdf   (vs. explicit knack add public:anthropics/pdf)
    if let Some(default_alias) = effective_default_registry(manifest, manifest_path, &registries)? {
        let Some(registry) = registries.get(&default_alias) else {
            bail!(
                "default_registry `{default_alias}` is set but no registry \
                 with that alias is configured; fix `install.default_registry` \
                 in {} or run `knack registry add`",
                manifest_path.display(),
            );
        };
        return match registry.kind {
            RegistryKind::GitHost => resolve_git_host_alias(registry, source),
            RegistryKind::Http => resolve_http_alias(registry, source),
        };
    }

    // No scheme, no `:`, not a local path, no default registry. Fall
    // through — install_local_skill will produce the existing "install
    // source does not exist" error with the un-resolved string, which
    // is at least honest.
    Ok(source.to_string())
}

/// Compute the effective default registry for a bare install command:
///
/// 1. `install.default_registry` set in the manifest wins.
/// 2. Otherwise, if exactly ONE registry is configured, auto-default
///    to that. Users with a single-registry setup shouldn't have to
///    write config to get the good UX.
/// 3. Otherwise (multi-registry, no explicit default) → None. The
///    caller falls back to the "install source does not exist" error
///    with the raw source string so the user can decide whether they
///    meant a local path they forgot to prefix with `./` or a
///    registry install they need to fully qualify.
fn effective_default_registry(
    manifest: &Manifest,
    manifest_path: &Path,
    registries: &std::collections::BTreeMap<String, RegistryConfig>,
) -> Result<Option<String>> {
    // The manifest passed in is the caller's chosen scope (usually
    // project or global). Also honour any default_registry set in the
    // OTHER scope files via the same layering effective_registries()
    // uses, so a global default is visible from a project without one
    // and vice versa. Precedence: project > global > system.
    if let Some(alias) = &manifest.install.default_registry {
        return Ok(Some(alias.clone()));
    }

    for scope in [Scope::Project, Scope::Global, Scope::System] {
        let scope_path = scope.manifest_path()?;
        // Skip the manifest we already checked to avoid re-reading it.
        if scope_path == manifest_path {
            continue;
        }
        if let Ok(scoped) = read_manifest(&scope_path) {
            if let Some(alias) = scoped.install.default_registry {
                return Ok(Some(alias));
            }
        }
    }

    // No explicit default anywhere — fall back to "the one and only
    // registry" if that's what the caller has configured.
    if registries.len() == 1 {
        return Ok(Some(registries.keys().next().unwrap().clone()));
    }

    Ok(None)
}

/// Accepts two install-command shapes for HTTP registries:
///
/// - `alias:namespace/name` — the canonical form once a registry
///   has been re-materialised with namespacing. Both segments must
///   be valid kebab-case identifiers; we construct the namespaced
///   archive URL `/skills/<namespace>/<name>/archive`.
/// - `alias:name` — legacy unscoped form. We construct
///   `/skills/<name>/archive` and let the registry resolve the
///   ambiguity server-side (it 200s when exactly one namespaced
///   entry matches, 409s with a disambiguation hint when several do,
///   404s otherwise). This keeps existing manifests / lockfiles
///   working against the upgraded registry without forcing every
///   user to rewrite every `source = "alias:name"`.
fn resolve_http_alias(registry: &RegistryConfig, rest: &str) -> Result<String> {
    let path = if let Some((namespace, name)) = rest.split_once('/') {
        if name.contains('/') {
            bail!("invalid install source `{rest}`: namespace/name must contain exactly one `/`");
        }
        validate_skill_name(namespace)
            .map_err(|err| anyhow!("invalid namespace `{namespace}`: {err}"))?;
        validate_skill_name(name)?;
        format!("{namespace}/{name}")
    } else {
        validate_skill_name(rest)?;
        rest.to_string()
    };
    Ok(format!(
        "http+knack:{}/skills/{}/archive",
        registry.url.trim_end_matches('/'),
        path
    ))
}

fn effective_registries(
    manifest: &Manifest,
    manifest_path: &Path,
) -> Result<std::collections::BTreeMap<String, RegistryConfig>> {
    let mut registries = std::collections::BTreeMap::new();

    // Always read every canonical scope from disk so a project-defined
    // alias is reachable from `knack add -g`, `knack update -g`, etc.
    // The previous shape merged `system + global + passed_manifest`, so
    // when the user passed `-g` the "passed manifest" *was* the global
    // one and the project manifest at CWD was never consulted — that's
    // the bug reproduced in the maya-contigo-frontend report.
    let system_path = Scope::System.manifest_path()?;
    let global_path = Scope::Global.manifest_path()?;
    let project_path = Scope::Project.manifest_path()?;

    for path in [&system_path, &global_path, &project_path] {
        if let Some(m) = read_optional_manifest(path)? {
            registries.extend(m.registries);
        }
    }

    // If the user pointed `--manifest <custom>` at a path outside the
    // canonical scope layers, apply it as the highest-priority override.
    // When the passed path *is* one of the canonical layers (the common
    // case — scope flags resolve to canonical paths), we already absorbed
    // it above, so skip to avoid clobbering project-over-global ordering.
    let passed_canon = manifest_path.canonicalize().ok();
    let already_layered = passed_canon.as_ref().is_some_and(|p| {
        [&system_path, &global_path, &project_path]
            .iter()
            .any(|s| s.canonicalize().ok().as_ref() == Some(p))
    });
    if !already_layered {
        registries.extend(manifest.registries.clone());
    }

    Ok(registries)
}

fn resolve_git_host_alias(registry: &RegistryConfig, rest: &str) -> Result<String> {
    let mut parts = rest.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("registry source must be alias:owner/repo[@ref]/path/to/skill"))?;
    let repo_with_ref = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("registry source must include a repository"))?;
    let skill_path = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("registry source must include a skill path"))?;
    let (repo, reference) = split_repo_ref(repo_with_ref, &registry.default_ref)?;
    let base_url = registry.url.trim_end_matches('/');
    let repo_url = if base_url.starts_with("git+") {
        format!("{}/{owner}/{repo}.git", base_url.trim_start_matches("git+"))
    } else {
        format!("{base_url}/{owner}/{repo}.git")
    };

    Ok(format!("git+{repo_url}@{reference}//{skill_path}"))
}

fn is_skill_installed(name: &str, target: &Path) -> bool {
    target.join(name).join("SKILL.md").is_file()
}

#[derive(Debug)]
struct InstalledSkill {
    name: String,
    path: PathBuf,
    /// SHA captured from the upstream repository when available (gh:
    /// and git+ paths only). Used to rewrite the lockfile's `resolved`
    /// field into a content-addressed form so peers get the same
    /// commit on `knack sync`.
    resolved_sha: Option<String>,

    /// Namespace captured from the registry response (the
    /// `X-Knack-Namespace` header). Set when an HTTP registry served
    /// the archive under a namespaced or legacy URL and reported the
    /// canonical scope. Threaded into the lockfile so subsequent
    /// `knack sync` invocations construct the namespaced URL directly
    /// without the registry's bare-name fallback. None for gh:/git+/
    /// local installs, where namespacing isn't a registry concept.
    namespace: Option<String>,
}

/// Install from `primary`, but if that fails AND `primary` had a
/// SHA-shaped pinned ref AND `fallback_source` differs, warn the user
/// and retry against `fallback_source`. Returns both the installed
/// skill and the source string actually used so the caller can re-pin
/// the lockfile against the URL whose SHA was captured.
///
/// Real-world scenario this handles: a teammate's lockfile pins
/// `gh:owner/repo@<sha>/path` but the upstream branch got force-pushed
/// and that SHA no longer exists. Without fallback, `knack sync` is
/// dead in the water until someone runs `knack update --force`. With
/// fallback, sync notices the SHA is gone, re-resolves the manifest
/// source (which still has a moving ref like @main), installs that,
/// and rewrites the lockfile to whatever the new SHA is. The user
/// sees a warning so the silent shift from 'pinned' to 'latest' is
/// visible.
///
/// The fallback only triggers when `primary` is genuinely SHA-pinned
/// — branch and tag refs are intentionally NOT eligible, because
/// 'main moved' or 'a tag was retagged' are upstream issues the user
/// should know about, not auto-paper-over.
fn install_skill_with_sha_fallback<'a>(
    primary: &'a str,
    fallback_source: &'a str,
    target: &PathBuf,
    skill_name: &str,
) -> Result<(InstalledSkill, &'a str)> {
    match install_skill(primary, target) {
        Ok(installed) => Ok((installed, primary)),
        Err(err) if is_pinned_source(primary) && primary != fallback_source => {
            anstream::eprintln!(
                "knack sync: lockfile pin for `{skill_name}` is no longer reachable; \
                 falling back to manifest source `{fallback_source}` ({err:#})"
            );
            let installed = install_skill(fallback_source, target).with_context(|| {
                format!(
                    "fallback install of `{skill_name}` from manifest source also failed \
                     (the pinned lockfile ref was unreachable too)"
                )
            })?;
            Ok((installed, fallback_source))
        }
        Err(err) => Err(err),
    }
}

fn install_skill(source: &str, target: &PathBuf) -> Result<InstalledSkill> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;

    if let Some(spec) = source.strip_prefix("gh:") {
        let fetched = fetch_github_skill(spec)?;
        let mut installed = install_skill_dir(&fetched.path, target)?;
        installed.resolved_sha = fetched.resolved_sha;
        return Ok(installed);
    }

    if source.starts_with("git+") {
        let spec = parse_git_source(source)?;
        let fetched = fetch_git_skill(&spec)?;
        let mut installed = install_skill_dir(&fetched.path, target)?;
        installed.resolved_sha = fetched.resolved_sha;
        return Ok(installed);
    }

    if let Some(url) = source.strip_prefix("http+knack:") {
        return install_http_skill_archive(url, target);
    }

    let source = PathBuf::from(source);

    install_local_skill(&source, target)
}

fn install_http_skill_archive(url: &str, target: &Path) -> Result<InstalledSkill> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    // Strip any `#sha=...` fragment we may have written into the lockfile's
    // resolved URL — the fragment is purely client-side bookkeeping and
    // shouldn't be sent over the wire.
    let request_url = url.split_once('#').map(|(base, _)| base).unwrap_or(url);
    let response = reqwest::blocking::Client::new()
        .get(request_url)
        .header(reqwest::header::USER_AGENT, "knack")
        .send()
        .with_context(|| format!("failed to download {request_url}"))?;

    // Translate 404 into an actionable diagnostic. The HTTP knack URL
    // produced by resolve_http_alias has the shape
    // `http://...../skills/<name>/archive` or
    // `http://...../skills/<namespace>/<name>/archive`, so we can
    // usually recover the identifier and point the user at
    // `knack find` for discovery. Also mention the local-path
    // fallback: bare `knack add foo/bar` now resolves via the
    // default registry (see resolve_source_alias), so users who
    // meant a local dir need to prefix with `./` to disambiguate.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        let hint = match extract_archive_skill_name(request_url) {
            Some(name) => format!(
                "skill `{name}` not found on the registry; \
                 try `knack find {name}` to look for related skills, \
                 or if you meant a local directory prefix it with `./`"
            ),
            None => "the registry has no skill at that URL; \
                     try `knack find <query>` to discover skills"
                .to_string(),
        };
        bail!("{hint} ({request_url})");
    }

    // 409 Conflict comes from the namespacing soft-resolve when a
    // bare-name request matches multiple namespaced skills. The
    // registry response body carries the disambiguation hint (list
    // of qualified forms); surface it verbatim so the user sees the
    // exact namespaced identifiers they can retry with. Without
    // this, reqwest's `error_for_status()` swallows the body and
    // the user just sees "409 Conflict" — useless.
    if response.status() == reqwest::StatusCode::CONFLICT {
        let body = response
            .text()
            .unwrap_or_else(|_| "(unable to read registry response body)".to_string());
        bail!("{body} ({request_url})");
    }

    let response = response
        .error_for_status()
        .with_context(|| format!("registry returned an error for {request_url}"))?;

    // Read the X-Knack-Resolved-Sha header (set by knack-registry when
    // the archive came from a git-backed source). When present, embed
    // it in the lockfile's resolved field so peers reinstall from the
    // same content. Absent for old registries and skills_root-served
    // archives; we fall back to checksum-based change detection there.
    let resolved_sha = response
        .headers()
        .get("x-knack-resolved-sha")
        .and_then(|value| value.to_str().ok())
        .filter(|s| looks_like_sha(s))
        .map(String::from);

    // X-Knack-Namespace is the registry's report of the canonical
    // namespace under which the served archive lives. It's set when
    // the registry maps a bare `<name>` request to a namespaced
    // entry, and also when the request itself was already namespaced
    // (registry echoes the namespace verbatim). Captured into the
    // lockfile so subsequent syncs construct the namespaced URL
    // directly instead of leaning on bare-name resolution again.
    // Old registries (pre-namespacing) don't set this header; we
    // gracefully degrade to None there.
    let namespace = response
        .headers()
        .get("x-knack-namespace")
        .and_then(|value| value.to_str().ok())
        .filter(|s| validate_skill_name(s).is_ok())
        .map(String::from);

    let bytes = response.bytes().context("failed to read skill archive")?;
    let mut installed =
        install_archive_reader(flate2::read::GzDecoder::new(Cursor::new(bytes)), target)?;
    installed.resolved_sha = resolved_sha;
    installed.namespace = namespace;
    Ok(installed)
}

/// Recover the user-typed identifier from an HTTP knack archive URL.
/// Handles both URL shapes:
///   `<base>/skills/<name>/archive`             → returns `"name"`
///   `<base>/skills/<namespace>/<name>/archive` → returns `"namespace/name"`
/// Returns None for any other shape so the caller can fall back to a
/// generic hint instead of producing nonsense.
fn extract_archive_skill_name(url: &str) -> Option<&str> {
    let trimmed = url.strip_suffix("/archive")?;
    let after_skills = {
        // Find the "/skills/" segment and return whatever follows it,
        // up to the trailing slash before /archive. Searching rather
        // than splitting from the right lets us correctly handle both
        // the legacy single-segment and the new ns/name forms.
        let needle = "/skills/";
        let start = trimmed.rfind(needle)? + needle.len();
        let candidate = &trimmed[start..];
        if candidate.is_empty() || candidate.contains("//") {
            return None;
        }
        candidate
    };
    Some(after_skills)
}

fn install_archive_reader<R: std::io::Read>(reader: R, target: &Path) -> Result<InstalledSkill> {
    let mut archive = tar::Archive::new(reader);
    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    archive
        .unpack(temp_dir.path())
        .context("failed to unpack skill archive")?;
    let root = single_child_dir(temp_dir.path())?;
    install_skill_dir(&root, target)
}

fn install_local_skill(source: &Path, target: &Path) -> Result<InstalledSkill> {
    if source.is_dir() {
        return install_skill_dir(source, target);
    }

    if source.is_file() {
        let file =
            File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
        return install_archive_reader(flate2::read::GzDecoder::new(file), target);
    }

    bail!("install source does not exist: {}", source.display());
}

#[derive(Debug)]
struct GithubSpec {
    owner: String,
    repo: String,
    reference: String,
    skill_path: PathBuf,
}

#[derive(Debug)]
struct FetchedSkill {
    path: PathBuf,
    _temp_dir: TempDir,
    /// Full commit SHA the fetch ended up at, when discoverable.
    /// For gh: it comes from the archive's root directory name
    /// (`<repo>-<sha>/`); for git+ from `git rev-parse HEAD` after
    /// clone/checkout. None for archive shapes that don't expose a SHA.
    resolved_sha: Option<String>,
}

fn fetch_github_skill(spec: &str) -> Result<FetchedSkill> {
    let spec = parse_github_spec(spec)?;
    let archive_url = format!(
        "https://github.com/{}/{}/archive/{}.tar.gz",
        spec.owner, spec.repo, spec.reference
    );

    let response = reqwest::blocking::Client::new()
        .get(&archive_url)
        .header(reqwest::header::USER_AGENT, "knack")
        .send()
        .with_context(|| format!("failed to fetch {archive_url}"))?;

    // Translate 404 into an actionable diagnostic. GitHub serves 404 for
    // any missing piece: the owner, the repo, the ref, or the path within
    // the repo. We can't tell which from the archive URL alone, so name
    // the three usual suspects rather than blaming one specifically.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "GitHub returned 404 for {}/{} at ref `{}`; \
             check that the owner, repo, and ref exist and are public, \
             or that the skill path within the repo is correct ({})",
            spec.owner,
            spec.repo,
            spec.reference,
            archive_url,
        );
    }

    let response = response
        .error_for_status()
        .with_context(|| format!("GitHub returned an error for {archive_url}"))?;

    let bytes = response
        .bytes()
        .with_context(|| format!("failed to read {archive_url}"))?;
    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(temp_dir.path())
        .context("failed to unpack GitHub archive")?;

    let repo_root = single_child_dir(temp_dir.path())?;
    // GitHub's archive tarballs unpack to `<repo>-<sha>/` regardless of
    // whether the ref was a branch, tag, or SHA. Pull the SHA out for
    // the lockfile. Only accept SHA-shaped suffixes so a future change
    // to GitHub's naming (e.g. `<repo>-<tag>/`) doesn't quietly write
    // garbage into the lock.
    let resolved_sha = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(&format!("{}-", spec.repo)))
        .filter(|suffix| looks_like_sha(suffix))
        .map(String::from);
    let skill_dir = repo_root.join(&spec.skill_path);
    let skill = read_skill(&skill_dir).with_context(|| {
        format!(
            "GitHub source did not resolve to a valid skill directory: {}",
            skill_dir.display()
        )
    })?;
    validate_skill(&skill)?;

    Ok(FetchedSkill {
        path: skill_dir,
        _temp_dir: temp_dir,
        resolved_sha,
    })
}

#[derive(Debug)]
struct GitSourceSpec {
    repo_url: String,
    reference: String,
    skill_path: PathBuf,
}

fn fetch_git_skill(spec: &GitSourceSpec) -> Result<FetchedSkill> {
    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let repo_dir = temp_dir.path().join("repo");
    let repo_dir_str = repo_dir.to_str().unwrap_or_default();

    if looks_like_sha(&spec.reference) {
        // SHA refs need a full clone + checkout because `git clone
        // --branch` only accepts branch and tag names, and `--depth 1`
        // requires server-side allowReachableSHA1InWant which we can't
        // assume across hosts. Slower than the shallow path, but always
        // works.
        let clone_action = format!("clone {} for SHA ref {}", spec.repo_url, spec.reference);
        run_git(["clone", &spec.repo_url, repo_dir_str], None, &clone_action)?;
        let checkout_action = format!("check out {} in {}", spec.reference, spec.repo_url);
        run_git(
            ["checkout", "--quiet", &spec.reference],
            Some(&repo_dir),
            &checkout_action,
        )?;
    } else {
        // Branch or tag: shallow clone the named ref directly.
        let action = format!("clone {} at ref {}", spec.repo_url, spec.reference);
        run_git(
            [
                "clone",
                "--depth",
                "1",
                "--branch",
                &spec.reference,
                &spec.repo_url,
                repo_dir_str,
            ],
            None,
            &action,
        )?;
    }

    // Capture the SHA we ended up at so the lockfile can pin to
    // content rather than a moving ref. For a SHA-shaped ref this is
    // redundant (the ref *is* the SHA), but we capture uniformly so
    // branches and tags also get pinned in the lockfile.
    let resolved_sha = capture_git_head_sha(&repo_dir).ok();

    let skill_dir = repo_dir.join(&spec.skill_path);
    let skill = read_skill(&skill_dir).with_context(|| {
        format!(
            "Git source did not resolve to a valid skill directory: {}",
            skill_dir.display()
        )
    })?;
    validate_skill(&skill)?;

    Ok(FetchedSkill {
        path: skill_dir,
        _temp_dir: temp_dir,
        resolved_sha,
    })
}

/// Run `git rev-parse HEAD` in `repo_dir` and return the full
/// 40-char SHA. Returns Err on any failure (missing git, detached
/// state weirdness, etc.); callers treat that as 'no SHA available'.
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

fn parse_git_source(source: &str) -> Result<GitSourceSpec> {
    let (repo_part, skill_path) = source
        .rsplit_once("//")
        .ok_or_else(|| anyhow!("git source must be git+<url>[@ref]//path/to/skill"))?;
    let skill_path = skill_path.trim_matches('/');
    if skill_path.is_empty() {
        bail!("git source must include a skill path after //");
    }

    let without_scheme = repo_part
        .strip_prefix("git+")
        .ok_or_else(|| anyhow!("git source must start with git+"))?;
    let (repo_url, reference) = split_repo_ref(without_scheme, "main")?;

    Ok(GitSourceSpec {
        repo_url: repo_url.to_string(),
        reference: reference.to_string(),
        skill_path: PathBuf::from(skill_path),
    })
}

fn split_repo_ref<'a>(repo_with_ref: &'a str, default_ref: &'a str) -> Result<(&'a str, &'a str)> {
    let at_position = repo_with_ref.rfind('@');
    let Some(position) = at_position else {
        return Ok((repo_with_ref, default_ref));
    };

    let scheme_position = repo_with_ref.find("://");
    if scheme_position.is_some_and(|scheme| position < scheme) {
        return Ok((repo_with_ref, default_ref));
    }

    let (repo, reference_with_at) = repo_with_ref.split_at(position);
    let reference = &reference_with_at[1..];
    if repo.is_empty() || reference.is_empty() {
        bail!("git source repository and ref must not be empty");
    }

    Ok((repo, reference))
}

fn parse_github_spec(spec: &str) -> Result<GithubSpec> {
    let mut parts = spec.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("GitHub source must be gh:owner/repo[@ref]/path/to/skill"))?;
    let repo_with_ref = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("GitHub source must include a repository"))?;
    let skill_path = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow!("GitHub source must include a skill path"))?;

    let (repo, reference) = repo_with_ref
        .split_once('@')
        .unwrap_or((repo_with_ref, "main"));

    if repo.is_empty() || reference.is_empty() {
        bail!("GitHub source repository and ref must not be empty");
    }

    Ok(GithubSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        reference: reference.to_string(),
        skill_path: PathBuf::from(skill_path),
    })
}

fn single_child_dir(path: &Path) -> Result<PathBuf> {
    let mut children = fs::read_dir(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
        })
        .map(|entry| entry.path());

    let child = children
        .next()
        .ok_or_else(|| anyhow!("GitHub archive did not contain a repository directory"))?;
    if children.next().is_some() {
        bail!("GitHub archive contained multiple repository directories");
    }
    Ok(child)
}

fn install_skill_dir(source: &Path, target: &Path) -> Result<InstalledSkill> {
    let skill = read_skill(source)?;
    validate_skill(&skill)?;
    let destination = target.join(&skill.name);
    if destination.exists() {
        bail!("skill already installed: {}", destination.display());
    }
    copy_dir(source, &destination)?;
    Ok(InstalledSkill {
        name: skill.name,
        path: destination,
        resolved_sha: None,
        // Local / gh: / git+ installs aren't served by a registry, so
        // there's no X-Knack-Namespace to capture. The HTTP install
        // path (install_http_skill_archive) sets this from the
        // response header before returning.
        namespace: None,
    })
}

fn list_skills(explicit_target: Option<&Path>) -> Result<()> {
    if let Some(target) = explicit_target {
        let skills = collect_installed_skills(target)?;
        print_skill_names(&skills);
        return Ok(());
    }

    let project_target = Scope::Project.install_target()?;
    let global_target = Scope::Global.install_target()?;
    let project_skills = collect_installed_skills(&project_target)?;
    let global_skills = collect_installed_skills(&global_target)?;

    if project_skills.is_empty() && global_skills.is_empty() {
        notice("no skills installed");
        return Ok(());
    }

    let mut printed_section = false;
    if !project_skills.is_empty() {
        print_skills_section("project", &project_target, &project_skills);
        printed_section = true;
    }
    if !global_skills.is_empty() {
        if printed_section {
            anstream::println!();
        }
        print_skills_section("global", &global_target, &global_skills);
    }

    Ok(())
}

fn collect_installed_skills(target: &Path) -> Result<Vec<String>> {
    if !target.exists() {
        return Ok(Vec::new());
    }

    let mut skills = Vec::new();
    for entry in
        fs::read_dir(target).with_context(|| format!("failed to read {}", target.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            let skill = read_skill(&path)?;
            validate_skill(&skill)?;
            skills.push(skill.name);
        }
    }

    skills.sort();
    Ok(skills)
}

fn print_skill_names(skills: &[String]) {
    let skill_style = accent_style();
    for skill in skills {
        anstream::println!("{skill_style}{skill}{skill_style:#}");
    }
}

fn print_skills_section(label: &str, target: &Path, skills: &[String]) {
    let heading_style = label_style();
    let path_style = label_style();
    let skill_style = accent_style();
    anstream::println!(
        "{heading_style}{label}{heading_style:#} {path_style}({}){path_style:#}",
        target.display()
    );
    for name in skills {
        anstream::println!("  {skill_style}{name}{skill_style:#}");
    }
}

fn copy_dir(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn pack_skill(path: &PathBuf, output: &PathBuf) -> Result<PathBuf> {
    let skill = read_skill(path)?;
    validate_skill(&skill)?;

    fs::create_dir_all(output).with_context(|| format!("failed to create {}", output.display()))?;
    let archive_path = output.join(format!("{}.skill.tar.gz", skill.name));
    let archive_file = File::create(&archive_path)
        .with_context(|| format!("failed to create {}", archive_path.display()))?;
    let encoder = GzEncoder::new(archive_file, Compression::default());
    let mut archive = Builder::new(encoder);

    let files = collect_files(path)?;
    for file in files {
        let relative_path = file.strip_prefix(path).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                file.display(),
                path.display()
            )
        })?;
        let archive_name = Path::new(&skill.name).join(relative_path);
        append_file(&mut archive, &file, &archive_name)?;
    }

    archive.finish()?;
    Ok(archive_path)
}

fn append_file(
    archive: &mut Builder<GzEncoder<File>>,
    source: &Path,
    archive_name: &Path,
) -> Result<()> {
    let mut file =
        File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", source.display()))?;

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

fn new_skill(name: &str, dir: PathBuf) -> Result<()> {
    validate_skill_name(name)?;

    let skill_dir = dir.join(name);
    if skill_dir.exists() {
        bail!("skill directory already exists: {}", skill_dir.display());
    }

    fs::create_dir_all(&skill_dir)
        .with_context(|| format!("failed to create {}", skill_dir.display()))?;

    let skill_file = skill_dir.join("SKILL.md");
    let content = format!(
        "---\nname: {name}\ndescription: \"TODO: Describe what this skill does and when to use it.\"\n---\n\n# {name}\n\nWrite concise instructions for the agent here.\n"
    );

    fs::write(&skill_file, content)
        .with_context(|| format!("failed to write {}", skill_file.display()))?;

    status("created skill:", skill_dir.display());
    Ok(())
}
