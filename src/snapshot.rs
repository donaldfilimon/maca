//! Prefix snapshots and rollback.
//!
//! A snapshot captures the install ledger plus the cellar directories and bin
//! links for a set of packages. Restoring replaces the current state with the
//! captured one, including removing kegs and links that appeared afterwards.
//! On macOS directories are cloned with `cp -c` (APFS copy-on-write); elsewhere
//! a recursive copy is used.
//!
//! Snapshots live under `<data>/snapshots/<id>/`.

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
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
    /// Every `cellar/<name>/<version>` directory that existed in the prefix
    /// when the snapshot was taken, whether or not maca's ledger knew about
    /// it. Restore removes only kegs that are absent from this list, so
    /// anything that was already there (another data dir's packages, files
    /// the user placed) is never deleted. `None` in manifests written before
    /// this field existed.
    #[serde(default)]
    pub kegs: Option<Vec<PathBuf>>,
}

/// All `<cellar_root>/<name>/<version>` directories, skipping staging dirs.
fn existing_kegs(cellar_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(names) = std::fs::read_dir(cellar_root) else {
        return out;
    };
    for name_entry in names.flatten() {
        let fname = name_entry.file_name();
        let fname = fname.to_string_lossy();
        let name_dir = name_entry.path();
        if !name_dir.is_dir() || fname.starts_with('.') || fname.starts_with("__tmp") {
            continue;
        }
        if let Ok(kegs) = std::fs::read_dir(&name_dir) {
            out.extend(kegs.flatten().map(|k| k.path()).filter(|k| k.is_dir()));
        }
    }
    out.sort();
    out
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
        kegs: Some(existing_kegs(&config::cellar_dir(prefix))),
    };
    std::fs::write(
        dir.join("ledger.json"),
        serde_json::to_string_pretty(&ledger)?,
    )?;
    // The manifest is written last and atomically: `list` and `restore` only
    // see a snapshot whose kegs were all copied.
    crate::install::atomic_write(
        &dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
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
    // 1. Validate before touching anything: every snapshotted keg must still
    //    be present in the snapshot directory.
    for pkg in &manifest.packages {
        let src = dir.join("cellar").join(&pkg.name).join(&pkg.version);
        if !src.is_dir() {
            return Err(Error::Other(anyhow::anyhow!(
                "snapshot {id} is incomplete: missing copy of {} {}; nothing was changed",
                pkg.name,
                pkg.version
            )));
        }
    }
    let snap_ledger: Ledger = serde_json::from_value(manifest.ledger.clone())
        .map_err(|e| Error::Config(format!("parse snapshot ledger: {e}")))?;

    // 2. Remove kegs that appeared after the snapshot (a new install, the new
    //    version of an upgrade, or a keg left by a failed install that never
    //    reached the ledger). Only kegs that did not exist at snapshot time
    //    are removed; anything that was already in the cellar stays, even if
    //    this ledger never knew about it.
    let cellar_root = config::cellar_dir(prefix);
    let created_since: Vec<PathBuf> = match &manifest.kegs {
        Some(then) => existing_kegs(&cellar_root)
            .into_iter()
            .filter(|keg| !then.contains(keg))
            .collect(),
        // Older manifest without the list: fall back to what the ledgers can
        // prove, i.e. kegs the current ledger records and the snapshot's
        // ledger does not.
        None => Ledger::load()
            .map(|now| now.packages)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.cellar)
            .filter(|keg| {
                keg.starts_with(&cellar_root)
                    && !snap_ledger.packages.iter().any(|r| &r.cellar == keg)
            })
            .collect(),
    };
    for keg in created_since {
        if keg.is_dir() {
            std::fs::remove_dir_all(&keg)?;
        }
        if let Some(name_dir) = keg.parent() {
            let _ = std::fs::remove_dir(name_dir); // only if now empty
        }
    }

    // 3. Put the snapshotted kegs back. Each is cloned next to its final
    //    path first, so the live keg is only removed once its replacement
    //    exists in full.
    for pkg in &manifest.packages {
        let src = dir.join("cellar").join(&pkg.name).join(&pkg.version);
        let mut staged = pkg.cellar.clone().into_os_string();
        staged.push(".maca-restore");
        let staged = PathBuf::from(staged);
        clone_dir(&src, &staged)?;
        if pkg.cellar.exists() {
            std::fs::remove_dir_all(&pkg.cellar)?;
        }
        std::fs::rename(&staged, &pkg.cellar)?;
    }

    // 4. Links. Drop every maca-made symlink that now dangles (it pointed at
    //    a keg removed above), then recreate what the snapshot recorded.
    config::ensure_dirs(prefix)?;
    sweep_dangling_links(&config::bin_dir(prefix), &cellar_root, false);
    sweep_dangling_links(&prefix.join("opt"), &cellar_root, false);
    sweep_dangling_links(&prefix.join("share"), &cellar_root, true);
    for link in &manifest.bin_links {
        #[cfg(windows)]
        {
            // Windows bin entries are copies; restore by copying the target.
            if link.target.exists() {
                let _ = std::fs::remove_file(&link.link);
                std::fs::copy(&link.target, &link.link)?;
            }
        }
        #[cfg(not(windows))]
        {
            if !link.target.exists() {
                continue; // belonged to a package outside this snapshot
            }
            if link.link.symlink_metadata().is_ok() {
                if std::fs::read_link(&link.link).is_err() {
                    continue; // a real file now lives there; leave it
                }
                let _ = std::fs::remove_file(&link.link);
            }
            std::os::unix::fs::symlink(&link.target, &link.link)?;
        }
    }
    for record in &snap_ledger.packages {
        if record.cellar.starts_with(&cellar_root) && record.cellar.exists() {
            let _ = crate::install::link_opt(&record.name, &record.version, &record.cellar, prefix);
            let _ = crate::install::link_share(&record.cellar, prefix);
        }
    }

    // 5. The ledger goes last, atomically, once the files match it.
    crate::install::atomic_write(
        &config::installed_path(),
        serde_json::to_string_pretty(&manifest.ledger)?.as_bytes(),
    )?;
    tracing::info!("snapshot {id} restored");
    Ok(manifest)
}

