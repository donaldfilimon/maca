//! Local reasoning over the ledger, pins, groups, and snapshots.
//!
//! These functions do not talk to a catalog. Callers pass the records they
//! already loaded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::backend::Package;
use crate::group::Group;
use crate::snapshot::SnapshotManifest;
use crate::state::Record;

/// Higher score first. Exact name, then prefix, then a name substring, then
/// a description substring. Ties break by backend, then name.
pub fn rank_packages(query: &str, mut packages: Vec<Package>) -> Vec<Package> {
    let q = query.trim().to_ascii_lowercase();
    packages.sort_by(|a, b| {
        rank_score(&q, b)
            .cmp(&rank_score(&q, a))
            .then_with(|| a.backend.cmp(&b.backend))
            .then_with(|| a.name.cmp(&b.name))
    });
    packages
}

fn rank_score(query: &str, package: &Package) -> u32 {
    if query.is_empty() {
        return 0;
    }
    let name = package.name.to_ascii_lowercase();
    let description = package.description.to_ascii_lowercase();
    if name == query {
        400
    } else if name.starts_with(query) {
        300
    } else if name.contains(query) {
        200
    } else if description.contains(query) {
        100
    } else {
        0
    }
}

/// Where a spec should go before the normal auto order.
///
/// `aqua:…` is the aqua registry. `owner/repo` is a GitHub release.
/// A single token, `owner--repo`, and anything with whitespace is `None`
/// (the caller already has an explicit backend, or should keep auto).
pub fn suggest_backend(spec: &str) -> Option<&'static str> {
    let spec = spec.trim();
    if spec.is_empty() || spec.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(rest) = spec.strip_prefix("aqua:") {
        if rest.is_empty() || rest.chars().any(char::is_whitespace) {
            return None;
        }
        return Some("aqua");
    }
    if spec.starts_with("http://") || spec.starts_with("https://") || spec.contains("--") {
        return None;
    }
    match spec.split_once('/') {
        Some((owner, repo))
            if !owner.is_empty()
                && !repo.is_empty()
                && !owner.contains('/')
                && !repo.contains('/')
                && !owner.contains("--")
                && !repo.contains("--") =>
        {
            Some("github")
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionChange {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDelta {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<VersionChange>,
}

/// Name → version. A backend-only difference is not a change.
/// Duplicate names in one manifest: the last one wins.
pub fn diff_snapshots(older: &SnapshotManifest, newer: &SnapshotManifest) -> SnapshotDelta {
    let old = last_versions(&older.packages);
    let new = last_versions(&newer.packages);
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (name, version) in &new {
        match old.get(name) {
            None => added.push(name.clone()),
            Some(prev) if prev != version => changed.push(VersionChange {
                name: name.clone(),
                from: prev.clone(),
                to: version.clone(),
            }),
            _ => {}
        }
    }
    for name in old.keys() {
        if !new.contains_key(name) {
            removed.push(name.clone());
        }
    }
    added.sort();
    removed.sort();
    changed.sort_by(|a, b| a.name.cmp(&b.name));
    SnapshotDelta {
        added,
        removed,
        changed,
    }
}

fn last_versions(packages: &[crate::snapshot::SnapshotPackage]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for package in packages {
        out.insert(package.name.clone(), package.version.clone());
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDrift {
    pub name: String,
    pub pinned: String,
    /// None when the pin names a package that is not installed.
    pub installed: Option<String>,
}

/// A pin drifts when the package is absent or installed at another version.
pub fn pin_drift(records: &[Record], pins: &BTreeMap<String, String>) -> Vec<PinDrift> {
    let mut out = Vec::new();
    for (name, pinned) in pins {
        let installed = records
            .iter()
            .find(|r| r.name == *name)
            .map(|r| r.version.clone());
        match &installed {
            Some(version) if version == pinned => {}
            _ => out.push(PinDrift {
                name: name.clone(),
                pinned: pinned.clone(),
                installed,
            }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `depends_on` entries are `(package, dependency)`.
///
/// A leaf is installed and is not the dependency of any other installed
/// package. A dependency that is not installed does not remove leaf status.
pub fn leaves(installed: &[String], depends_on: &[(String, String)]) -> Vec<String> {
    let installed_set: BTreeSet<&str> = installed.iter().map(String::as_str).collect();
    let mut needed = BTreeSet::new();
    for (package, dependency) in depends_on {
        if installed_set.contains(package.as_str()) && installed_set.contains(dependency.as_str()) {
            needed.insert(dependency.as_str());
        }
    }
    let mut out: Vec<String> = installed
        .iter()
        .filter(|name| !needed.contains(name.as_str()))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Installed packages that list `name` as a dependency.
pub fn dependents(
    name: &str,
    installed: &[String],
    depends_on: &[(String, String)],
) -> Vec<String> {
    let installed_set: BTreeSet<&str> = installed.iter().map(String::as_str).collect();
    let mut out: Vec<String> = depends_on
        .iter()
        .filter(|(package, dependency)| {
            dependency == name && installed_set.contains(package.as_str())
        })
        .map(|(package, _)| package.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinConflict {
    pub bin_name: String,
    pub packages: Vec<String>,
}

/// Scan `<prefix>/bin` and `<prefix>/sbin`.
///
/// A conflict is one filename whose symlink target sits inside more than one
/// record's cellar, or a regular file whose name equals more than one
/// installed package name. A missing directory is empty. On Windows, an
/// unreadable symlink is skipped.
pub fn bin_conflicts(prefix: &Path, records: &[Record]) -> Result<Vec<BinConflict>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dir_name in ["bin", "sbin"] {
        let dir = prefix.join(dir_name);
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let bin_name = entry.file_name().to_string_lossy().to_string();
            let owners = match std::fs::read_link(&path) {
                Ok(target) => owners_of_target(&target, records),
                Err(_) => {
                    if path.is_file() {
                        records
                            .iter()
                            .filter(|r| r.name == bin_name)
                            .map(|r| r.name.clone())
                            .collect()
                    } else {
                        Vec::new()
                    }
                }
            };
            if owners.len() >= 2 {
                found.entry(bin_name).or_default().extend(owners);
            }
        }
    }
    Ok(found
        .into_iter()
        .filter(|(_, packages)| packages.len() >= 2)
        .map(|(bin_name, packages)| BinConflict {
            bin_name,
            packages: packages.into_iter().collect(),
        })
        .collect())
}

fn owners_of_target(target: &Path, records: &[Record]) -> Vec<String> {
    let mut owners: Vec<String> = records
        .iter()
        .filter(|r| target.starts_with(&r.cellar))
        .map(|r| r.name.clone())
        .collect();
    owners.sort();
    owners.dedup();
    owners
}

/// Which installed packages claim `bin_name` given already-resolved targets.
/// `targets` is empty when the path is a regular file: a conflict then needs
/// two records whose name equals `bin_name`.
pub fn conflict_from_targets(
    bin_name: &str,
    targets: &[std::path::PathBuf],
    records: &[Record],
) -> Option<BinConflict> {
    let mut packages = BTreeSet::new();
    if targets.is_empty() {
        for record in records {
            if record.name == bin_name {
                packages.insert(record.name.clone());
            }
        }
    } else {
        for target in targets {
            for owner in owners_of_target(target, records) {
                packages.insert(owner);
            }
        }
    }
    if packages.len() < 2 {
        return None;
    }
    Some(BinConflict {
        bin_name: bin_name.to_string(),
        packages: packages.into_iter().collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupGap {
    pub package: String,
    pub wanted: Option<String>,
    pub installed: Option<String>,
}

/// Members missing from the ledger, or installed at another version when the
/// group entry names one.
pub fn group_gaps(group: &Group, records: &[Record]) -> Vec<GroupGap> {
    let mut out = Vec::new();
    for entry in &group.packages {
        let installed = records.iter().find(|r| r.name == entry.name);
        match (entry.version.as_deref(), installed) {
            (_, None) => out.push(GroupGap {
                package: entry.name.clone(),
                wanted: entry.version.clone(),
                installed: None,
            }),
            (Some(wanted), Some(record)) if record.version != wanted => out.push(GroupGap {
                package: entry.name.clone(),
                wanted: Some(wanted.to_string()),
                installed: Some(record.version.clone()),
            }),
            _ => {}
        }
    }
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{GroupEntry, Kind};
    use crate::snapshot::SnapshotPackage;
    use std::path::PathBuf;

    fn pkg(name: &str, backend: &str, description: &str) -> Package {
        Package {
            name: name.into(),
            version: "1".into(),
            description: description.into(),
            homepage: String::new(),
            backend: backend.into(),
            tags: vec![],
        }
    }

    fn record(name: &str, version: &str) -> Record {
        Record {
            name: name.into(),
            version: version.into(),
            backend: "custom".into(),
            cellar: PathBuf::from(format!("/cellar/{name}/{version}")),
            installed_at: String::new(),
            kind: Kind::Cli,
            depends_on: vec![],
        }
    }

    fn manifest(packages: &[(&str, &str, &str)]) -> SnapshotManifest {
        SnapshotManifest {
            id: "1".into(),
            created_at: String::new(),
            reason: String::new(),
            prefix: PathBuf::from("/prefix"),
            packages: packages
                .iter()
                .map(|(name, version, backend)| SnapshotPackage {
                    name: (*name).into(),
                    version: (*version).into(),
                    backend: (*backend).into(),
                    cellar: PathBuf::from(format!("/cellar/{name}")),
                })
                .collect(),
            bin_links: vec![],
            ledger: serde_json::json!({}),
            kegs: None,
            selected: None,
        }
    }

    #[test]
    fn rank_prefers_exact_then_prefix_then_description() {
        let ranked = rank_packages(
            "jq",
            vec![
                pkg("njq", "winget", "not jq"),
                pkg("jq", "brew", "command"),
                pkg("other", "custom", "a jq clone"),
                pkg("jq-front", "aqua", "prefix"),
            ],
        );
        let names: Vec<_> = ranked.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["jq", "jq-front", "njq", "other"]);
    }

    #[test]
    fn rank_tie_breaks_by_backend_then_name() {
        let ranked = rank_packages(
            "",
            vec![
                pkg("b", "winget", ""),
                pkg("a", "brew", ""),
                pkg("c", "brew", ""),
            ],
        );
        let keys: Vec<_> = ranked
            .iter()
            .map(|p| (p.backend.as_str(), p.name.as_str()))
            .collect();
        assert_eq!(keys, vec![("brew", "a"), ("brew", "c"), ("winget", "b")]);
    }

    #[test]
    fn suggest_backend_knows_aqua_and_github_only() {
        assert_eq!(suggest_backend("aqua:jq"), Some("aqua"));
        assert_eq!(suggest_backend("cli/cli"), Some("github"));
        assert_eq!(suggest_backend("cli--cli"), None);
        assert_eq!(suggest_backend("jq"), None);
        assert_eq!(suggest_backend("a/b/c"), None);
        assert_eq!(suggest_backend(""), None);
        assert_eq!(suggest_backend("has space/repo"), None);
    }

    #[test]
    fn diff_reports_added_removed_and_version_only() {
        let older = manifest(&[
            ("jq", "1.7", "brew"),
            ("tree", "2", "brew"),
            ("same", "1", "brew"),
        ]);
        let newer = manifest(&[
            ("jq", "1.8", "custom"),
            ("same", "1", "winget"),
            ("rg", "14", "github"),
        ]);
        let delta = diff_snapshots(&older, &newer);
        assert_eq!(delta.added, vec!["rg".to_string()]);
        assert_eq!(delta.removed, vec!["tree".to_string()]);
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].name, "jq");
        assert_eq!(delta.changed[0].from, "1.7");
        assert_eq!(delta.changed[0].to, "1.8");
    }

    #[test]
    fn pin_drift_flags_mismatch_and_absence() {
        let records = vec![record("jq", "1.7"), record("tree", "2")];
        let pins = BTreeMap::from([
            ("jq".into(), "1.7".into()),
            ("tree".into(), "1".into()),
            ("gone".into(), "9".into()),
        ]);
        let drift = pin_drift(&records, &pins);
        assert_eq!(drift.len(), 2);
        assert_eq!(drift[0].name, "gone");
        assert_eq!(drift[0].installed, None);
        assert_eq!(drift[1].name, "tree");
        assert_eq!(drift[1].installed.as_deref(), Some("2"));
    }

    #[test]
    fn leaves_ignore_dependencies_that_are_not_installed() {
        let installed = vec!["app".into(), "lib".into(), "tool".into()];
        let edges = vec![
            ("app".into(), "lib".into()),
            ("app".into(), "missing".into()),
            ("outsider".into(), "tool".into()),
        ];
        assert_eq!(
            leaves(&installed, &edges),
            vec!["app".to_string(), "tool".to_string()]
        );
        assert_eq!(
            dependents("lib", &installed, &edges),
            vec!["app".to_string()]
        );
        assert!(dependents("tool", &installed, &edges).is_empty());
    }

    #[test]
    fn conflict_from_targets_needs_two_cellars() {
        let mut jq = record("jq", "1");
        jq.cellar = PathBuf::from("/cellar/jq/1");
        let mut rg = record("rg", "14");
        rg.cellar = PathBuf::from("/cellar/rg/14");
        let records = vec![jq, rg];
        let conflict = conflict_from_targets(
            "shared",
            &[
                PathBuf::from("/cellar/jq/1/bin/shared"),
                PathBuf::from("/cellar/rg/14/bin/shared"),
            ],
            &records,
        )
        .unwrap();
        assert_eq!(conflict.packages, vec!["jq".to_string(), "rg".to_string()]);
        assert!(
            conflict_from_targets(
                "shared",
                &[PathBuf::from("/cellar/jq/1/bin/shared")],
                &records
            )
            .is_none()
        );
    }

    #[test]
    fn group_gaps_skip_a_satisfied_member() {
        let group = Group {
            name: "dev".into(),
            description: String::new(),
            packages: vec![
                GroupEntry {
                    name: "jq".into(),
                    backend: None,
                    version: Some("1.7".into()),
                    kind: Kind::Cli,
                    note: None,
                },
                GroupEntry {
                    name: "tree".into(),
                    backend: None,
                    version: Some("2".into()),
                    kind: Kind::Cli,
                    note: None,
                },
                GroupEntry {
                    name: "gone".into(),
                    backend: None,
                    version: None,
                    kind: Kind::Cli,
                    note: None,
                },
            ],
        };
        let gaps = group_gaps(&group, &[record("jq", "1.8"), record("tree", "2")]);
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].package, "gone");
        assert_eq!(gaps[0].installed, None);
        assert_eq!(gaps[1].package, "jq");
        assert_eq!(gaps[1].installed.as_deref(), Some("1.8"));
        assert_eq!(gaps[1].wanted.as_deref(), Some("1.7"));
    }

    #[cfg(unix)]
    #[test]
    fn bin_conflicts_reads_symlinks_in_a_prefix() {
        let root = std::env::temp_dir().join(format!(
            "maca-conflicts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let jq = root.join("cellar/jq/1");
        let rg = root.join("cellar/rg/14");
        std::fs::create_dir_all(jq.join("bin")).unwrap();
        std::fs::create_dir_all(rg.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink(jq.join("bin/shared"), root.join("bin/shared-jq")).unwrap();
        // One filename cannot be two symlinks. Point one link at a path that
        // is recorded under both cellars by giving both records the same
        // prefix and using conflict_from_targets for the two-target case.
        // Here a single link has one owner, so the scan is empty.
        let mut jq_rec = record("jq", "1");
        jq_rec.cellar = jq.clone();
        let mut rg_rec = record("rg", "14");
        rg_rec.cellar = rg.clone();
        let none = bin_conflicts(&root, &[jq_rec.clone(), rg_rec.clone()]).unwrap();
        assert!(none.is_empty(), "{none:?}");
        // A regular file named like two records is not two names. Build the
        // two-owner case through the same scanner by nesting one cellar
        // inside the other so one symlink is inside both.
        let nested = jq.join("also-rg");
        std::fs::create_dir_all(&nested).unwrap();
        rg_rec.cellar = nested.clone();
        std::os::unix::fs::symlink(nested.join("bin/shared"), root.join("bin/shared")).unwrap();
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        let found = bin_conflicts(&root, &[jq_rec, rg_rec]).unwrap();
        assert!(
            found
                .iter()
                .any(|c| c.bin_name == "shared" && c.packages.len() == 2),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_bin_dir_is_not_a_conflict() {
        let root = std::env::temp_dir().join(format!(
            "maca-conflicts-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(bin_conflicts(&root, &[]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
