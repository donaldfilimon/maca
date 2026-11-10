//! Custom registry backend (local JSON file + optional HTTP mirrors).
//!
//! Registry schema (`custom-registry.json`):
//! ```json
//! {
//!   "packages": [
//!     {
//!       "name": "hello",
//!       "version": "1.0.0",
//!       "description": "Hello world",
//!       "homepage": "https://example.com",
//!       "artifacts": {
//!         "macos-arm64": {
//!           "url": "https://example.com/hello-1.0.0-macos-arm64.tar.gz",
//!           "sha256": "deadbeef...",
//!           "kind": "tar.gz"
//!         }
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! Platform keys accepted: `macos-arm64`, `macos-x64`, `linux-x64`,
//! `linux-arm64`, `windows-x64`, `darwin-aarch64`, `darwin-x86_64`,
//! and bare `macos`, `linux`, `windows` (any arch). `kind` is one of
//! `tar.gz` or `binary` (single executable copied to `bin/`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backend::{InstallOptions, InstallOutcome, InstallPlan, Package};
use crate::config;
use crate::install;
use crate::source;
use crate::state;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    #[serde(default)]
    pub packages: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
    /// Custom build steps run in the extracted source tree. Use `{{prefix}}`
    /// for the install destination. When set, the artifact is treated as a
    /// source archive and built instead of being linked directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub kind: ArtifactKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    #[default]
    #[serde(rename = "tar.gz")]
    TarGz,
    Binary,
}

impl Registry {
    pub fn load_local() -> Result<Self> {
        let path = config::custom_registry_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let reg: Registry = serde_json::from_str(&text)
            .map_err(|e| Error::Config(format!("parse {}: {e}", path.display())))?;
        Ok(reg)
    }

    pub fn save_local(&self) -> Result<()> {
        let path = config::custom_registry_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        install::atomic_write(&path, text.as_bytes())
    }

    /// Local registry merged with the mirrors listed in `config.toml`
    /// (`custom_registry_urls`). Used by install and auto-search paths so a
    /// package found on a mirror can also be installed. An unreachable or
    /// malformed mirror degrades to the local registry rather than blocking
    /// local packages.
    pub async fn load_configured() -> Result<Self> {
        let urls = config::Config::load()?.custom_registry_urls;
        if urls.is_empty() {
            return Self::load_local();
        }
        match Self::load_merged(&urls).await {
            Ok(reg) => Ok(reg),
            Err(e) => {
                tracing::warn!("custom registry mirrors unavailable, using local only: {e}");
                Self::load_local()
            }
        }
    }

    /// Merge local + HTTP mirrors (mirrors are additive, local wins on conflict).
    pub async fn load_merged(extra_urls: &[String]) -> Result<Self> {
        let mut merged = Self::load_local()?;
        for url in extra_urls {
            if !mirror_url_allowed(url) {
                return Err(Error::Config(format!(
                    "custom registry mirror {url} must use https (plain http is allowed only for localhost)"
                )));
            }
            let bytes = install::download(url).await?;
            let remote: Registry = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Network(format!("decoding registry {url}: {e}")))?;
            for entry in remote.packages {
                if !merged.packages.iter().any(|p| p.name == entry.name) {
                    merged.packages.push(entry);
                }
            }
        }
        merged.packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(merged)
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn upsert(&mut self, entry: Entry) {
        if let Some(existing) = self.packages.iter_mut().find(|p| p.name == entry.name) {
            *existing = entry;
        } else {
            self.packages.push(entry);
        }
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() != before
    }
}

/// Pick the artifact for the current host platform.
pub fn pick_artifact(entry: &Entry) -> Option<(String, &Artifact)> {
    let (os, arch) = host_os_arch();
    let exact = format!("{os}-{arch}");
    if let Some(a) = entry.artifacts.get(&exact) {
        return Some((exact, a));
    }
    // Darwin aliases for macOS.
    if os == "macos" {
        let alias = match arch {
            "arm64" => Some("darwin-aarch64"),
            "x64" | "x86_64" => Some("darwin-x86_64"),
            _ => None,
        };
        if let Some(alias) = alias {
            if let Some(a) = entry.artifacts.get(alias) {
                return Some((alias.to_string(), a));
            }
        }
    }
    // x64/x86_64 spelling interop.
    let alt_arch = match arch {
        "x64" => Some("x86_64"),
        "x86_64" => Some("x64"),
        _ => None,
    };
    if let Some(alt) = alt_arch {
        let key = format!("{os}-{alt}");
        if let Some(a) = entry.artifacts.get(&key) {
            return Some((key, a));
        }
    }
    // Bare OS key (any arch).
    if let Some(a) = entry.artifacts.get(os) {
        return Some((os.to_string(), a));
    }
    None
}

fn host_os_arch() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "unknown"
    };
    (os, arch)
}

pub struct CustomBackend {
    pub registry: Registry,
}

