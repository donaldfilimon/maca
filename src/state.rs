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
        std::fs::write(&path, text)?;
        Ok(())
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

fn now_rfc3339() -> String {
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
    }
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
