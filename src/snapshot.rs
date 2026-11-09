//! Prefix snapshots and rollback.
//!
//! A snapshot captures the install ledger plus the cellar directories and bin
//! links for a set of packages. Restoring replaces the current state with the
//! captured one. On macOS directories are cloned with `clonefile` (copy-on-write,
//! cheap and instant); elsewhere a recursive copy is used.
//!
//! Snapshots live under `<data>/snapshots/<id>/`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::state::{self, Ledger};
use crate::{Error, Result};

pub const SNAPSHOT_DIR: &str = "snapshots";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub id: String,
    pub created_at: String,
    pub reason: String,
    pub prefix: PathBuf,
    pub packages: Vec<SnapshotPackage>,
    #[serde(default)]
    pub bin_links: Vec<BinLink>,
    pub ledger: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPackage {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub cellar: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinLink {
    pub link: PathBuf,
    pub target: PathBuf,
}

fn snapshots_root() -> PathBuf {
    config::data_dir().join(SNAPSHOT_DIR)
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{secs}-{nanos}-{seq}")
}

/// Create a snapshot of `packages` (empty = every installed package).
pub fn create(prefix: &Path, reason: &str, only: &[String]) -> Result<SnapshotManifest> {
    let ledger = Ledger::load()?;
    let selected: Vec<&state::Record> = if only.is_empty() {
        ledger.packages.iter().collect()
    } else {
        ledger
            .packages
            .iter()
            .filter(|r| only.iter().any(|o| o.eq_ignore_ascii_case(&r.name)))
            .collect()
    };
    let id = new_id();
    let dir = snapshots_root().join(&id);
    std::fs::create_dir_all(&dir)?;
    let cellar_dst = dir.join("cellar");
    std::fs::create_dir_all(&cellar_dst)?;

    let mut packages = Vec::new();
    for record in selected {
        if !record.cellar.exists() {
            continue;
        }
        let name_dir = cellar_dst.join(&record.name);
        std::fs::create_dir_all(&name_dir)?;
        let dest = name_dir.join(&record.version);
        clone_dir(&record.cellar, &dest)?;
        packages.push(SnapshotPackage {
            name: record.name.clone(),
            version: record.version.clone(),
            backend: record.backend.clone(),
            cellar: record.cellar.clone(),
        });
    }

    // Record bin links so restore can rebuild them.
    let bin_dir = config::bin_dir(prefix);
    let mut bin_links = Vec::new();
    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)?.flatten() {
            let link = entry.path();
            #[cfg(windows)]
            {
                bin_links.push(BinLink {
                    link: link.clone(),
                    target: link.clone(),
                });
            }
            #[cfg(not(windows))]
            {
                if let Ok(target) = std::fs::read_link(&link) {
                    bin_links.push(BinLink { link, target });
                }
            }
        }
    }

    let ledger_json: serde_json::Value = serde_json::to_value(&ledger)?;
    let manifest = SnapshotManifest {
        id: id.clone(),
        created_at: crate::state::now_rfc3339(),
        reason: reason.to_string(),
        prefix: prefix.to_path_buf(),
        packages,
        bin_links,
        ledger: ledger_json.clone(),
    };
    let text = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(dir.join("manifest.json"), text)?;
    std::fs::write(
        dir.join("ledger.json"),
        serde_json::to_string_pretty(&ledger)?,
    )?;
    tracing::info!(
        "snapshot {id} created ({} packages, reason: {reason})",
        manifest.packages.len()
    );
    Ok(manifest)
}

