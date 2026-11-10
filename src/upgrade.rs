//! Upgrade orchestration: plan without the prefix lock, apply under it.
//!
//! Version checks hit the network and can be slow, so they run with no lock
//! held. Only the state-changing phase takes `<prefix>/.maca.lock`, and it
//! re-validates every planned upgrade against the ledger and pins as they are
//! *after* the lock is held: a plan computed earlier (by this process or by a
//! concurrent `maca upgrade`) is applied only if the installed record it was
//! computed from is unchanged. Two upgrades racing on the same package
//! therefore cannot both apply a stale plan.
//!
//! The environment (ledger, pins, catalog, snapshots, installer) sits behind
//! [`UpgradeEnv`] so the lock discipline is unit-tested without network or a
//! real data directory.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::lock::PrefixLock;
use crate::state::{Ledger, Record};
use crate::{Error, Result, config};

/// An upgrade decided in the check phase, tied to the record it came from.
#[derive(Debug, Clone)]
pub struct Planned {
    /// The installed record the plan was computed from.
    pub record: Record,
    /// The catalog version seen in the check phase.
    pub latest: String,
}

/// Why a planned upgrade is no longer applied after the lock was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stale {
    Removed,
    Changed { now: String },
    Pinned,
    NotNewer,
}

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stale::Removed => write!(f, "it was removed since the check"),
            Stale::Changed { now } => write!(f, "it changed to {now} since the check"),
            Stale::Pinned => write!(f, "it was pinned since the check"),
            Stale::NotNewer => write!(f, "the installed version is no longer older"),
        }
    }
}

/// Re-check a plan against current state. `Ok(record)` is the current record
/// to upgrade from; `Err` says why the plan is stale.
pub fn revalidate(
    planned: &Planned,
    ledger: &Ledger,
    pins: &BTreeMap<String, String>,
) -> std::result::Result<Record, Stale> {
    let Some(now) = ledger
        .packages
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(&planned.record.name))
    else {
        return Err(Stale::Removed);
    };
    if now.version != planned.record.version
        || now.backend != planned.record.backend
        || now.cellar != planned.record.cellar
    {
        return Err(Stale::Changed {
            now: format!(
                "{} ({}, {})",
                now.version,
                now.backend,
                now.cellar.display()
            ),
        });
    }
    if pins
        .keys()
        .any(|k| k.eq_ignore_ascii_case(&planned.record.name))
    {
        return Err(Stale::Pinned);
    }
    if !crate::backend::is_newer_version(&planned.latest, &now.version) {
        return Err(Stale::NotNewer);
    }
    Ok(now.clone())
}

/// Everything the orchestration needs from the outside world.
pub trait UpgradeEnv {
    fn ledger(&self) -> Result<Ledger>;
    fn pins(&self) -> Result<BTreeMap<String, String>>;
    /// Version the catalog offers now (network; called without the lock).
    fn latest(&self, record: &Record) -> impl std::future::Future<Output = Result<String>>;
    /// Take a rollback point; called with the lock held. `None` = disabled.
    fn snapshot(&self) -> Result<Option<String>>;
    fn restore(&self, snapshot_id: &str) -> Result<()>;
    /// Perform one upgrade; called with the lock held.
    fn apply(&self, from: &Record) -> impl std::future::Future<Output = Result<()>>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub upgraded: usize,
    pub current: usize,
    pub failed: usize,
    pub unchecked: usize,
    pub stale: usize,
    /// Selected packages recorded under another prefix (not touched).
    pub elsewhere: usize,
}

/// Records selected by `only` (empty = all), split into those installed under
/// `prefix` and the names of those that are not (the ledger spans every
/// prefix; the others are reported, never silently dropped).
fn candidates(
    ledger: &Ledger,
    prefix: &Path,
    only: &HashSet<String>,
) -> (Vec<Record>, Vec<String>) {
    let cellar = config::cellar_dir(prefix);
    let (mut here, mut elsewhere) = (Vec::new(), Vec::new());
    for r in &ledger.packages {
        if !(only.is_empty() || only.contains(&r.name.to_ascii_lowercase())) {
            continue;
        }
        if config::path_under(&r.cellar, &cellar) {
            here.push(r.clone());
        } else {
            elsewhere.push(r.name.clone());
        }
    }
    (here, elsewhere)
}

