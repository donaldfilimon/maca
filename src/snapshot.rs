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
/// Suffix of a keg being restored, staged next to its final path.
const RESTORE_SUFFIX: &str = ".maca-restore";

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
    /// Lower-cased package names when the snapshot was taken with `--only`.
    /// Restore then touches only those packages and leaves every other keg,
    /// link and ledger record as it is now. `None` means every package in
    /// the prefix.
    #[serde(default)]
    pub selected: Option<Vec<String>>,
}

/// What a snapshot covers: kegs inside its prefix's cellar and, for an
/// `--only` snapshot, only the named packages. Everything outside the scope
/// (other prefixes sharing this data dir, unselected packages) is never
/// copied, deleted or rewritten.
struct Scope<'a> {
    cellar_root: &'a Path,
    selected: Option<&'a [String]>,
}

impl Scope<'_> {
    fn has_name(&self, name: &str) -> bool {
        self.selected
            .is_none_or(|s| s.iter().any(|n| n.eq_ignore_ascii_case(name)))
    }

    /// A ledger record is in scope if its keg is in this cellar.
    fn has_record(&self, r: &state::Record) -> bool {
        config::path_under(&r.cellar, self.cellar_root) && self.has_name(&r.name)
    }

    /// A path inside `cellar/<name>/...` is in scope if `<name>` is.
    fn has_path(&self, path: &Path) -> bool {
        path.strip_prefix(self.cellar_root)
            .ok()
            .and_then(|rel| rel.components().next())
            .and_then(|c| c.as_os_str().to_str())
            .is_some_and(|name| self.has_name(name))
    }
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
            out.extend(
                kegs.flatten()
                    .map(|k| k.path())
                    .filter(|k| k.is_dir() && !k.to_string_lossy().ends_with(RESTORE_SUFFIX)),
            );
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

