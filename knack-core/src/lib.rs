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

pub fn validate_skill(skill: &Skill) -> Result<()> {
    validate_skill_name(&skill.name)?;

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

    if skill.description.trim().is_empty() {
        bail!("description must not be empty");
    }

    if skill.description.chars().count() > 1024 {
        bail!("description must be at most 1024 characters");
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
pub const LOCKFILE_VERSION: u32 = 1;

fn default_lockfile_version() -> u32 {
    LOCKFILE_VERSION
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
    pub description: String,
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexSource {
    pub source: String,
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
        if self.description.trim().is_empty() {
            bail!("indexed skill description must not be empty: {}", self.name);
        }
        if self.source.trim().is_empty() {
            bail!("indexed skill source must not be empty: {}", self.name);
        }
        Ok(())
    }

    fn search_text(&self) -> String {
        format!("{} {} {}", self.name, self.description, self.tags.join(" ")).to_ascii_lowercase()
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
    fn searches_registry_index() {
        let index = RegistryIndex {
            skill: vec![
                IndexedSkill {
                    name: "pdf".to_string(),
                    description: "Work with PDF documents".to_string(),
                    source: "gh:anthropics/skills/skills/pdf".to_string(),
                    tags: vec!["documents".to_string(), "ocr".to_string()],
                },
                IndexedSkill {
                    name: "rust-code-review".to_string(),
                    description: "Review Rust code".to_string(),
                    source: "tea:platform/skills/rust-code-review".to_string(),
                    tags: vec!["rust".to_string()],
                },
            ],
            source: Vec::new(),
        };

        assert_eq!(index.search("pdf").len(), 1);
        assert_eq!(index.search("documents ocr").len(), 1);
        assert_eq!(index.search("python").len(), 0);
    }
}
