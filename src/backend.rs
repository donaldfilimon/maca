//! Shared package types and backend dispatch.
//!
//! Each backend implements search/info/install via native HTTP APIs.
//! No backend shells out to `brew` or `winget` for catalog operations.
//! Uses Rust 2024 native `async fn` in traits (no proc-macro needed).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Auto,
    Brew,
    Winget,
    Custom,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Brew => "brew",
            Self::Winget => "winget",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "brew" => Self::Brew,
            "winget" => Self::Winget,
            "custom" => Self::Custom,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub backend: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub prefix: std::path::PathBuf,
    /// Package kind recorded in the ledger (app, plugin, cli, ...).
    pub kind: crate::group::Kind,
    /// Compile from source instead of using a prebuilt bottle/installer.
    pub build_from_source: bool,
    /// Skip automatic snapshots before mutating commands.
    pub no_snapshot: bool,
    /// Concurrent install jobs for multi-package commands.
    pub jobs: usize,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            prefix: std::path::PathBuf::new(),
            kind: crate::group::Kind::App,
            build_from_source: false,
            no_snapshot: false,
            jobs: 4,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstallOutcome {
    pub name: String,
    pub version: String,
    pub backend: String,
    pub cellar_path: std::path::PathBuf,
    pub linked_bins: Vec<std::path::PathBuf>,
}

/// Resolve which concrete backend handles a request.
pub fn resolve(kind: BackendKind) -> BackendKind {
    match kind {
        BackendKind::Auto => auto_default(),
        other => other,
    }
}

pub fn auto_default() -> BackendKind {
    // Auto order: custom (local, fast) -> brew (macOS/Linux) -> winget (Windows catalog).
    // Callers try each in order for search; for install, first backend whose
    // `plan()` succeeds wins.
    BackendKind::Custom
}

/// Ordered fallback list for `--backend auto` search.
pub fn auto_search_order() -> [BackendKind; 3] {
    [BackendKind::Custom, BackendKind::Brew, BackendKind::Winget]
}

/// Search every backend in auto order, merging results. Backends run
/// concurrently so one slow or failing catalog does not stall the others.
pub async fn search_all(query: &str, limit: usize) -> Vec<Package> {
    let custom_reg = match custom::Registry::load_merged(&[]).await {
        Ok(reg) => Some(reg),
        Err(e) => {
            tracing::warn!("custom registry: {e}");
            None
        }
    };
    let brew = brew::BrewBackend::new();
    let winget = winget::WingetBackend::new();
    let (custom_res, brew_res, winget_res) = tokio::join!(
        async {
            match custom_reg {
                Some(reg) => CustomBackend::new(reg).search(query, limit),
                None => Ok(vec![]),
            }
        },
        brew.search(query, limit),
        winget.search(query, limit),
    );
    let mut all = Vec::new();
    for result in [custom_res, brew_res, winget_res] {
        match result {
            Ok(mut pkgs) => all.append(&mut pkgs),
            Err(e) => tracing::warn!("backend search: {e}"),
        }
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.dedup_by(|a, b| a.name == b.name && a.backend == b.backend);
    all
}

/// Install `name` with an explicit backend name (`brew`, `winget`, `custom`).
pub async fn install_with_backend(
    backend: &str,
    name: &str,
    opts: &InstallOptions,
) -> Result<InstallOutcome> {
    let kind = BackendKind::from_str_lossy(backend);
    match kind {
        BackendKind::Brew => brew::BrewBackend::new().install(name, opts).await,
        BackendKind::Winget => winget::WingetBackend::new().install(name, opts).await,
        BackendKind::Custom => {
            let reg = custom::Registry::load_local()?;
            custom::CustomBackend::new(reg).install(name, opts).await
        }
        BackendKind::Auto => auto_install(name, opts).await,
    }
}

async fn auto_install(name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
    if let Ok(reg) = custom::Registry::load_local() {
        let custom_backend = custom::CustomBackend::new(reg);
        if custom_backend.plan(name).is_ok() {
            return custom_backend.install(name, opts).await;
        }
    }
    if brew::BrewBackend::new().plan(name).await.is_ok() {
        return brew::BrewBackend::new().install(name, opts).await;
    }
    winget::WingetBackend::new().install(name, opts).await
}

/// Remove an installed package by name, using the prefix's ledger.
pub fn remove_with_prefix(prefix: &std::path::Path, name: &str) -> Result<()> {
    let mut ledger = state::Ledger::load()?;
    let record = ledger
        .get(name)
        .cloned()
        .ok_or_else(|| crate::Error::NotFound(format!("package '{name}' is not installed")))?;
    if record.cellar.exists() {
        std::fs::remove_dir_all(&record.cellar)?;
        if let Some(parent) = record.cellar.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    let bin_dir = config::bin_dir(prefix);
    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)?.flatten() {
            let link = entry.path();
            if let Ok(target) = read_link_target(&link) {
                if target.starts_with(&record.cellar) && !target.exists() {
                    let _ = std::fs::remove_file(&link);
                }
            }
        }
    }
    let opt_link = prefix.join("opt").join(name);
    if opt_link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&opt_link);
    }
    ledger.remove(name);
    ledger.save()?;
    Ok(())
}

fn read_link_target(link: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let _ = link;
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "not a symlink on windows",
        ))
    }
    #[cfg(not(windows))]
    {
        std::fs::read_link(link)
    }
}

/// Upgrade a single installed package. Returns a human-readable outcome.
pub async fn upgrade_with_prefix(prefix: &std::path::Path, name: &str) -> Result<String> {
    let ledger = state::Ledger::load()?;
    let record = ledger
        .get(name)
        .cloned()
        .ok_or_else(|| crate::Error::NotFound(format!("package '{name}' is not installed")))?;
    let opts = InstallOptions {
        dry_run: false,
        prefix: prefix.to_path_buf(),
        ..Default::default()
    };
    let outcome = install_with_backend(&record.backend, &record.name, &opts).await?;
    Ok(format!(
        "upgraded {} {} -> {}",
        outcome.name, record.version, outcome.version
    ))
}

use crate::config;
use crate::custom::{self, CustomBackend};
use crate::state;
use crate::{Result, brew, winget};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_roundtrip() {
        assert_eq!(BackendKind::from_str_lossy("brew"), BackendKind::Brew);
        assert_eq!(BackendKind::from_str_lossy("WINGET"), BackendKind::Winget);
        assert_eq!(BackendKind::from_str_lossy(""), BackendKind::Auto);
        assert_eq!(BackendKind::Brew.as_str(), "brew");
    }

    #[test]
    fn resolve_auto_is_custom_first() {
        assert_eq!(resolve(BackendKind::Auto), BackendKind::Custom);
    }
}
