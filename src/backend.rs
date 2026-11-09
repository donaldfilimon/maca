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

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub prefix: std::path::PathBuf,
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
