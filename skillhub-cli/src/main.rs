use std::{
    fs::{self, File},
    io::Cursor,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use flate2::{Compression, write::GzEncoder};
use serde::Deserialize;
use tar::{Builder, Header};
use tempfile::TempDir;

#[derive(Debug, Parser)]
#[command(name = "skillhub")]
#[command(version, about = "Package, share, and install Agent Skills")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
        #[arg(long, default_value = ".agents/skills")]
        target: PathBuf,
    },

    /// List installed skills.
    List {
        /// Directory containing installed skills.
        #[arg(long, default_value = ".agents/skills")]
        target: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New { name, dir } => {
            new_skill(&name, dir)?;
        }
        Command::Validate { path } => {
            let skill = read_skill(&path)?;
            validate_skill(&skill)?;
            println!("valid skill: {}", skill.name);
        }
        Command::Pack { path, output } => {
            let archive = pack_skill(&path, &output)?;
            println!("packed skill: {}", archive.display());
        }
        Command::Install { source, target } => {
            let installed = install_skill(&source, &target)?;
            println!("installed skill: {}", installed.display());
        }
        Command::List { target } => {
            list_skills(&target)?;
        }
    }

    Ok(())
}

fn install_skill(source: &str, target: &PathBuf) -> Result<PathBuf> {
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;

    if let Some(spec) = source.strip_prefix("gh:") {
        let fetched = fetch_github_skill(spec)?;
        return install_skill_dir(&fetched.path, target);
    }

    let source = PathBuf::from(source);

    install_local_skill(&source, target)
}

fn install_local_skill(source: &PathBuf, target: &PathBuf) -> Result<PathBuf> {
    if source.is_dir() {
        return install_skill_dir(source, target);
    }

    if source.is_file() {
        let file =
            File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let unpacked_root = archive_root(source)?;
        let destination = target.join(&unpacked_root);
        if destination.exists() {
            bail!("skill already installed: {}", destination.display());
        }
        archive
            .unpack(target)
            .with_context(|| format!("failed to unpack {}", source.display()))?;
        let skill = read_skill(&destination)?;
        validate_skill(&skill)?;
        return Ok(destination);
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
}

fn fetch_github_skill(spec: &str) -> Result<FetchedSkill> {
    let spec = parse_github_spec(spec)?;
    let archive_url = format!(
        "https://github.com/{}/{}/archive/{}.tar.gz",
        spec.owner, spec.repo, spec.reference
    );

    let response = reqwest::blocking::Client::new()
        .get(&archive_url)
        .header(reqwest::header::USER_AGENT, "skillhub")
        .send()
        .with_context(|| format!("failed to fetch {archive_url}"))?
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
    })
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
        .map(|(repo, reference)| (repo, reference))
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

fn install_skill_dir(source: &Path, target: &Path) -> Result<PathBuf> {
    let skill = read_skill(source)?;
    validate_skill(&skill)?;
    let destination = target.join(&skill.name);
    if destination.exists() {
        bail!("skill already installed: {}", destination.display());
    }
    copy_dir(source, &destination)?;
    Ok(destination)
}

fn list_skills(target: &PathBuf) -> Result<()> {
    if !target.exists() {
        return Ok(());
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
    for skill in skills {
        println!("{skill}");
    }

    Ok(())
}

fn archive_root(source: &Path) -> Result<String> {
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("archive path has no valid file name"))?;
    let root = filename
        .strip_suffix(".skill.tar.gz")
        .ok_or_else(|| anyhow!("archive must end with .skill.tar.gz"))?;
    validate_skill_name(root)?;
    Ok(root.to_string())
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

fn collect_files(path: &Path) -> Result<Vec<PathBuf>> {
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

#[derive(Debug)]
struct Skill {
    path: PathBuf,
    name: String,
    description: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

    println!("created skill: {}", skill_dir.display());
    Ok(())
}

fn read_skill(path: &Path) -> Result<Skill> {
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

fn validate_skill(skill: &Skill) -> Result<()> {
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

fn validate_skill_name(name: &str) -> Result<()> {
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