impl CustomBackend {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Package>> {
        let q = query.trim().to_ascii_lowercase();
        let mut hits: Vec<Package> = self
            .registry
            .packages
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.name.to_ascii_lowercase().contains(&q)
                    || p.description.to_ascii_lowercase().contains(&q)
            })
            .take(limit.max(1))
            .map(|p| Package {
                name: p.name.clone(),
                version: p.version.clone(),
                description: p.description.clone(),
                homepage: p.homepage.clone(),
                backend: "custom".into(),
                tags: vec!["custom".into()],
            })
            .collect();
        hits.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(hits)
    }

    pub fn info(&self, name: &str) -> Result<Package> {
        let entry = self
            .registry
            .find(name)
            .ok_or_else(|| Error::NotFound(format!("custom package '{name}' not found")))?;
        Ok(Package {
            name: entry.name.clone(),
            version: entry.version.clone(),
            description: entry.description.clone(),
            homepage: entry.homepage.clone(),
            backend: "custom".into(),
            tags: vec!["custom".into()],
        })
    }

    pub fn plan(&self, name: &str) -> Result<InstallPlan> {
        let entry = self
            .registry
            .find(name)
            .ok_or_else(|| Error::NotFound(format!("custom package '{name}' not found")))?;
        // Name and version become path components under the cellar and bin/.
        if !install::valid_component(&entry.name) || !install::valid_component(&entry.version) {
            return Err(Error::Unsupported(format!(
                "custom package {:?} version {:?} is not a valid name/version (no path separators, `.` or `..`)",
                entry.name, entry.version
            )));
        }
        let (key, art) = pick_artifact(entry).ok_or_else(|| {
            Error::Unsupported(format!(
                "custom package '{}' has no artifact for this host (have: {})",
                name,
                entry
                    .artifacts
                    .keys()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        Ok(InstallPlan {
            name: entry.name.clone(),
            version: entry.version.clone(),
            backend: "custom".into(),
            url: art.url.clone(),
            sha256: art.sha256.clone(),
            note: Some(format!("platform key `{key}`")),
        })
    }

    pub async fn install(&self, name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
        let plan = self.plan(name)?;
        let entry = self
            .registry
            .find(name)
            .expect("plan succeeded so entry exists");
        let (_, art) = pick_artifact(entry).expect("plan succeeded");
        if opts.dry_run {
            println!(
                "[dry-run] would install {} {} via {} ({})",
                plan.name, plan.version, plan.backend, plan.url
            );
            return Ok(InstallOutcome {
                name: plan.name.clone(),
                version: plan.version.clone(),
                backend: "custom".into(),
                cellar_path: cellar_for(&opts.prefix, &plan.name, &plan.version),
                linked_bins: vec![],
            });
        }
        let bytes = install::download(&plan.url).await?;
        if plan.sha256.is_none() {
            eprintln!(
                "maca: warning: custom package {} declares no sha256; installing {} UNVERIFIED",
                plan.name, plan.url
            );
        }
        install::verify_sha256(&plan.name, &bytes, plan.sha256.as_deref())?;
        // Source build path: the entry declares build_steps, so the artifact is
        // a source tree that is compiled into the cellar.
        if !entry.build_steps.is_empty() {
            let scratch = std::env::temp_dir().join(format!(
                "maca-custom-src-{}-{}-{}",
                plan.name,
                plan.version,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&scratch);
            std::fs::create_dir_all(&scratch)?;
            let source_dir =
                match install::extract_tar_gz_to_cellar("src", &plan.name, &bytes, &scratch) {
                    Ok(dir) => dir,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&scratch);
                        return Err(e);
                    }
                };
            let cellar = cellar_for(&opts.prefix, &plan.name, &plan.version);
            let built =
                match source::run_steps(&source_dir, &cellar, &entry.build_steps, opts.jobs.max(1))
                {
                    Ok(b) => b,
                    Err(e) => {
                        // Steps install straight into the keg; a failed build
                        // must not leave a partial one that looks installed.
                        let _ = std::fs::remove_dir_all(&cellar);
                        if let Some(name_dir) = cellar.parent() {
                            let _ = std::fs::remove_dir(name_dir);
                        }
                        let _ = std::fs::remove_dir_all(&scratch);
                        return Err(e);
                    }
                };
            let linked = link_all_bins(&cellar, &opts.prefix)?;
            let _ = install::link_share(&cellar, &opts.prefix);
            state::upsert_record(state::record_with_kind(
                &plan.name,
                &plan.version,
                "custom",
                cellar.clone(),
                opts.kind,
            ))?;
            let _ = std::fs::remove_dir_all(&scratch);
            tracing::info!("built {} with custom steps", plan.name);
            let _ = built;
            return Ok(InstallOutcome {
                name: plan.name.clone(),
                version: plan.version.clone(),
                backend: "custom".into(),
                cellar_path: cellar,
                linked_bins: linked,
            });
        }
        match art.kind {
            ArtifactKind::TarGz => {
                let cellar = install::extract_tar_gz_to_cellar(
                    &plan.name,
                    &plan.version,
                    &bytes,
                    &opts.prefix,
                )?;
                // Link any `bin/*` files into the prefix bin.
                let linked = link_all_bins(&cellar, &opts.prefix)?;
                let _ = install::link_share(&cellar, &opts.prefix);
                state::upsert_record(state::record_with_kind(
                    &plan.name,
                    &plan.version,
                    "custom",
                    cellar.clone(),
                    opts.kind,
                ))?;
                Ok(InstallOutcome {
                    name: plan.name.clone(),
                    version: plan.version.clone(),
                    backend: "custom".into(),
                    cellar_path: cellar,
                    linked_bins: linked,
                })
            }
            ArtifactKind::Binary => {
                config::ensure_dirs(&opts.prefix)?;
                let cellar = cellar_for(&opts.prefix, &plan.name, &plan.version);
                std::fs::create_dir_all(&cellar)?;
                let binary_path = cellar.join(&plan.name);
                std::fs::write(&binary_path, &bytes)?;
                #[cfg(not(windows))]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))?;
                }
                let linked = vec![install::link_single_binary(
                    &binary_path,
                    &plan.name,
                    &opts.prefix,
                )?];
                state::upsert_record(state::record_with_kind(
                    &plan.name,
                    &plan.version,
                    "custom",
                    cellar.clone(),
                    opts.kind,
                ))?;
                Ok(InstallOutcome {
                    name: plan.name.clone(),
                    version: plan.version.clone(),
                    backend: "custom".into(),
                    cellar_path: cellar,
                    linked_bins: linked,
                })
            }
        }
    }
}

/// Mirrors supply download URLs and build steps, so they must not be
/// fetched over a tamperable transport. Loopback http is allowed for local
/// mirrors and tests.
pub fn mirror_url_allowed(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://")
        || ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|p| {
                lower
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
            })
}

