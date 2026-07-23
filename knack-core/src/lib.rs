use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(feature = "archive")]
mod archive;
#[cfg(feature = "archive")]
pub use archive::{create_skill_archive, unpack_skill_archive};

#[derive(Debug)]
pub struct Skill {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
}

/// Agent Skills frontmatter knack understands. Unknown fields are
/// silently ignored: the SKILL.md format is shared across tooling
/// (Anthropic's catalog, third-party agents, internal extensions),
/// and other tools write fields like `hidden` that knack has no use
/// for. Rejecting them broke `knack list` whenever a foreign skill
/// landed in `.agents/skills/` — the user couldn't list any skill
/// just because one of them had an extra field.
///
/// Required fields (`name`, `description`) still fail loudly on
/// typos: serde reports "missing field `description`" rather than
/// silently accepting `desciption`. Typos in optional fields will
/// silently be dropped, which is the usual YAML/serde convention
/// and the acceptable tradeoff for ecosystem interop.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    metadata: Option<serde_yaml::Mapping>,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: Option<String>,
}

pub fn read_skill(path: &Path) -> Result<Skill> {
    if !path.is_dir() {
        bail!("skill path is not a directory: {}", path.display());
    }

    let skill_file = path.join("SKILL.md");
    let contents = fs::read_to_string(&skill_file)
        .with_context(|| format!("failed to read {}", skill_file.display()))?;
    let frontmatter = parse_frontmatter(&contents)
        .with_context(|| format!("failed to parse {}", skill_file.display()))?;

    Ok(Skill {
        path: path.to_path_buf(),
        name: frontmatter.name,
        description: frontmatter.description,
    })
}

fn parse_frontmatter(contents: &str) -> Result<SkillFrontmatter> {
    let mut lines = contents.lines();
    if lines.next() != Some("---") {
        bail!("SKILL.md must start with YAML frontmatter delimited by ---");
    }

    let mut yaml = String::new();
    for line in lines {
        if line == "---" {
            let frontmatter = serde_yaml::from_str(&yaml)?;
            return Ok(frontmatter);
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    bail!("SKILL.md frontmatter is missing closing ---");
}

/// Validate a skill's identity (name well-formed, description present)
/// without requiring the on-disk directory to match the frontmatter
/// name. Use this when reading skills from an upstream source whose
/// layout we don't control — e.g. when materialising a registry's
/// dynamic `[[source]]` entries against third-party repos. Vendors
/// commonly use unprefixed directory names with brand-prefixed
/// frontmatter names (e.g. `skills/composition-patterns/` containing
/// `name: vercel-composition-patterns`); that's a legitimate
/// convention and the registry's archive flow already renames on
/// the way out using the frontmatter name.
pub fn validate_skill_metadata(skill: &Skill) -> Result<()> {
    validate_skill_name(&skill.name)?;

    if skill.description.trim().is_empty() {
        bail!("description must not be empty");
    }

    // We previously rejected descriptions over 1024 characters, but
    // real-world Agent Skills (notably anthropics/skills) include
    // multi-paragraph "use when..." prose well past that ceiling
    // (skill-creator runs to ~5200 chars). knack should not gatekeep
    // the format the broader ecosystem uses; UIs are free to truncate
    // long descriptions at display time. Required fields stay strict.

    Ok(())
}

/// Strict validation for skills installed locally or being authored.
/// In addition to the metadata checks, the on-disk directory name
/// must match the frontmatter name — this is a local-filesystem
/// invariant for `.agents/skills/<name>/` lookup. Use this from
/// `knack install`, `knack new`, `knack validate`, and the publish
/// flow.
pub fn validate_skill(skill: &Skill) -> Result<()> {
    validate_skill_metadata(skill)?;

    let dirname = skill
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("skill path has no valid directory name"))?;
    if dirname != skill.name {
        bail!(
            "skill name must match directory name: frontmatter has {:?}, directory is {:?}",
            skill.name,
            dirname
        );
    }

    Ok(())
}

pub fn validate_skill_name(name: &str) -> Result<()> {
    let len = name.chars().count();
    if len == 0 || len > 64 {
        bail!("skill name must be 1-64 characters");
    }

    if name.starts_with('-') || name.ends_with('-') {
        bail!("skill name must not start or end with a hyphen");
    }

    if name.contains("--") {
        bail!("skill name must not contain consecutive hyphens");
    }

    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("skill name may only contain lowercase letters, numbers, and hyphens");
    }

    Ok(())
}

