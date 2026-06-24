use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
            println!("new skill: {name} in {}", dir.display());
        }
        Command::Validate { path } => {
            println!("validate skill: {}", path.display());
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