/// Upgrade installed packages: check without the lock, then lock, re-validate
/// and apply. A failed apply rolls back to the snapshot and fails the call.
pub async fn run<E: UpgradeEnv>(env: &E, prefix: &Path, only: &[String]) -> Result<Summary> {
    let mut summary = Summary::default();
    let ledger = env.ledger()?;
    if ledger.packages.is_empty() {
        println!("no packages installed");
        return Ok(summary);
    }
    let only_set: HashSet<String> = only.iter().map(|s| s.to_ascii_lowercase()).collect();
    // A corrupt pins file must stop the upgrade, not release every pin.
    let pins = env.pins()?;

    // Phase 1: network checks, no lock held.
    let mut plan: Vec<Planned> = Vec::new();
    let (here, elsewhere) = candidates(&ledger, prefix, &only_set);
    if !elsewhere.is_empty() {
        eprintln!(
            "skipped (not installed under this prefix): {}",
            elsewhere.join(", ")
        );
        summary.elsewhere = elsewhere.len();
    }
    for record in here {
        if pins.keys().any(|k| k.eq_ignore_ascii_case(&record.name)) {
            println!("{} {} is pinned; skipping", record.name, record.version);
            summary.current += 1;
            continue;
        }
        match env.latest(&record).await {
            Err(e) => {
                eprintln!("warning: could not check {}: {e}", record.name);
                summary.unchecked += 1;
            }
            Ok(latest) if crate::backend::is_newer_version(&latest, &record.version) => {
                plan.push(Planned { record, latest });
            }
            Ok(_) => {
                println!("{} {} is up to date", record.name, record.version);
                summary.current += 1;
            }
        }
    }

    // Phase 2: lock, re-validate against current state, apply.
    if !plan.is_empty() {
        let _lock = PrefixLock::acquire(prefix)?;
        let ledger = env.ledger()?;
        let pins = env.pins()?;
        let mut valid: Vec<(Planned, Record)> = Vec::new();
        for planned in plan {
            match revalidate(&planned, &ledger, &pins) {
                Ok(now) => valid.push((planned, now)),
                Err(why) => {
                    println!("{} not upgraded: {why}", planned.record.name);
                    summary.stale += 1;
                }
            }
        }
        if !valid.is_empty() {
            let snapshot = env.snapshot()?;
            for (planned, now) in &valid {
                println!(
                    "upgrading {} {} -> {} ({})",
                    now.name, now.version, planned.latest, now.backend
                );
                match env.apply(now).await {
                    Ok(()) => summary.upgraded += 1,
                    Err(e) => {
                        eprintln!("error: upgrade of {} failed: {e}", now.name);
                        summary.failed += 1;
                    }
                }
            }
            if summary.failed > 0
                && let Some(id) = &snapshot
            {
                match env.restore(id) {
                    Ok(()) => eprintln!(
                        "maca: {} package(s) failed to upgrade; rolled back to snapshot {id}",
                        summary.failed
                    ),
                    Err(r) => eprintln!(
                        "maca: {} package(s) failed to upgrade; rollback also failed: {r}",
                        summary.failed
                    ),
                }
            }
        }
    }

    println!(
        "upgrade complete: {} upgraded, {} current, {} failed, {} could not be checked, {} no longer applicable",
        summary.upgraded, summary.current, summary.failed, summary.unchecked, summary.stale
    );
    // A version check that could not be made does not fail the command (it
    // changed nothing); a failed install does, after the rollback.
    if summary.failed > 0 {
        return Err(Error::Other(anyhow::anyhow!(
            "{} package(s) failed to upgrade",
            summary.failed
        )));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn rec(prefix: &Path, name: &str, version: &str) -> Record {
        Record {
            name: name.into(),
            version: version.into(),
            backend: "custom".into(),
            cellar: config::cellar_dir(prefix).join(name).join(version),
            installed_at: String::new(),
            kind: crate::group::Kind::App,
            depends_on: vec![],
        }
    }

    fn temp_prefix(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("maca-upgrade-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    type Hook = Box<dyn Fn(&Fake)>;

    struct Fake {
        prefix: PathBuf,
        ledger: RefCell<Ledger>,
        pins: RefCell<BTreeMap<String, String>>,
        latest: String,
        /// Run during the check phase for each package.
        during_check: RefCell<Option<Hook>>,
        lock_free_in_check: RefCell<Vec<bool>>,
        lock_held_in_apply: RefCell<Vec<bool>>,
        applied: RefCell<Vec<String>>,
        fail_apply: bool,
        restored: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(prefix: &Path, records: Vec<Record>, latest: &str) -> Self {
            Fake {
                prefix: prefix.to_path_buf(),
                ledger: RefCell::new(Ledger { packages: records }),
                pins: RefCell::new(BTreeMap::new()),
                latest: latest.into(),
                during_check: RefCell::new(None),
                lock_free_in_check: RefCell::new(vec![]),
                lock_held_in_apply: RefCell::new(vec![]),
                applied: RefCell::new(vec![]),
                fail_apply: false,
                restored: RefCell::new(vec![]),
            }
        }
        /// Free means acquirable. Other tests in this process spawn child
        /// processes, and a child forked while a lock is held keeps the
        /// descriptor until it execs, so poll briefly before calling it held.
        fn lock_is_free(&self) -> bool {
            for _ in 0..300 {
                if PrefixLock::try_acquire(&self.prefix).unwrap().is_some() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        }
        fn lock_is_held(&self) -> bool {
            PrefixLock::try_acquire(&self.prefix).unwrap().is_none()
        }
    }

    impl UpgradeEnv for Fake {
        fn ledger(&self) -> Result<Ledger> {
            Ok(self.ledger.borrow().clone())
        }
        fn pins(&self) -> Result<BTreeMap<String, String>> {
            Ok(self.pins.borrow().clone())
        }
        async fn latest(&self, _record: &Record) -> Result<String> {
            self.lock_free_in_check
                .borrow_mut()
                .push(self.lock_is_free());
            if let Some(f) = self.during_check.borrow().as_ref() {
                f(self);
            }
            Ok(self.latest.clone())
        }
        fn snapshot(&self) -> Result<Option<String>> {
            Ok(Some("snap1".into()))
        }
        fn restore(&self, id: &str) -> Result<()> {
            self.restored.borrow_mut().push(id.into());
            Ok(())
        }
        async fn apply(&self, from: &Record) -> Result<()> {
            self.lock_held_in_apply
                .borrow_mut()
                .push(self.lock_is_held());
            self.applied.borrow_mut().push(from.name.clone());
            if self.fail_apply {
                return Err(Error::Other(anyhow::anyhow!("boom")));
            }
            let mut l = self.ledger.borrow_mut();
            if let Some(r) = l.packages.iter_mut().find(|r| r.name == from.name) {
                r.version = self.latest.clone();
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn lock_is_not_held_during_checks_but_is_during_apply() {
        let prefix = temp_prefix("phases");
        let env = Fake::new(
            &prefix,
            vec![rec(&prefix, "a", "1.0.0"), rec(&prefix, "b", "1.0.0")],
            "2.0.0",
        );
        let s = run(&env, &prefix, &[]).await.unwrap();
        assert_eq!(s.upgraded, 2);
        assert_eq!(*env.lock_free_in_check.borrow(), vec![true, true]);
        assert_eq!(*env.lock_held_in_apply.borrow(), vec![true, true]);
        // And the lock is released afterwards.
        assert!(env.lock_is_free());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn plan_made_stale_by_a_concurrent_upgrade_is_not_applied() {
        let prefix = temp_prefix("stale");
        let env = Fake::new(&prefix, vec![rec(&prefix, "a", "1.0.0")], "2.0.0");
        // While this process checks (no lock held), another upgrade wins the
        // race and installs the same version.
        *env.during_check.borrow_mut() = Some(Box::new(|f: &Fake| {
            f.ledger.borrow_mut().packages[0].version = "2.0.0".into();
        }));
        let s = run(&env, &prefix, &[]).await.unwrap();
        assert_eq!(s.upgraded, 0);
        assert_eq!(s.stale, 1);
        assert!(env.applied.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn pin_added_or_package_removed_after_the_check_is_respected() {
        let prefix = temp_prefix("pinremoved");
        let env = Fake::new(
            &prefix,
            vec![rec(&prefix, "a", "1.0.0"), rec(&prefix, "b", "1.0.0")],
            "2.0.0",
        );
        *env.during_check.borrow_mut() = Some(Box::new(|f: &Fake| {
            f.ledger.borrow_mut().packages.retain(|r| r.name != "b");
        }));
        let s = run(&env, &prefix, &[]).await.unwrap();
        assert_eq!((s.upgraded, s.stale), (1, 1));
        assert_eq!(*env.applied.borrow(), vec!["a".to_string()]);

        let planned = Planned {
            record: rec(&prefix, "a", "2.0.0"),
            latest: "3.0.0".into(),
        };
        let ledger = Ledger {
            packages: vec![rec(&prefix, "a", "2.0.0")],
        };
        let mut pins = BTreeMap::new();
        pins.insert("A".to_string(), "2.0.0".to_string());
        assert_eq!(
            revalidate(&planned, &ledger, &pins).err(),
            Some(Stale::Pinned)
        );
        assert_eq!(
            revalidate(&planned, &Ledger::default(), &BTreeMap::new()).err(),
            Some(Stale::Removed)
        );
        assert!(revalidate(&planned, &ledger, &BTreeMap::new()).is_ok());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn pin_added_during_the_check_phase_stops_the_apply() {
        let prefix = temp_prefix("pinrace");
        let env = Fake::new(&prefix, vec![rec(&prefix, "a", "1.0.0")], "2.0.0");
        *env.during_check.borrow_mut() = Some(Box::new(|f: &Fake| {
            f.pins.borrow_mut().insert("a".into(), "1.0.0".into());
        }));
        let s = run(&env, &prefix, &[]).await.unwrap();
        assert_eq!((s.upgraded, s.stale), (0, 1));
        assert!(env.applied.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn backend_change_is_reported_with_the_new_backend() {
        let prefix = temp_prefix("backend");
        let planned = Planned {
            record: rec(&prefix, "a", "1.0.0"),
            latest: "2.0.0".into(),
        };
        let mut moved = rec(&prefix, "a", "1.0.0");
        moved.backend = "brew".into();
        let ledger = Ledger {
            packages: vec![moved],
        };
        let why = revalidate(&planned, &ledger, &BTreeMap::new()).unwrap_err();
        assert!(why.to_string().contains("brew"), "{why}");
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn nothing_to_upgrade_never_takes_the_lock_or_a_snapshot() {
        let prefix = temp_prefix("nothing");
        let env = Fake::new(&prefix, vec![rec(&prefix, "a", "2.0.0")], "2.0.0");
        let s = run(&env, &prefix, &[]).await.unwrap();
        assert_eq!((s.upgraded, s.current), (0, 1));
        assert!(env.lock_held_in_apply.borrow().is_empty());
        assert!(env.restored.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn failed_apply_rolls_back_and_fails_the_run() {
        let prefix = temp_prefix("fail");
        let mut env = Fake::new(&prefix, vec![rec(&prefix, "a", "1.0.0")], "2.0.0");
        env.fail_apply = true;
        assert!(run(&env, &prefix, &[]).await.is_err());
        assert_eq!(*env.restored.borrow(), vec!["snap1".to_string()]);
        assert!(env.lock_is_free());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[tokio::test]
    async fn only_filter_and_other_prefixes_are_respected() {
        let prefix = temp_prefix("only");
        let other = temp_prefix("only-other");
        let env = Fake::new(
            &prefix,
            vec![
                rec(&prefix, "a", "1.0.0"),
                rec(&prefix, "b", "1.0.0"),
                rec(&other, "c", "1.0.0"),
            ],
            "2.0.0",
        );
        let s = run(&env, &prefix, &["A".to_string()]).await.unwrap();
        assert_eq!(s.upgraded, 1);
        assert_eq!(*env.applied.borrow(), vec!["a".to_string()]);
        let env2 = Fake::new(&prefix, env.ledger.borrow().packages.clone(), "3.0.0");
        run(&env2, &prefix, &[]).await.unwrap();
        assert!(!env2.applied.borrow().contains(&"c".to_string()));
        // The record under another prefix is reported, not silently dropped.
        let s2 = run(&env2, &prefix, &[]).await.unwrap();
        assert_eq!(s2.elsewhere, 1);
        let _ = std::fs::remove_dir_all(&prefix);
        let _ = std::fs::remove_dir_all(&other);
    }
}