pub fn checksum_dir(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for file in collect_files(path)? {
        let relative_path = file.strip_prefix(path).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                file.display(),
                path.display()
            )
        })?;
        hasher.update(relative_path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher
            .update(fs::read(&file).with_context(|| format!("failed to read {}", file.display()))?);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn collect_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files_inner(path, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_inner(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            collect_files_inner(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub install: InstallConfig,
    #[serde(default)]
    pub skills: BTreeMap<String, String>,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryConfig>,
}

impl Manifest {
    pub fn new(target: PathBuf) -> Self {
        Self {
            install: InstallConfig {
                target,
                default_registry: None,
            },
            skills: BTreeMap::new(),
            registries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallConfig {
    pub target: PathBuf,

    /// Registry alias used to resolve bare install sources that omit
    /// the `<registry>:` prefix. Set via `knack init` (defaults to
    /// `"public"` when the public registry is seeded) or by hand.
    ///
    /// With this set to `"public"`, `knack add anthropics/pdf`
    /// resolves the same as `knack add public:anthropics/pdf`, and
    /// `knack add pdf` resolves as `knack add public:pdf` (which the
    /// registry then soft-resolves under its own namespace via
    /// X-Knack-Namespace). None means "no implicit default"; the CLI
    /// falls back to auto-defaulting when exactly one registry is
    /// configured, and errors on ambiguity otherwise.
    ///
    /// Layered through the system → global → project scopes like
    /// `[registries.*]`, last-write-wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
}

/// Current lockfile schema version. Bump when the on-disk layout
/// changes in a way an older `knack` couldn't safely interpret.
///
/// Backward compatibility is one-way: a new knack reading an old
/// lockfile should keep working (with default values for new fields).
/// An old knack reading a new lockfile errors loudly rather than
/// guessing — that's what `Lockfile::ensure_supported_version`
/// enforces.
///
/// Version history
/// - **v1**: initial layout. `name`, `source`, `resolved`, `checksum`.
///   Lockfiles with no `version` field at all are also v1 (the field
///   only became required when v2 landed).
/// - **v2**: adds optional `namespace` per locked skill so namespaced
///   registries (`public:anthropics/pdf`) can round-trip through the
///   lockfile without losing the vendor scope. Old v1 entries with no
///   `namespace` field continue to read fine into v2; new writes emit
///   `version = 2` and include the field when scoped.
pub const LOCKFILE_VERSION: u32 = 2;

fn default_lockfile_version() -> u32 {
    // Missing-version means "written before this field existed" → v1
    // by definition, not the current latest. Without this we'd
    // silently promote untouched v1 files to whatever LOCKFILE_VERSION
    // happens to be today, masking actual version skew.
    1
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Lockfile {
    /// Schema version. Older lockfiles without this field default to
    /// version 1 since v1 is identical to the pre-versioned layout —
    /// only the SHA-pinning convention differs, and that's transparent
    /// to the parser.
    #[serde(default = "default_lockfile_version")]
    pub version: u32,
    #[serde(default)]
    pub skill: Vec<LockedSkill>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            skill: Vec::new(),
        }
    }
}

impl Lockfile {
    /// Refuse to operate on a lockfile from a future knack version.
    /// New schema versions may add fields or change semantics in ways
    /// this binary can't preserve on round-trip; bailing avoids
    /// silently corrupting the file when we write it back.
    pub fn ensure_supported_version(&self) -> Result<(), String> {
        if self.version > LOCKFILE_VERSION {
            return Err(format!(
                "lockfile version {} is newer than this knack supports (max {LOCKFILE_VERSION}); upgrade knack",
                self.version
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LockedSkill {
    pub name: String,

    /// Vendor scope for namespaced registries (lockfile v2+). None
    /// for legacy unscoped entries written by knack 0.2.x or skills
    /// installed from unnamespaced sources (gh:/git+/local paths).
    /// `skip_serializing_if = "Option::is_none"` keeps legacy entries
    /// from gaining a noisy `namespace = ""` on round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    pub source: String,
    pub resolved: String,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegistryConfig {
    pub kind: RegistryKind,
    pub url: String,
    pub default_ref: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryKind {
    GitHost,
    Http,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RegistryIndex {
    #[serde(default)]
    pub skill: Vec<IndexedSkill>,
    #[serde(default)]
    pub source: Vec<IndexSource>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexedSkill {
    pub name: String,

    /// Vendor-scoping prefix added at materialize time so two skills
    /// sharing a bare name (e.g. `find-skills` exists in both
    /// vercel-labs/skills and ajac-zero/knack) can coexist in one
    /// registry without colliding. None means "unscoped" — supported
    /// for backward compatibility with pre-namespacing index files;
    /// new entries written by `knack-registry build-static` or
    /// `materialize` always carry one. Validated against the same
    /// kebab-case rules as `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    pub description: String,

    /// The install command suffix that follows `<registry>:`. For
    /// namespaced entries this is `<namespace>/<name>`; for legacy
    /// unscoped entries it's just `<name>`. Always matches what
    /// `qualified_name()` returns.
    pub source: String,

    #[serde(default)]
    pub tags: Vec<String>,

    /// Relevance score assigned by `RegistryIndex::search`. Only ever
    /// set on results returned from a search — never persisted in
    /// `knack.index.toml` and never present on entries read from
    /// `/index`, so this is skipped on serialization whenever `None`.
    /// Kept `#[serde(default)]` on the way in so index files written
    /// before this field existed (and any registry that hasn't
    /// upgraded yet) still deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexSource {
    pub source: String,

    /// Optional explicit namespace override for skills materialised
    /// from this source. When set, all skills walked under this
    /// source's tree get scoped as `<namespace>/<skill-name>`. When
    /// omitted, knack-registry derives a namespace from the source
    /// URL itself (typically the gh:owner segment). The override
    /// matters for cases like `gh:ajac-zero/knack/skills` where the
    /// owner segment ("ajac-zero") isn't the brand we want users to
    /// install under ("knack"). Same kebab-case rules as skill names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(default)]
    pub tags: Vec<String>,
}

impl RegistryIndex {
    pub fn validate(&self) -> Result<()> {
        for skill in &self.skill {
            skill.validate()?;
        }
        for source in &self.source {
            source.validate()?;
        }
        Ok(())
    }

    /// Scores and ranks skills against a whitespace-separated query.
    /// Every term must match *somewhere* in a skill (name, namespace,
    /// description, or tags) for the skill to be included at all —
    /// same AND semantics as before. Where a term matched affects
    /// ranking (a hit in the name or tags counts for much more than
    /// a hit buried in the description), and so does *how common*
    /// the term is across the whole index: a generic word that shows
    /// up in most skills' descriptions (e.g. "deploy") contributes
    /// far less to the score than a term that's genuinely
    /// distinctive, via a BM25-style inverse-document-frequency
    /// weight computed per query. This keeps a broad, common query
    /// term from single-handedly promoting every skill that happens
    /// to mention it once in passing. Results are sorted
    /// best-match-first (ties broken alphabetically by
    /// `qualified_name()` for stable output).
    pub fn search(&self, query: &str) -> Vec<(&IndexedSkill, f64)> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let total_skills = self.skill.len();
        // Document frequency per term: how many skills the term
        // matches *somewhere*, independent of the other query terms.
        // This is what lets a term's own weight reflect how
        // discriminating it is across this specific index, rather
        // than using a single fixed weight for every term regardless
        // of how common it is.
        let idf_weights: Vec<f64> = terms
            .iter()
            .map(|term| {
                let doc_freq = self
                    .skill
                    .iter()
                    .filter(|skill| skill.matches_term(term))
                    .count();
                inverse_document_frequency(total_skills, doc_freq)
            })
            .collect();

        let mut scored: Vec<(&IndexedSkill, f64)> = self
            .skill
            .iter()
            .filter_map(|skill| {
                skill
                    .match_score(&terms, &idf_weights)
                    .map(|score| (skill, score))
            })
            .collect();

        scored.sort_by(|(a, a_score), (b, b_score)| {
            b_score
                .partial_cmp(a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.qualified_name().cmp(&b.qualified_name()))
        });
        scored
    }
}

/// BM25-style smoothed inverse document frequency: `ln(1 + (N - df +
/// 0.5) / (df + 0.5))`. A term present in only a handful of skills
/// (small `df`) gets a large weight; a term present in most or all
/// skills (large `df`, up to `N`) gets a small-but-positive weight —
/// it never zeroes out entirely (a query term still had to match for
/// the skill to be included at all via the AND filter in
/// `match_score`), but it stops dominating the ranking the way a
/// flat per-field weight would. `df` is expected to be at least 1
/// here (only called for terms that matched something); `N == 0` or
/// `df == 0` fall back to a neutral weight of `1.0` as a defensive
/// guard rather than dividing by zero.
fn inverse_document_frequency(total_skills: usize, doc_freq: usize) -> f64 {
    if total_skills == 0 || doc_freq == 0 {
        return 1.0;
    }
    let n = total_skills as f64;
    let df = doc_freq as f64;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

impl IndexSource {
    pub fn validate(&self) -> Result<()> {
        if self.source.trim().is_empty() {
            bail!("indexed source must not be empty");
        }
        Ok(())
    }
}

impl IndexedSkill {
    pub fn validate(&self) -> Result<()> {
        validate_skill_name(&self.name)?;
        if let Some(ns) = &self.namespace {
            // Namespaces use the same character set as skill names —
            // kebab-case for URL-safe round-tripping through
            // `/skills/<ns>/<name>/archive`.
            validate_skill_name(ns).map_err(|err| anyhow!("invalid namespace: {err}"))?;
        }
        if self.description.trim().is_empty() {
            bail!("indexed skill description must not be empty: {}", self.name);
        }
        if self.source.trim().is_empty() {
            bail!("indexed skill source must not be empty: {}", self.name);
        }
        Ok(())
    }

    /// `<namespace>/<name>` when scoped, bare `<name>` otherwise. This
    /// is the on-the-wire identifier — what comes after `<registry>:`
    /// in install commands, and what's used as the archive URL path
    /// segment.
    pub fn qualified_name(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}/{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Scores a single already-lowercased term against one field,
    /// choosing the highest tier the term qualifies for in that
    /// field. `is_name_or_tag` widens the "whole word" and "starts
    /// with" tiers to also apply to tags/namespace, since those are
    /// short identifier-like strings where a whole-word hit is just
    /// as meaningful as an exact name match, unlike the free-form
    /// description field.
    fn field_weight(term: &str, field: &str, is_name_or_tag: bool) -> f64 {
        if field.is_empty() {
            return 0.0;
        }
        if is_name_or_tag {
            if field == term {
                return 4.0;
            }
            if field.starts_with(term) {
                return 3.0;
            }
            if word_boundary_match(field, term) {
                return 2.0;
            }
        }
        if field.contains(term) {
            if is_name_or_tag { 1.0 } else { 0.5 }
        } else {
            0.0
        }
    }

    /// Best weight found for one already-lowercased `term` across
    /// name/namespace/tags/description, or `0.0` if it matches
    /// nowhere. Factored out of `match_score` so document-frequency
    /// counting (`matches_term`) and scoring can share the same
    /// per-field logic instead of drifting apart.
    fn best_field_weight(&self, term: &str) -> f64 {
        let name = self.name.to_ascii_lowercase();
        let namespace = self
            .namespace
            .as_deref()
            .map(|ns| ns.to_ascii_lowercase())
            .unwrap_or_default();
        let description = self.description.to_ascii_lowercase();

        let mut best = Self::field_weight(term, &name, true);
        best = best.max(Self::field_weight(term, &namespace, true));
        for tag in &self.tags {
            best = best.max(Self::field_weight(term, &tag.to_ascii_lowercase(), true));
        }
        best.max(Self::field_weight(term, &description, false))
    }

    /// Whether an already-lowercased `term` matches this skill
    /// anywhere at all, independent of any other query terms. Used
    /// to compute each term's document frequency across the whole
    /// index for IDF weighting.
    fn matches_term(&self, term: &str) -> bool {
        self.best_field_weight(term) > 0.0
    }

    /// Sums, per term, the best field weight found across
    /// name/namespace/tags/description, scaled by that term's IDF
    /// weight (`idf_weights[i]`, aligned by position with `terms`)
    /// so common terms contribute less than distinctive ones.
    /// Returns `None` if any term matched nowhere (preserving the
    /// AND-of-substrings filtering behaviour), `Some(total_score)`
    /// otherwise.
    fn match_score(&self, terms: &[String], idf_weights: &[f64]) -> Option<f64> {
        let mut total = 0.0;
        for (term, idf) in terms.iter().zip(idf_weights) {
            let best = self.best_field_weight(term);
            if best <= 0.0 {
                return None;
            }
            total += best * idf;
        }
        Some(total)
    }
}

/// True if `term` appears in `field` as a whole word — surrounded by
/// non-alphanumeric boundaries (or the string edges). Used to rank a
/// standalone-word hit above a mid-word substring hit (e.g. "pdf" in
/// tag "pdf" vs. tag "pdf-export") without requiring an exact match.
fn word_boundary_match(field: &str, term: &str) -> bool {
    let mut start = 0;
    while let Some(idx) = field[start..].find(term) {
        let match_start = start + idx;
        let match_end = match_start + term.len();
        let before_ok = match_start == 0
            || !field[..match_start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after_ok = match_end == field.len()
            || !field[match_end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = match_start + 1;
        if start >= field.len() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_skill_names() {
        assert!(validate_skill_name("rust-code-review").is_ok());
        assert!(validate_skill_name("Rust-Code-Review").is_err());
        assert!(validate_skill_name("-rust").is_err());
        assert!(validate_skill_name("rust-").is_err());
        assert!(validate_skill_name("rust--review").is_err());
    }

    #[test]
    fn parses_frontmatter() {
        let frontmatter =
            parse_frontmatter("---\nname: demo-skill\ndescription: Use for demos.\n---\n\nBody\n")
                .expect("frontmatter should parse");

        assert_eq!(frontmatter.name, "demo-skill");
        assert_eq!(frontmatter.description, "Use for demos.");
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse_frontmatter("# demo\n").is_err());
    }

    #[test]
    fn tolerates_unknown_frontmatter_fields() {
        // Reported by a user whose `knack list` blew up on agent-browser's
        // SKILL.md because it set `hidden: true`. Knack doesn't model that
        // field — and shouldn't have to — so parsing must skip past it.
        let frontmatter = parse_frontmatter(
            "---\n\
             name: agent-browser\n\
             description: Browser automation.\n\
             allowed-tools: Bash(agent-browser:*)\n\
             hidden: true\n\
             custom-field: arbitrary\n\
             ---\n",
        )
        .expect("foreign fields must be ignored, not rejected");
        assert_eq!(frontmatter.name, "agent-browser");
        assert_eq!(frontmatter.description, "Browser automation.");
        assert_eq!(
            frontmatter.allowed_tools.as_deref(),
            Some("Bash(agent-browser:*)")
        );
    }

    #[test]
    fn still_requires_name_and_description() {
        // Removing deny_unknown_fields shouldn't make typos in REQUIRED
        // fields silent. Missing `description` should still fail.
        assert!(parse_frontmatter("---\nname: x\ndesciption: oops\n---\n").is_err());
    }

    #[test]
    fn validate_skill_metadata_ignores_directory_mismatch() {
        // Vendors like Vercel and Remotion use unprefixed dirs with
        // brand-prefixed frontmatter names — e.g.
        // skills/composition-patterns/SKILL.md whose frontmatter
        // declares `name: vercel-composition-patterns`. That's a
        // legitimate convention and a registry indexing them must
        // not reject these. validate_skill (strict) still does.
        let skill = Skill {
            path: PathBuf::from("/tmp/composition-patterns"),
            name: "vercel-composition-patterns".to_string(),
            description: "React composition patterns.".to_string(),
        };
        assert!(validate_skill_metadata(&skill).is_ok());
        assert!(validate_skill(&skill).is_err());
    }

    #[test]
    fn accepts_long_descriptions() {
        // Real ecosystem SKILL.md files (anthropics/skills/skill-creator
        // is ~5200 chars) bury invocation guidance in description. The
        // old 1024-char ceiling locked us out of indexing them. Empty
        // descriptions still fail; length no longer does.
        let long = "Use when the user is doing things. ".repeat(200);
        assert!(long.len() > 1024);
        let skill = Skill {
            path: PathBuf::from("/tmp/example"),
            name: "example".to_string(),
            description: long,
        };
        assert!(validate_skill(&skill).is_ok());

        let blank = Skill {
            path: PathBuf::from("/tmp/example"),
            name: "example".to_string(),
            description: "   ".to_string(),
        };
        assert!(validate_skill(&blank).is_err());
    }

    #[test]
    fn searches_registry_index() {
        let index = RegistryIndex {
            skill: vec![
                IndexedSkill {
                    name: "pdf".to_string(),
                    namespace: Some("anthropics".to_string()),
                    description: "Work with PDF documents".to_string(),
                    source: "anthropics/pdf".to_string(),
                    tags: vec!["documents".to_string(), "ocr".to_string()],
                    score: None,
                },
                IndexedSkill {
                    name: "rust-code-review".to_string(),
                    namespace: None,
                    description: "Review Rust code".to_string(),
                    source: "rust-code-review".to_string(),
                    tags: vec!["rust".to_string()],
                    score: None,
                },
            ],
            source: Vec::new(),
        };

        assert_eq!(index.search("pdf").len(), 1);
        assert_eq!(index.search("documents ocr").len(), 1);
        assert_eq!(index.search("python").len(), 0);
        // Namespace itself is searchable so users can scope by vendor:
        // `knack find anthropics` lists everything from that vendor.
        assert_eq!(index.search("anthropics").len(), 1);
    }

    #[test]
    fn ranks_name_matches_above_description_only_matches() {
        // Both skills mention "rust" somewhere, but only one is
        // actually named/tagged for it. The name/tag hit must
        // outrank the incidental description mention so a query for
        // "rust" doesn't bury the relevant skill under unrelated
        // results that merely reference it in passing.
        let index = RegistryIndex {
            skill: vec![
                IndexedSkill {
                    name: "changelog-writer".to_string(),
                    namespace: None,
                    description: "Summarize commits, including ones touching Rust code."
                        .to_string(),
                    source: "changelog-writer".to_string(),
                    tags: vec![],
                    score: None,
                },
                IndexedSkill {
                    name: "rust-code-review".to_string(),
                    namespace: None,
                    description: "Review code for correctness".to_string(),
                    source: "rust-code-review".to_string(),
                    tags: vec!["rust".to_string()],
                    score: None,
                },
            ],
            source: Vec::new(),
        };

        let results = index.search("rust");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0.name, "rust-code-review");
        assert!(results[0].1 > results[1].1);
    }

    #[test]
    fn discounts_common_terms_against_rare_terms_via_idf() {
        // Two-term query "ci deploy" where "ci" is rare across the
        // index but "deploy" is common (mentioned in passing by many
        // unrelated skills' descriptions). Skill `x` matches the
        // *rare* term strongly (exact tag) and the common term only
        // weakly (description substring); skill `y` is the mirror
        // image — strong on the *common* term, weak on the rare one.
        // Their flat, un-weighted field scores are equal (4.0 + 0.5
        // each way), so without IDF weighting they'd tie. With IDF,
        // `x`'s strong hit on the rarer, more discriminating term
        // should outrank `y`'s strong hit on the term that's common
        // enough to appear almost everywhere.
        let mut skill = vec![
            IndexedSkill {
                name: "ci-tools".to_string(),
                namespace: None,
                description: "Helps deploy your pipeline safely.".to_string(),
                source: "ci-tools".to_string(),
                tags: vec!["ci".to_string()],
                score: None,
            },
            IndexedSkill {
                name: "deploy".to_string(),
                namespace: None,
                description: "Also handles some ci related tasks.".to_string(),
                source: "deploy".to_string(),
                tags: vec!["deploy".to_string()],
                score: None,
            },
        ];
        // Filler skills that mention "deploy" in passing (inflating
        // its document frequency) but never "ci", so "ci" stays rare
        // relative to "deploy" across the whole index.
        for i in 0..15 {
            skill.push(IndexedSkill {
                name: format!("filler-{i}"),
                namespace: None,
                description: "Handles deployment automation for unrelated workflows.".to_string(),
                source: format!("filler-{i}"),
                tags: vec![],
                score: None,
            });
        }
        let index = RegistryIndex {
            skill,
            source: Vec::new(),
        };

        let results = index.search("ci deploy");
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].0.name, "ci-tools",
            "strong match on the rarer, more discriminating term should outrank \
             a strong match on the term that's common across the index"
        );
        assert!(results[0].1 > results[1].1);
    }

    #[test]
    fn requires_every_term_to_match_somewhere() {
        // AND semantics preserved: a skill matching only one of two
        // terms must be excluded entirely, not merely ranked lower.
        let index = RegistryIndex {
            skill: vec![IndexedSkill {
                name: "pdf".to_string(),
                namespace: Some("anthropics".to_string()),
                description: "Work with PDF documents".to_string(),
                source: "anthropics/pdf".to_string(),
                tags: vec!["documents".to_string()],
                score: None,
            }],
            source: Vec::new(),
        };

        assert_eq!(index.search("pdf python").len(), 0);
        assert_eq!(index.search("pdf documents").len(), 1);
    }

    #[test]
    fn ties_break_alphabetically_by_qualified_name() {
        let index = RegistryIndex {
            skill: vec![
                IndexedSkill {
                    name: "zeta".to_string(),
                    namespace: None,
                    description: "docs helper".to_string(),
                    source: "zeta".to_string(),
                    tags: vec![],
                    score: None,
                },
                IndexedSkill {
                    name: "alpha".to_string(),
                    namespace: None,
                    description: "docs helper".to_string(),
                    source: "alpha".to_string(),
                    tags: vec![],
                    score: None,
                },
            ],
            source: Vec::new(),
        };

        let results = index.search("docs");
        assert_eq!(results[0].0.name, "alpha");
        assert_eq!(results[1].0.name, "zeta");
    }

    #[test]
    fn qualified_name_round_trips() {
        let scoped = IndexedSkill {
            name: "pdf".to_string(),
            namespace: Some("anthropics".to_string()),
            description: "x".to_string(),
            source: "anthropics/pdf".to_string(),
            tags: vec![],
            score: None,
        };
        assert_eq!(scoped.qualified_name(), "anthropics/pdf");

        let unscoped = IndexedSkill {
            name: "legacy".to_string(),
            namespace: None,
            description: "x".to_string(),
            source: "legacy".to_string(),
            tags: vec![],
            score: None,
        };
        assert_eq!(unscoped.qualified_name(), "legacy");
    }

    #[test]
    fn validates_namespace_charset() {
        // Same kebab-case rules as skill name (URL-safe).
        let mut skill = IndexedSkill {
            name: "ok".to_string(),
            namespace: Some("good-ns".to_string()),
            description: "x".to_string(),
            source: "good-ns/ok".to_string(),
            tags: vec![],
            score: None,
        };
        assert!(skill.validate().is_ok());

        skill.namespace = Some("Bad_Namespace".to_string());
        let err = skill.validate().unwrap_err().to_string();
        assert!(err.contains("invalid namespace"), "got: {err}");
    }

    #[test]
    fn parses_v1_lockfile_without_version_or_namespace() {
        // Lockfiles written by knack 0.1.x had no `version` field and
        // no `namespace`. Reading one with v2-aware knack must yield
        // version=1, namespace=None — never silently promote to v2.
        let toml_v1 = r#"
[[skill]]
name = "pdf"
source = "public:pdf"
resolved = "http+knack:https://example.com/skills/pdf/archive#sha=abc123"
checksum = "sha256:deadbeef"
"#;
        let lockfile: Lockfile = toml::from_str(toml_v1).expect("v1 lockfile must parse");
        assert_eq!(lockfile.version, 1);
        assert_eq!(lockfile.skill.len(), 1);
        assert_eq!(lockfile.skill[0].namespace, None);
        assert!(lockfile.ensure_supported_version().is_ok());
    }

    #[test]
    fn parses_v2_lockfile_with_namespace() {
        let toml_v2 = r#"
version = 2

[[skill]]
name = "pdf"
namespace = "anthropics"
source = "public:anthropics/pdf"
resolved = "http+knack:https://example.com/skills/anthropics/pdf/archive#sha=abc"
checksum = "sha256:deadbeef"
"#;
        let lockfile: Lockfile = toml::from_str(toml_v2).expect("v2 lockfile must parse");
        assert_eq!(lockfile.version, 2);
        assert_eq!(lockfile.skill[0].namespace.as_deref(), Some("anthropics"));
    }

    #[test]
    fn rejects_lockfile_from_newer_knack() {
        let future = r#"
version = 999
[[skill]]
name = "pdf"
source = "public:pdf"
resolved = "x"
checksum = "x"
"#;
        let lockfile: Lockfile = toml::from_str(future).unwrap();
        let err = lockfile
            .ensure_supported_version()
            .expect_err("future lockfile must be rejected");
        assert!(err.contains("newer than this knack supports"), "got: {err}");
    }

    #[test]
    fn locked_skill_omits_namespace_when_absent() {
        let skill = LockedSkill {
            name: "pdf".to_string(),
            namespace: None,
            source: "public:pdf".to_string(),
            resolved: "x".to_string(),
            checksum: "y".to_string(),
        };
        let serialized = toml::to_string(&skill).unwrap();
        assert!(
            !serialized.contains("namespace"),
            "namespace should be omitted from legacy entries, got: {serialized}"
        );
    }

    #[test]
    fn parses_legacy_unnamespaced_index_json() {
        // index.json files produced by knack-registry 0.2.x don't
        // carry a `namespace` field. They MUST keep deserializing —
        // existing R2 buckets, lockfiles, and clients depend on that.
        let json = r#"{
            "name": "pdf",
            "description": "PDF docs",
            "source": "public:pdf",
            "tags": ["documents"]
        }"#;
        let parsed: IndexedSkill =
            serde_json::from_str(json).expect("legacy index.json must parse");
        assert_eq!(parsed.name, "pdf");
        assert_eq!(parsed.namespace, None);
        assert_eq!(parsed.qualified_name(), "pdf");
    }

    #[test]
    fn omits_namespace_field_when_absent_on_serialize() {
        // Symmetric to the legacy-parse test: writing out an unscoped
        // skill should not introduce a noisy `"namespace": null` into
        // index.json. skip_serializing_if = "Option::is_none" enforces
        // this round-trip cleanliness.
        let skill = IndexedSkill {
            name: "legacy".to_string(),
            namespace: None,
            description: "x".to_string(),
            source: "legacy".to_string(),
            tags: vec![],
            score: None,
        };
        let json = serde_json::to_string(&skill).unwrap();
        assert!(
            !json.contains("namespace"),
            "namespace should be omitted, got: {json}"
        );
    }
}