/// Create a snapshot of the packages installed in `prefix` (`only` empty =
/// all of them). Packages recorded in the shared ledger for other prefixes
/// are not part of it.
pub fn create(prefix: &Path, reason: &str, only: &[String]) -> Result<SnapshotManifest> {
    let ledger = Ledger::load()?;
    let cellar_root = config::cellar_dir(prefix);
    let selected_names: Option<Vec<String>> =
        (!only.is_empty()).then(|| only.iter().map(|n| n.to_ascii_lowercase()).collect());
    let scope = Scope {
        cellar_root: &cellar_root,
        selected: selected_names.as_deref(),
    };
    let id = new_id();
    let dir = snapshots_root().join(&id);
    std::fs::create_dir_all(&dir)?;
    // A snapshot that fails part-way must not linger as an unlisted copy.
    match create_in(
        &dir,
        &id,
        prefix,
        reason,
        &ledger,
        &scope,
        selected_names.clone(),
    ) {
        Ok(m) => Ok(m),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn create_in(
    dir: &Path,
    id: &str,
    prefix: &Path,
    reason: &str,
    ledger: &Ledger,
    scope: &Scope,
    selected: Option<Vec<String>>,
) -> Result<SnapshotManifest> {
    let cellar_dst = dir.join("cellar");
    std::fs::create_dir_all(&cellar_dst)?;
    let mut packages = Vec::new();
    for record in ledger.packages.iter().filter(|r| scope.has_record(r)) {
        if !record.cellar.exists() {
            continue;
        }
        let dest = cellar_dst.join(&record.name).join(&record.version);
        clone_dir(&record.cellar, &dest)?;
        packages.push(SnapshotPackage {
            name: record.name.clone(),
            version: record.version.clone(),
            backend: record.backend.clone(),
            cellar: record.cellar.clone(),
        });
    }

    // Record maca's own bin links (symlinks into this cellar) so restore can
    // rebuild them. Windows entries are copies and are re-created from the
    // restored kegs instead.
    #[cfg_attr(windows, allow(unused_mut))]
    let mut bin_links = Vec::new();
    #[cfg(not(windows))]
    if let Ok(entries) = std::fs::read_dir(config::bin_dir(prefix)) {
        for entry in entries.flatten() {
            let link = entry.path();
            if let Ok(target) = std::fs::read_link(&link)
                && scope.has_path(&target)
            {
                bin_links.push(BinLink { link, target });
            }
        }
    }

    let manifest = SnapshotManifest {
        id: id.to_string(),
        created_at: crate::state::now_rfc3339(),
        reason: reason.to_string(),
        prefix: prefix.to_path_buf(),
        packages,
        bin_links,
        ledger: serde_json::to_value(ledger)?,
        kegs: Some(existing_kegs(scope.cellar_root)),
        selected,
    };
    std::fs::write(
        dir.join("ledger.json"),
        serde_json::to_string_pretty(ledger)?,
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

/// Restore a snapshot by id: within the snapshot's scope (its prefix, and
/// its `--only` packages if any), put back the snapshotted kegs, remove kegs
/// created since, rebuild maca's links and the matching ledger records.
/// Nothing outside the scope is touched.
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
    let snap_ledger: Ledger = serde_json::from_value(manifest.ledger.clone())
        .map_err(|e| Error::Config(format!("parse snapshot ledger: {e}")))?;
    let cellar_root = config::cellar_dir(prefix);
    let scope = Scope {
        cellar_root: &cellar_root,
        selected: manifest.selected.as_deref(),
    };
    // Manifests written before snapshots were prefix-scoped may list kegs
    // of other prefixes; those are skipped, never restored over.
    let packages: Vec<&SnapshotPackage> = manifest
        .packages
        .iter()
        .filter(|p| config::path_under(&p.cellar, &cellar_root) && scope.has_name(&p.name))
        .collect();

    // 1. Stage a full copy of every snapshotted keg next to its final path
    //    before anything live is removed. If any copy fails, the staged
    //    copies are dropped and nothing has changed.
    let mut staged = Vec::new();
    for pkg in &packages {
        let src = dir.join("cellar").join(&pkg.name).join(&pkg.version);
        if !src.is_dir() {
            discard(&staged);
            return Err(Error::Other(anyhow::anyhow!(
                "snapshot {id} is incomplete: missing copy of {} {}; nothing was changed",
                pkg.name,
                pkg.version
            )));
        }
        let mut path = pkg.cellar.clone().into_os_string();
        path.push(RESTORE_SUFFIX);
        let path = PathBuf::from(path);
        if let Err(e) = clone_dir(&src, &path) {
            let _ = std::fs::remove_dir_all(&path);
            discard(&staged);
            return Err(e);
        }
        staged.push((path, pkg.cellar.clone()));
    }

    // 2. Remove in-scope kegs that appeared after the snapshot (a new
    //    install, the new version of an upgrade, a keg a failed install left
    //    unrecorded). Kegs that already existed at snapshot time stay, even
    //    if this ledger never knew about them.
    let created_since: Vec<PathBuf> = match &manifest.kegs {
        Some(then) => existing_kegs(&cellar_root)
            .into_iter()
            .filter(|keg| !then.contains(keg) && scope.has_path(keg))
            .collect(),
        // Older manifest without the list: remove only what the ledgers
        // prove is new, i.e. kegs the current ledger records and the
        // snapshot's ledger does not.
        None => Ledger::load()
            .map(|now| now.packages)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| scope.has_record(r))
            .map(|r| r.cellar)
            .filter(|keg| !snap_ledger.packages.iter().any(|r| &r.cellar == keg))
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

    // 3. Swap the staged copies in.
    for (from, to) in &staged {
        if to.exists() {
            std::fs::remove_dir_all(to)?;
        }
        std::fs::rename(from, to)?;
    }

    // 4. Links: drop maca symlinks that now dangle into the cellar, then put
    //    back the ones the snapshot recorded. An entry that is not a maca
    //    link into this cellar (a user's file or link) is left alone.
    config::ensure_dirs(prefix)?;
    sweep_dangling_links(&config::bin_dir(prefix), &cellar_root, false);
    sweep_dangling_links(&prefix.join("opt"), &cellar_root, false);
    sweep_dangling_links(&prefix.join("share"), &cellar_root, true);
    #[cfg(not(windows))]
    for link in &manifest.bin_links {
        if !scope.has_path(&link.target) || !link.target.exists() {
            continue;
        }
        if link.link.symlink_metadata().is_ok() {
            match std::fs::read_link(&link.link) {
                Ok(current) if config::path_under(&current, &cellar_root) => {
                    let _ = std::fs::remove_file(&link.link);
                }
                _ => continue, // not maca's: leave it
            }
        }
        std::os::unix::fs::symlink(&link.target, &link.link)?;
    }
    let restored: Vec<&state::Record> = snap_ledger
        .packages
        .iter()
        .filter(|r| scope.has_record(r) && r.cellar.exists())
        .collect();
    for record in &restored {
        let _ = crate::install::link_opt(&record.name, &record.version, &record.cellar, prefix);
        let _ = crate::install::link_share(&record.cellar, prefix);
        #[cfg(windows)]
        {
            let _ = crate::install::link_bins(&record.cellar, &[], prefix);
        }
    }

    // 5. Ledger last: keep every current record outside the scope, take the
    //    snapshot's records inside it. Done under the ledger lock because
    //    other prefixes may be writing their own records right now.
    state::with_ledger_lock(|| {
        let mut ledger = Ledger::load()?;
        ledger.packages.retain(|r| !scope.has_record(r));
        for record in &restored {
            ledger.upsert((*record).clone());
        }
        ledger.save()
    })?;
    tracing::info!("snapshot {id} restored");
    Ok(manifest)
}

/// Remove staged restore copies after a failed restore.
fn discard(staged: &[(PathBuf, PathBuf)]) {
    for (path, _) in staged {
        let _ = std::fs::remove_dir_all(path);
    }
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
        } else if ft.is_symlink()
            && let Ok(target) = std::fs::read_link(&path)
            && target.starts_with(cellar_root)
            && !target.exists()
        {
            let _ = std::fs::remove_file(&path);
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
        if let Ok(text) = std::fs::read_to_string(&manifest_path)
            && let Ok(m) = serde_json::from_str::<SnapshotManifest>(&text)
        {
            out.push(m);
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

/// Prune snapshots, keeping only the `keep` most recent snapshots. Also
/// removes directories left by a snapshot that was interrupted before its
/// manifest was written (older than an hour, so a snapshot being created
/// right now is never touched).
pub fn prune(keep: usize) -> Result<usize> {
    let mut deleted = sweep_incomplete(std::time::Duration::from_secs(3600));
    let all = list()?;
    if all.len() > keep {
        for s in &all[keep..] {
            if delete(&s.id).is_ok() {
                deleted += 1;
            }
        }
    }
    Ok(deleted)
}

fn sweep_incomplete(min_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(snapshots_root()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if path.is_dir()
            && !path.join("manifest.json").exists()
            && old_enough
            && std::fs::remove_dir_all(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
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
    fn clone_dir_makes_an_independent_copy() {
        let base = std::env::temp_dir().join(format!("maca-clone-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src/demo/1.0.0");
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::write(src.join("marker"), b"original").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("marker", src.join("lib/alias")).unwrap();
        let dst = base.join("clone/demo/1.0.0"); // parent does not exist yet
        clone_dir(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"original");
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(dst.join("lib/alias")).unwrap(),
            PathBuf::from("marker")
        );
        // Changing the source does not change the clone.
        std::fs::write(src.join("marker"), b"changed").unwrap();
        assert_eq!(std::fs::read(dst.join("marker")).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scope_limits_by_prefix_and_selection() {
        let root = Path::new("/p/cellar");
        let all = Scope {
            cellar_root: root,
            selected: None,
        };
        assert!(all.has_path(Path::new("/p/cellar/jq/1.0")));
        assert!(!all.has_path(Path::new("/q/cellar/jq/1.0")));
        let picked = ["jq".to_string()];
        let only = Scope {
            cellar_root: root,
            selected: Some(&picked),
        };
        assert!(only.has_path(Path::new("/p/cellar/jq/1.0/bin/jq")));
        assert!(!only.has_path(Path::new("/p/cellar/tree/2.0")));
        assert!(only.has_name("JQ"));
    }

    #[test]
    fn new_id_is_unique_enough() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b, "snapshot ids must not collide");
        assert!(a.len() > 8);
    }
}
