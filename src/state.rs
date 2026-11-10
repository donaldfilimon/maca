//! Installed-package ledger (`installed.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ledger {
    #[serde(default)]
    pub packages: Vec<Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub cellar: PathBuf,
    #[serde(default)]
    pub installed_at: String,
    /// How the package is used: app, plugin, cli, library, other.
    #[serde(default = "default_kind")]
    pub kind: crate::group::Kind,
    /// Names of the maca-managed packages this one needs at run time
    /// (Homebrew `dependencies`). `sync --prune` keeps them installed while
    /// a locked package needs them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

fn default_kind() -> crate::group::Kind {
    crate::group::Kind::App
}

impl Ledger {
    pub fn load() -> Result<Self> {
        let path = config::installed_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let ledger: Ledger = serde_json::from_str(&text)
            .map_err(|e| crate::Error::Config(format!("parse {}: {e}", path.display())))?;
        Ok(ledger)
    }

    pub fn save(&self) -> Result<()> {
        let path = config::installed_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| crate::Error::Other(anyhow::anyhow!(e)))?;
        crate::install::atomic_write(&path, text.as_bytes())
    }

    pub fn get(&self, name: &str) -> Option<&Record> {
        self.packages.iter().find(|r| r.name == name)
    }

    pub fn upsert(&mut self, rec: Record) {
        if let Some(existing) = self.packages.iter_mut().find(|r| r.name == rec.name) {
            *existing = rec;
        } else {
            self.packages.push(rec);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|r| r.name != name);
        self.packages.len() != before
    }
}

pub fn now_rfc3339() -> String {
    // Avoid chrono: use a simple UTC timestamp via std time.
    // Full RFC3339 without tz conversion indent; second-precision UTC.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (d, t) = days_to_ymd_hms(secs);
    format!("{d}T{t}Z")
}

fn days_to_ymd_hms(secs: u64) -> (String, String) {
    // Minimal civil-date conversion; enough for ledger display.
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 1970-01-01 weekday offset; use Howard Hinnant's algorithm.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{h:02}:{mi:02}:{s:02}"),
    )
}

pub fn record(name: &str, version: &str, backend: &str, cellar: PathBuf) -> Record {
    Record {
        name: name.to_string(),
        version: version.to_string(),
        backend: backend.to_string(),
        cellar,
        installed_at: now_rfc3339(),
        kind: crate::group::Kind::App,
        depends_on: Vec::new(),
    }
}

/// Run a load-modify-save of the ledger under both an in-process mutex
/// (concurrent install tasks) and a cross-process file lock on
/// `<data>/.ledger.lock`. The ledger is shared by every prefix, so the
/// per-prefix lock alone does not serialize two maca processes that use
/// different prefixes.
pub fn with_ledger_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = DataLock::acquire(&LEDGER_LOCK, ".ledger.lock")?;
    f()
}

/// Lock for a load-modify-save of the other data-dir files shared by every
/// prefix: `pins.json`, `groups/` and `custom-registry.json`. Lock order is
/// prefix lock, then state lock, then ledger lock; never the reverse.
pub fn state_lock() -> Result<DataLock> {
    DataLock::acquire(&STATE_LOCK, ".state.lock")
}

/// A data-dir lock: an in-process mutex (orders threads) plus an OS file lock
/// (orders processes). Released on drop. Neither part is re-entrant.
pub struct DataLock {
    _file: std::fs::File,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl DataLock {
    fn acquire(mutex: &'static std::sync::Mutex<()>, file_name: &str) -> Result<Self> {
        let guard = mutex
            .lock()
            .map_err(|_| crate::Error::Other(anyhow::anyhow!("{file_name} lock poisoned")))?;
        let dir = config::data_dir();
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(file_name))?;
        file.lock()?;
        Ok(Self {
            _file: file,
            _guard: guard,
        })
    }
}

/// The ledger holds one record per package name for the whole data dir, so
/// a package can be installed in only one prefix per `MACA_HOME`. Returns the
/// existing record when `name` is installed in a prefix other than the one
/// whose cellar is `cellar_root`.
pub fn installed_elsewhere(name: &str, cellar_root: &std::path::Path) -> Result<Option<Record>> {
    Ok(Ledger::load()?
        .get(name)
        .filter(|r| r.cellar.exists() && !config::path_under(&r.cellar, cellar_root))
        .cloned())
}

pub fn elsewhere_error(name: &str, existing: &Record) -> crate::Error {
    crate::Error::Unsupported(format!(
        "{name} is already installed in another prefix ({}); a data dir records one install per package, so use a separate MACA_HOME for a second prefix",
        existing.cellar.display()
    ))
}

pub fn upsert_record(rec: Record) -> Result<()> {
    with_ledger_lock(|| {
        let mut ledger = Ledger::load()?;
        // Backstop for every install path: never overwrite a record that
        // belongs to a keg in another prefix (that would orphan it).
        if let Some(existing) = ledger.get(&rec.name) {
            let root_of =
                |p: &std::path::Path| p.parent().and_then(|n| n.parent()).map(PathBuf::from);
            if existing.cellar.exists()
                && root_of(&existing.cellar).is_some_and(|r| !config::path_under(&rec.cellar, &r))
            {
                return Err(elsewhere_error(&rec.name, existing));
            }
        }
        ledger.upsert(rec);
        ledger.save()
    })
}

pub fn remove_record(name: &str) -> Result<bool> {
    with_ledger_lock(|| {
        let mut ledger = Ledger::load()?;
        let removed = ledger.remove(name);
        ledger.save()?;
        Ok(removed)
    })
}

pub fn pins_path() -> PathBuf {
    config::data_dir().join("pins.json")
}

/// Pinned packages: name -> the version recorded when the pin was made.
/// A pinned package is held: `upgrade` never touches it. A corrupt file is an
/// error, not "no pins", so a broken file cannot silently release every pin.
pub fn load_pins() -> Result<std::collections::BTreeMap<String, String>> {
    let path = pins_path();
    if !path.exists() {
        return Ok(Default::default());
    }
    serde_json::from_str(&std::fs::read_to_string(&path)?)
        .map_err(|e| crate::Error::Config(format!("parse {}: {e}", path.display())))
}

pub fn save_pins(pins: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let text = serde_json::to_string_pretty(pins)?;
    crate::install::atomic_write(&pins_path(), text.as_bytes())
}

pub fn is_pinned(name: &str) -> Result<bool> {
    Ok(load_pins()?.keys().any(|k| k.eq_ignore_ascii_case(name)))
}

static LEDGER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn record_with_kind(
    name: &str,
    version: &str,
    backend: &str,
    cellar: PathBuf,
    kind: crate::group::Kind,
) -> Record {
    let mut r = record(name, version, backend, cellar);
    r.kind = kind;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_upsert_replaces() {
        let mut l = Ledger::default();
        l.upsert(record("jq", "1.8", "brew", PathBuf::from("/tmp/a")));
        l.upsert(record("jq", "1.9", "brew", PathBuf::from("/tmp/b")));
        assert_eq!(l.packages.len(), 1);
        assert_eq!(l.get("jq").unwrap().version, "1.9");
        assert!(l.remove("jq"));
        assert!(l.get("jq").is_none());
    }

    #[test]
    fn civil_date_known() {
        let (d, t) = days_to_ymd_hms(0);
        assert_eq!(d, "1970-01-01");
        assert_eq!(t, "00:00:00");
    }
}