pub fn cellar_for(prefix: &Path, name: &str, version: &str) -> PathBuf {
    config::cellar_dir(prefix).join(name).join(version)
}

fn link_all_bins(cellar: &Path, prefix: &Path) -> Result<Vec<PathBuf>> {
    // An empty list makes link_bins discover everything in bin/ and sbin/,
    // symlinked executables included.
    install::link_bins(cellar, &[], prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with(keys: &[&str]) -> Entry {
        let mut artifacts = BTreeMap::new();
        for k in keys {
            artifacts.insert(
                k.to_string(),
                Artifact {
                    url: format!("https://example.com/{k}.tar.gz"),
                    sha256: Some("abc".into()),
                    kind: ArtifactKind::TarGz,
                },
            );
        }
        Entry {
            name: "x".into(),
            version: "1.0.0".into(),
            description: String::new(),
            homepage: String::new(),
            artifacts,
            build_steps: vec![],
        }
    }

    #[test]
    fn registry_roundtrip() {
        let mut reg = Registry::default();
        reg.upsert(entry_with(&["macos-arm64"]));
        let s = serde_json::to_string_pretty(&reg).unwrap();
        let back: Registry = serde_json::from_str(&s).unwrap();
        assert_eq!(back.packages.len(), 1);
        assert_eq!(back.find("x").unwrap().version, "1.0.0");
    }

    #[test]
    fn search_filters_by_query() {
        let mut reg = Registry::default();
        reg.upsert(Entry {
            name: "hello".into(),
            version: "1.0".into(),
            description: "greeter".into(),
            homepage: String::new(),
            artifacts: BTreeMap::new(),
            build_steps: vec![],
        });
        let backend = CustomBackend { registry: reg };
        assert_eq!(backend.search("hello", 10).unwrap().len(), 1);
        assert_eq!(backend.search("zzz", 10).unwrap().len(), 0);
    }
}

#[cfg(test)]
mod mirror_tests {
    use super::*;

    #[test]
    fn mirrors_must_be_https_or_loopback() {
        for ok in [
            "https://example.com/reg.json",
            "HTTPS://example.com/reg.json",
            "http://127.0.0.1:8080/reg.json",
            "http://localhost/reg.json",
            "http://[::1]:9/reg.json",
        ] {
            assert!(mirror_url_allowed(ok), "{ok} should be allowed");
        }
        for bad in [
            "http://example.com/reg.json",
            "http://127.0.0.1.evil.com/reg.json",
            "http://localhost.evil.com/",
            "ftp://example.com/reg.json",
            "file:///etc/passwd",
        ] {
            assert!(!mirror_url_allowed(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn plan_refuses_traversing_name_or_version() {
        let art = Artifact {
            url: "https://example.com/x.tar.gz".into(),
            sha256: None,
            kind: ArtifactKind::TarGz,
        };
        let mut reg = Registry::default();
        for (name, version) in [("../evil", "1.0"), ("ok", "../../x"), ("a/b", "1.0")] {
            let mut artifacts = BTreeMap::new();
            for key in ["macos", "linux", "windows"] {
                artifacts.insert(key.to_string(), art.clone());
            }
            reg.upsert(Entry {
                name: name.into(),
                version: version.into(),
                description: String::new(),
                homepage: String::new(),
                artifacts,
                build_steps: vec![],
            });
        }
        let backend = CustomBackend::new(reg);
        for name in ["../evil", "ok", "a/b"] {
            assert!(
                matches!(backend.plan(name), Err(Error::Unsupported(_))),
                "plan({name}) should be refused"
            );
        }
    }
}
