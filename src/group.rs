//! Groups (profiles), package kinds, and lockfiles.
//!
//! A **group** is a named set of packages with an optional pinned version and a
//! kind (`app`, `plugin`, `cli`, `library`, `other`). Groups live at
//! `<data>/groups/<name>.json` and can be installed as a unit or exported as a
//! lockfile.
//!
//! A **lockfile** (`<prefix>/maca.lock.json`) lists packages with a version,
//! backend and kind. `maca lock` records the exact installed versions;
//! `maca group export` writes `latest` for group entries without a version.
//! `maca sync` installs the set and fails if a package resolves to a version
//! other than the one locked (`latest` accepts any).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    App,
    Plugin,
    Cli,
    Library,
    Other,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Plugin => "plugin",
            Self::Cli => "cli",
            Self::Library => "library",
            Self::Other => "other",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "app" => Self::App,
            "plugin" => Self::Plugin,
            "cli" => Self::Cli,
            "library" => Self::Library,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_kind() -> Kind {
    Kind::App
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub packages: Vec<GroupEntry>,
}

impl Group {
    pub fn path(name: &str) -> PathBuf {
        crate::config::data_dir()
            .join("groups")
            .join(format!("{name}.json"))
    }

    pub fn load(name: &str) -> Result<Self> {
        let path = Self::path(name);
        if !path.exists() {
            return Err(Error::NotFound(format!("group '{name}' not found")));
        }
        let text = std::fs::read_to_string(&path)?;
        serde_json::from_str(&text).map_err(|e| Error::Config(format!("parse group: {e}")))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path(&self.name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        crate::install::atomic_write(&path, text.as_bytes())
    }

    pub fn upsert(&mut self, entry: GroupEntry) {
        if let Some(existing) = self.packages.iter_mut().find(|p| p.name == entry.name) {
            *existing = entry;
        } else {
            self.packages.push(entry);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() != before
    }

    pub fn list() -> Result<Vec<Group>> {
        let dir = crate::config::data_dir().join("groups");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(g) = serde_json::from_str::<Group>(&text)
            {
                out.push(g);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn delete(name: &str) -> Result<()> {
        let path = Self::path(name);
        if !path.exists() {
            return Err(Error::NotFound(format!("group '{name}' not found")));
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    pub created_at: String,
    pub packages: Vec<LockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    pub backend: String,
    /// Package kind to record on install. Absent in older lockfiles.
    #[serde(default = "default_kind")]
    pub kind: Kind,
}

pub fn lock_path(prefix: &std::path::Path) -> PathBuf {
    prefix.join("maca.lock.json")
}

/// Write a lockfile from the current ledger.
pub fn write_lock(prefix: &std::path::Path) -> Result<Lockfile> {
    let ledger = crate::state::Ledger::load()?;
    // Only this prefix's packages: the ledger is shared by every prefix.
    let cellar_root = crate::config::cellar_dir(prefix);
    let packages = ledger
        .packages
        .iter()
        .filter(|r| crate::config::path_under(&r.cellar, &cellar_root))
        .map(|r| LockEntry {
            name: r.name.clone(),
            version: r.version.clone(),
            backend: r.backend.clone(),
            kind: r.kind,
        })
        .collect();
    let lock = Lockfile {
        version: 1,
        created_at: crate::state::now_rfc3339(),
        packages,
    };
    crate::config::ensure_dirs(prefix)?;
    crate::install::atomic_write(
        &lock_path(prefix),
        serde_json::to_string_pretty(&lock)?.as_bytes(),
    )?;
    Ok(lock)
}

pub fn read_lock(prefix: &std::path::Path) -> Result<Lockfile> {
    let path = lock_path(prefix);
    if !path.exists() {
        return Err(Error::NotFound(format!(
            "no lockfile at {}",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| Error::Config(format!("parse lockfile: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        assert_eq!(Kind::from_str_lossy("cli"), Kind::Cli);
        assert_eq!(Kind::from_str_lossy("APP"), Kind::App);
        assert_eq!(Kind::from_str_lossy("nope"), Kind::Other);
        assert_eq!(Kind::Cli.as_str(), "cli");
    }

    #[test]
    fn group_upsert_and_remove() {
        let mut g = Group {
            name: "dev".into(),
            description: String::new(),
            packages: vec![],
        };
        g.upsert(GroupEntry {
            name: "jq".into(),
            backend: Some("brew".into()),
            version: Some("1.8.2".into()),
            kind: Kind::Cli,
            note: None,
        });
        g.upsert(GroupEntry {
            name: "jq".into(),
            backend: Some("brew".into()),
            version: Some("1.9.0".into()),
            kind: Kind::Cli,
            note: None,
        });
        assert_eq!(g.packages.len(), 1);
        assert_eq!(g.packages[0].version.as_deref(), Some("1.9.0"));
        assert!(g.remove("jq"));
        assert!(!g.remove("jq"));
    }

    #[test]
    fn lockfile_roundtrip_shape() {
        let lock = Lockfile {
            version: 1,
            created_at: "2026-11-09T00:00:00Z".into(),
            packages: vec![LockEntry {
                name: "jq".into(),
                version: "1.8.2".into(),
                backend: "brew".into(),
                kind: Kind::Cli,
            }],
        };
        let text = serde_json::to_string(&lock).unwrap();
        let back: Lockfile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.packages[0].name, "jq");
    }
}
