use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
            install: InstallConfig { target },
            skills: BTreeMap::new(),
            registries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallConfig {
    pub target: PathBuf,
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

    pub fn search(&self, query: &str) -> Vec<&IndexedSkill> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }

        self.skill
            .iter()
            .filter(|skill| {
                let haystack = skill.search_text();
                terms.iter().all(|term| haystack.contains(term))
            })
            .collect()
    }
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

    fn search_text(&self) -> String {
        let ns = self.namespace.as_deref().unwrap_or("");
        format!(
            "{} {} {} {}",
            ns,
            self.name,
            self.description,
            self.tags.join(" ")
        )
        .to_ascii_lowercase()
    }
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
                },
                IndexedSkill {
                    name: "rust-code-review".to_string(),
                    namespace: None,
                    description: "Review Rust code".to_string(),
                    source: "rust-code-review".to_string(),
                    tags: vec!["rust".to_string()],
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
    fn qualified_name_round_trips() {
        let scoped = IndexedSkill {
            name: "pdf".to_string(),
            namespace: Some("anthropics".to_string()),
            description: "x".to_string(),
            source: "anthropics/pdf".to_string(),
            tags: vec![],
        };
        assert_eq!(scoped.qualified_name(), "anthropics/pdf");

        let unscoped = IndexedSkill {
            name: "legacy".to_string(),
            namespace: None,
            description: "x".to_string(),
            source: "legacy".to_string(),
            tags: vec![],
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
        };
        let json = serde_json::to_string(&skill).unwrap();
        assert!(
            !json.contains("namespace"),
            "namespace should be omitted, got: {json}"
        );
    }
}
