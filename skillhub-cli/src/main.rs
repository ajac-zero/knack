use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;

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
        /// Path to a skill directory or package archive.
        source: PathBuf,

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
            println!("pack skill: {} into {}", path.display(), output.display());
        }
        Command::Install { source, target } => {
            println!(
                "install skill: {} into {}",
                source.display(),
                target.display()
            );
        }
        Command::List { target } => {
            println!("list skills in {}", target.display());
        }
    }

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

fn read_skill(path: &PathBuf) -> Result<Skill> {
    if !path.is_dir() {
        bail!("skill path is not a directory: {}", path.display());
    }

    let skill_file = path.join("SKILL.md");
    let contents = fs::read_to_string(&skill_file)
        .with_context(|| format!("failed to read {}", skill_file.display()))?;
    let frontmatter = parse_frontmatter(&contents)
        .with_context(|| format!("failed to parse {}", skill_file.display()))?;

    Ok(Skill {
        path: path.clone(),
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