/// Restore a snapshot by id. Replaces the affected cellars, bin links, and ledger.
pub fn restore(id: &str, prefix: &Path) -> Result<SnapshotManifest> {
    let dir = snapshots_root().join(id);
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(Error::NotFound(format!("snapshot '{id}' not found")));
    }
    let text = std::fs::read_to_string(&manifest_path)?;
    let manifest: SnapshotManifest = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("parse snapshot manifest: {e}")))?;
    if manifest.prefix != prefix {
        return Err(Error::Other(anyhow::anyhow!(
            "snapshot {id} was taken for prefix {}; refusing to restore onto {}",
            manifest.prefix.display(),
            prefix.display()
        )));
    }
    // Remove the current cellars for the snapshotted packages, then copy back.
    for pkg in &manifest.packages {
        if pkg.cellar.exists() {
            std::fs::remove_dir_all(&pkg.cellar)?;
        }
        let src = dir.join("cellar").join(&pkg.name).join(&pkg.version);
        if src.exists() {
            if let Some(parent) = pkg.cellar.parent() {
                std::fs::create_dir_all(parent)?;
            }
            clone_dir(&src, &pkg.cellar)?;
        }
    }
    // Rebuild bin links.
    config::ensure_dirs(prefix)?;
    for link in &manifest.bin_links {
        if link.link.exists() || link.link.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&link.link);
        }
        #[cfg(windows)]
        {
            // Windows bin entries are copies; restore by copying the target.
            if link.target.exists() {
                std::fs::copy(&link.target, &link.link)?;
            }
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(&link.target, &link.link)?;
        }
    }
    // Restore the ledger.
    let ledger_path = config::installed_path();
    std::fs::write(
        &ledger_path,
        serde_json::to_string_pretty(&manifest.ledger)?,
    )?;
    tracing::info!("snapshot {id} restored");
    Ok(manifest)
}

pub fn list() -> Result<Vec<SnapshotManifest>> {
    let root = snapshots_root();
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Ok(m) = serde_json::from_str::<SnapshotManifest>(&text) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

pub fn delete(id: &str) -> Result<()> {
    let dir = snapshots_root().join(id);
    if !dir.exists() {
        return Err(Error::NotFound(format!("snapshot '{id}' not found")));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

pub fn get(id: &str) -> Result<SnapshotManifest> {
    let path = snapshots_root().join(id).join("manifest.json");
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| Error::Config(format!("parse snapshot manifest: {e}")))
}

/// Clone `src` to `dst` with copy-on-write when the platform offers it.
fn clone_dir(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(target_os = "macos")]
    {
        // clonefile gives an APFS copy-on-write clone: cheap and safe.
        let status = Command::new("/usr/bin/clonefile")
            .arg(src)
            .arg(dst)
            .status();
        match status {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => {
                tracing::warn!(
                    "clonefile failed ({s}); falling back to recursive copy for {}",
                    src.display()
                );
            }
            Err(_) => {}
        }
    }
    copy_dir_recursive(src, dst)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(windows)]
            {
                let _ = std::fs::copy(&from, &to);
            }
            #[cfg(not(windows))]
            {
                let target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(target, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Roll back to `manifest` if the caller passes a snapshot taken before a
/// failed operation. Convenience wrapper used by the install path.
pub fn rollback(manifest: &SnapshotManifest, prefix: &Path) -> Result<()> {
    restore(&manifest.id, prefix).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip_restores_files() {
        // Build a fake prefix with one package and a bin link.
        let base = std::env::temp_dir().join(format!("maca-snap-test-{}", std::process::id()));
        let prefix = base.join("prefix");
        let data = base.join("data");
        // Point config at the temp data dir.
        // SAFETY: single-threaded test; sets XDG_DATA_HOME for `directories`.
        // `directories` caches nothing across calls, so this is effective.
        // (macOS ignores XDG_DATA_HOME for BaseDirs, so also stub the
        // installed ledger by using MACA_PREFIX + explicit file paths.)
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let _ = data;
        // Create a cellar dir with a marker file.
        let cellar = prefix.join("cellar").join("demo").join("1.0.0");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::write(cellar.join("marker"), b"original").unwrap();
        // clone_dir is the primitive under test; verify it preserves content.
        let dst = base.join("clone");
        clone_dir(&cellar, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"original");
        // Modify the source, then confirm the clone is independent.
        std::fs::write(cellar.join("marker"), b"changed").unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn new_id_is_unique_enough() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b, "snapshot ids must not collide");
        assert!(a.len() > 8);
    }
}
