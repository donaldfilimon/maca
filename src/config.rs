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
    /// Override the cellar root. If set, kegs live here while bin/opt/share stay
    /// under the prefix. Empty => use <prefix>/cellar.
    #[serde(default)]
    pub cellar: Option<PathBuf>,
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
        if let Ok(env) = std::env::var("MACA_PREFIX")
            && !env.trim().is_empty()
        {
            return PathBuf::from(env);
        }
        self.prefix_with_env(None)
    }

    /// Prefix resolution given an explicit `MACA_PREFIX` value (pure, testable).
    pub fn prefix_with_env(&self, env_value: Option<&str>) -> PathBuf {
        if let Some(env) = env_value
            && !env.trim().is_empty()
        {
            return PathBuf::from(env);
        }
        if let Some(p) = &self.prefix {
            return p.clone();
        }
        default_prefix()
    }

    /// Cellar root for kegs.
    ///
    /// Resolution: `MACA_CELLAR` > `cellar` field > `<prefix>/cellar`.
    pub fn cellar(&self, prefix: &std::path::Path) -> PathBuf {
        self.cellar_with_env(std::env::var("MACA_CELLAR").ok().as_deref(), prefix)
    }

    /// Cellar resolution given an explicit `MACA_CELLAR` value (pure, testable).
    pub fn cellar_with_env(&self, env_value: Option<&str>, prefix: &std::path::Path) -> PathBuf {
        if let Some(env) = env_value
            && !env.trim().is_empty()
        {
            return PathBuf::from(env);
        }
        if let Some(c) = &self.cellar {
            if c.is_absolute() {
                return c.clone();
            }
            return prefix.join(c);
        }
        cellar_dir(prefix)
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
    if let Some(env) = env_value
        && !env.trim().is_empty()
    {
        return PathBuf::from(env);
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

/// The nearest `maca.toml` walking up from `start`, if one names a prefix.
///
/// This is how a directory gets its own install root without a flag: a project
/// checks in `prefix = "..."` and every command run inside it uses that
/// prefix. `--prefix` and `MACA_PREFIX` still win, because both are explicit.
///
/// A `maca.toml` that cannot be read or parsed is an error: silently skipping
/// it would install into the global prefix when the project asked for its own.
pub fn project_prefix(start: &std::path::Path) -> Result<Option<PathBuf>> {
    project_value(start, |cfg| cfg.prefix)
}

/// The nearest `maca.toml` walking up from `start`, if one names a cellar root.
///
/// Combined with `project_prefix`, this lets a project share a common
/// keg location while keeping its own bin/opt/share links under its prefix.
pub fn project_cellar(start: &std::path::Path) -> Result<Option<PathBuf>> {
    project_value(start, |cfg| cfg.cellar)
}

/// Walk up from `start` to the first `maca.toml` for which `pick` yields a
/// non-empty path, resolving a relative path against that file's directory.
fn project_value(
    start: &std::path::Path,
    pick: impl Fn(Config) -> Option<PathBuf>,
) -> Result<Option<PathBuf>> {
    let mut dir = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(cur) = dir {
        let file = cur.join("maca.toml");
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                let cfg: Config = toml::from_str(&text)
                    .map_err(|e| Error::Config(format!("parse {}: {e}", file.display())))?;
                if let Some(p) = pick(cfg).filter(|p| !p.as_os_str().is_empty()) {
                    return Ok(Some(if p.is_absolute() { p } else { cur.join(p) }));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Config(format!("read {}: {e}", file.display())));
            }
        }
        dir = cur.parent();
    }
    Ok(None)
}

/// True when `path` lies inside `root`. Compared as written first, then with
/// both sides resolved, so a prefix reached through a symlink still matches.
pub fn path_under(path: &std::path::Path, root: &std::path::Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    match (std::fs::canonicalize(root), canonical_lenient(path)) {
        (Ok(r), Some(p)) => p.starts_with(r),
        _ => false,
    }
}

/// Canonicalize `path`, or, if it no longer exists, its nearest existing
/// ancestor with the missing tail re-attached.
fn canonical_lenient(path: &std::path::Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            let mut out = c;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        tail.push(cur.file_name()?.to_os_string());
        cur = cur.parent()?;
    }
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
    fn a_project_file_scopes_the_prefix_and_a_nearer_one_wins() {
        let root = std::env::temp_dir().join(format!("maca-proj-{}", std::process::id()));
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("maca.toml"), "prefix = \"prefix\"\n").unwrap();
        assert_eq!(project_prefix(&nested).unwrap(), Some(root.join("prefix")));

        // The nearer file wins over the one further up.
        std::fs::write(nested.join("maca.toml"), "prefix = \"/opt/near\"\n").unwrap();
        assert_eq!(
            project_prefix(&nested).unwrap(),
            Some(PathBuf::from("/opt/near"))
        );

        // A file that names no prefix is not a project, so the walk continues.
        std::fs::write(nested.join("maca.toml"), "default_backend = \"brew\"\n").unwrap();
        assert_eq!(project_prefix(&nested).unwrap(), Some(root.join("prefix")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unparseable_project_file_is_an_error_not_a_fallback() {
        let root = std::env::temp_dir().join(format!("maca-badproj-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("maca.toml"), "prefix = [not toml\n").unwrap();
        assert!(project_prefix(&root).is_err());
        assert!(project_cellar(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_file_can_name_a_cellar_relative_to_itself() {
        let root = std::env::temp_dir().join(format!("maca-cellarproj-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("maca.toml"), "cellar = \"kegs\"\n").unwrap();
        assert_eq!(
            project_cellar(&root.join("a/b")).unwrap(),
            Some(root.join("kegs"))
        );
        assert_eq!(project_prefix(&root.join("a/b")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
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
