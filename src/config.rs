//! Paths, prefix discovery and user config.
//!
//! Layout (data dir = `$MACA_HOME` if set, else `~/.maca` on macOS and
//! `<XDG data dir>/maca` elsewhere):
//! ```text
//! <data>/config.toml          # optional user config
//! <data>/installed.json       # install ledger
//! <data>/custom-registry.json # custom backend registry
//! <prefix>/cellar/<name>/<version>/...  # extracted artifacts
//! <prefix>/bin/<exe>                    # symlinks to cellar bins
//! ```
//!
//! Prefix default is `$MACA_PREFIX` if set, else `<data>/prefix`.
//! On macOS this resolves to `~/.maca/prefix` (deliberately space-free, so
//! not `~/Library/Application Support`), on Linux to
//! `~/.local/share/maca/prefix`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Override install prefix. Empty => default layout.
    #[serde(default)]
    pub prefix: Option<PathBuf>,
    /// Default backend for install/search when `--backend` is omitted.
    #[serde(default)]
    pub default_backend: Option<String>,
    /// Extra custom registry URLs (HTTP) to consult after the local file.
    #[serde(default)]
    pub custom_registry_urls: Vec<String>,
    /// GitHub `owner/repo` used by `maca self-update`.
    #[serde(default)]
    pub update_repo: Option<String>,
}

impl Config {
    /// Load from `<data_dir>/config.toml` if present, else defaults.
    pub fn load() -> Result<Self> {
        let path = data_dir().join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("parse {}: {e}", path.display())))?;
        Ok(cfg)
    }

    pub fn prefix(&self) -> PathBuf {
        if let Ok(env) = std::env::var("MACA_PREFIX") {
            if !env.trim().is_empty() {
                return PathBuf::from(env);
            }
        }
        self.prefix_with_env(None)
    }

    /// Prefix resolution given an explicit `MACA_PREFIX` value (pure, testable).
    pub fn prefix_with_env(&self, env_value: Option<&str>) -> PathBuf {
        if let Some(env) = env_value {
            if !env.trim().is_empty() {
                return PathBuf::from(env);
            }
        }
        if let Some(p) = &self.prefix {
            return p.clone();
        }
        default_prefix()
    }
}

/// Data dir for maca state. `MACA_HOME` overrides the platform default, which
/// relocates the ledger, pins, snapshots, groups, registry, cache and logs
/// (and the default prefix, `<data>/prefix`) in one step.
pub fn data_dir() -> PathBuf {
    data_dir_with_env(std::env::var("MACA_HOME").ok().as_deref())
}

/// Data dir resolution given an explicit `MACA_HOME` value (pure, testable).
pub fn data_dir_with_env(env_value: Option<&str>) -> PathBuf {
    if let Some(env) = env_value {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    default_data_dir()
}

fn default_data_dir() -> PathBuf {
    if let Some(base) = directories::BaseDirs::new() {
        // Prefer a space-free home: many build recipes split on whitespace,
        // and `~/Library/Application Support` breaks them.
        #[cfg(target_os = "macos")]
        {
            return base.home_dir().join(".maca");
        }
        #[cfg(not(target_os = "macos"))]
        {
            let data = base.data_dir();
            return data.join("maca");
        }
    }
    // Fallback: ~/.local/share/maca (Linux) / ~/Library/Application Support/maca (macOS)
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::home_dir() {
            return home.join(".maca");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(home) = std::env::home_dir() {
            return home.join(".local/share/maca");
        }
    }
    PathBuf::from(".maca")
}

/// Default install prefix. Space-free so source build recipes work.
fn default_prefix() -> PathBuf {
    data_dir().join("prefix")
}

pub fn installed_path() -> PathBuf {
    data_dir().join("installed.json")
}

pub fn custom_registry_path() -> PathBuf {
    data_dir().join("custom-registry.json")
}

pub fn cellar_dir(prefix: &std::path::Path) -> PathBuf {
    prefix.join("cellar")
}

pub fn bin_dir(prefix: &std::path::Path) -> PathBuf {
    prefix.join("bin")
}

pub fn ensure_dirs(prefix: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(cellar_dir(prefix))?;
    std::fs::create_dir_all(bin_dir(prefix))?;
    std::fs::create_dir_all(data_dir())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        let cfg = Config::default();
        assert_eq!(
            cfg.prefix_with_env(Some("/tmp/maca-test-prefix")),
            PathBuf::from("/tmp/maca-test-prefix")
        );
    }

    #[test]
    fn explicit_prefix_used_when_env_empty() {
        let cfg = Config {
            prefix: Some(PathBuf::from("/opt/maca")),
            ..Default::default()
        };
        assert_eq!(cfg.prefix_with_env(Some("")), PathBuf::from("/opt/maca"));
        assert_eq!(cfg.prefix_with_env(None), PathBuf::from("/opt/maca"));
    }

    #[test]
    fn maca_home_overrides_data_dir() {
        assert_eq!(
            data_dir_with_env(Some("/tmp/maca-home")),
            PathBuf::from("/tmp/maca-home")
        );
        assert_eq!(data_dir_with_env(Some("  ")), default_data_dir());
        assert_eq!(data_dir_with_env(None), default_data_dir());
    }

    #[test]
    fn data_dir_ends_with_maca_home() {
        let d = default_data_dir();
        let name = d.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(
            name == "maca" || name == ".maca",
            "unexpected data dir name: {name}"
        );
    }

    #[test]
    fn config_roundtrip() {
        let cfg = Config {
            prefix: Some(PathBuf::from("/tmp/p")),
            default_backend: Some("brew".into()),
            custom_registry_urls: vec!["https://example.com/reg.json".into()],
            ..Default::default()
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.prefix.unwrap(), PathBuf::from("/tmp/p"));
        assert_eq!(back.default_backend.as_deref(), Some("brew"));
    }
}
