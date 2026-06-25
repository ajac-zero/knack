use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Lockfile {
    #[serde(default)]
    pub skill: Vec<LockedSkill>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LockedSkill {
    pub name: String,
    pub source: String,
    pub resolved: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryConfig {
    pub kind: RegistryKind,
    pub url: String,
    pub default_ref: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryKind {
    GitHost,
}