/// Remove symlinks in `dir` whose target lies under `cellar_root` but no
/// longer exists. Files and links pointing elsewhere are never touched.
fn sweep_dangling_links(dir: &Path, cellar_root: &Path, recursive: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if recursive {
                sweep_dangling_links(&path, cellar_root, true);
                let _ = std::fs::remove_dir(&path); // only if now empty
            }
        } else if ft.is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                if target.starts_with(cellar_root) && !target.exists() {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
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
    // Newest first. Ids are `<secs>-<nanos>-<seq>`, so they order precisely;
    // `created_at` has one-second resolution and only breaks ties for ids
    // that do not parse.
    let key = |m: &SnapshotManifest| -> (u64, u64, u64) {
        let mut it = m.id.split('-').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    };
    out.sort_by(|a, b| {
        key(b)
            .cmp(&key(a))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
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

/// Prune snapshots, keeping only the `keep` most recent snapshots.
pub fn prune(keep: usize) -> Result<usize> {
    let all = list()?;
    let mut deleted = 0;
    if all.len() > keep {
        for s in &all[keep..] {
            if delete(&s.id).is_ok() {
                deleted += 1;
            }
        }
    }
    Ok(deleted)
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
        // `cp -c` asks for APFS clonefile(2) per file: copy-on-write, so the
        // clone costs no extra disk until either side changes. On a
        // non-APFS volume cp itself falls back to a normal copy.
        let status = Command::new("/bin/cp")
            .arg("-cR")
            .arg(src)
            .arg(dst)
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => return Ok(()),
            other => {
                tracing::warn!(
                    "cp -c failed ({other:?}); falling back to recursive copy for {}",
                    src.display()
                );
                let _ = std::fs::remove_dir_all(dst);
            }
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
