//! Homebrew catalog + bottle backend via native JSON APIs.
//!
//! Catalog: `https://formulae.brew.sh/api/formula/<name>.json` (per-formula)
//! and `https://formulae.brew.sh/api/formula.json` (full index, for search).
//! Installs: OCI blob download from GHCR with an anonymous bearer token
//! (`https://ghcr.io/token?scope=repository:homebrew/core/<name>:pull`),
//! SHA-256 verified, extracted to the maca Cellar. No `brew` binary needed.

use std::collections::HashMap;

use serde::Deserialize;

use crate::backend::{InstallOptions, InstallOutcome, InstallPlan, Package};
use crate::install;
use crate::mach_o;
use crate::state;
use crate::{Error, Result};

const FORMULA_API: &str = "https://formulae.brew.sh/api/formula";
const FORMULA_INDEX: &str = "https://formulae.brew.sh/api/formula.json";
const GHCR_TOKEN: &str = "https://ghcr.io/token";

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // disabled reserved for info/search filtering
struct Formula {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    homepage: String,
    versions: Versions,
    #[serde(default)]
    executables: Vec<String>,
    #[serde(default)]
    bottle: Option<Bottle>,
    /// Runtime dependencies from the formula metadata.
    #[serde(default)]
    dependencies: Vec<String>,
    /// Dependencies satisfied by the OS rather than by Homebrew.
    #[serde(default)]
    uses_from_macos: Vec<serde_json::Value>,
    #[serde(default)]
    disabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Versions {
    stable: String,
}

#[derive(Debug, Deserialize)]
struct Bottle {
    stable: Option<BottleStable>,
}

#[derive(Debug, Deserialize)]
struct BottleStable {
    #[serde(default)]
    files: HashMap<String, BottleFile>,
}

#[derive(Debug, Deserialize)]
struct BottleFile {
    url: String,
    sha256: String,
}

#[derive(Debug, Default)]
pub struct BrewBackend;

impl BrewBackend {
    pub fn new() -> Self {
        Self
    }

    /// Fetch per-formula metadata. Name is lowercased; `homebrew/core` only.
    async fn fetch_formula(&self, name: &str) -> Result<Formula> {
        let normalized = name.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(Error::NotFound("empty formula name".into()));
        }
        let url = format!("{FORMULA_API}/{normalized}.json");
        let data = install::download(&url).await.map_err(|e| match e {
            Error::Network(m) if m.contains("404") => {
                Error::NotFound(format!("formula '{normalized}' not found"))
            }
            other => other,
        })?;
        serde_json::from_slice(&data)
            .map_err(|e| Error::Network(format!("decoding formula {normalized}: {e}")))
    }

    /// Search the full index (31 MiB). Filters by name/desc substring.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Package>> {
        let q = query.trim().to_ascii_lowercase();
        let data = install::download(FORMULA_INDEX).await?;
        let all: Vec<FormulaSummary> = serde_json::from_slice(&data)
            .map_err(|e| Error::Network(format!("decoding formula index: {e}")))?;
        let mut hits: Vec<Package> = all
            .into_iter()
            .filter(|f| !f.disabled.unwrap_or(false))
            .filter(|f| {
                q.is_empty()
                    || f.name.to_ascii_lowercase().contains(&q)
                    || f.desc
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&q)
            })
            .take(limit.max(1))
            .map(|f| Package {
                name: f.name.clone(),
                version: f.stable(),
                description: f.desc.unwrap_or_default(),
                homepage: f.homepage,
                backend: "brew".into(),
                tags: vec!["homebrew/core".into()],
            })
            .collect();
        hits.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(hits)
    }

    pub async fn info(&self, name: &str) -> Result<Package> {
        let f = self.fetch_formula(name).await?;
        Ok(Package {
            name: f.name.clone(),
            version: f.versions.stable,
            description: f.desc.unwrap_or_default(),
            homepage: f.homepage,
            backend: "brew".into(),
            tags: vec!["homebrew/core".into()],
        })
    }

    /// Pick the bottle file for the current host.
    /// Pure function (no IO) so it is unit-testable.
    pub fn pick_bottle_key(
        files: &BTreeSet<String>,
        os: &str,
        arch: &str,
        macos_codename: Option<&str>,
    ) -> Option<String> {
        let mut prefs: Vec<String> = Vec::new();
        match (os, arch) {
            ("macos", "arm64") => {
                if let Some(c) = macos_codename {
                    prefs.push(format!("arm64_{c}"));
                }
                for c in ["tahoe", "sequoia", "sonoma", "ventura", "monterey"] {
                    prefs.push(format!("arm64_{c}"));
                }
            }
            ("macos", "x86_64") => {
                if let Some(c) = macos_codename {
                    prefs.push(c.to_string());
                }
                for c in ["tahoe", "sequoia", "sonoma", "ventura", "monterey"] {
                    prefs.push(c.to_string());
                }
            }
            ("linux", "x86_64") => prefs.push("x86_64_linux".into()),
            ("linux", "arm64") => prefs.push("arm64_linux".into()),
            _ => {}
        }
        if let Some(key) = prefs.iter().find(|k| files.contains(k.as_str())) {
            return Some(key.clone());
        }
        // Fallback: any key matching arch+os suffix.
        files
            .iter()
            .find(|k| match (os, arch) {
                ("macos", "arm64") => k.starts_with("arm64_") && !k.ends_with("_linux"),
                ("macos", "x86_64") => !k.contains('_') && !k.ends_with("_linux"),
                ("linux", "x86_64") => k.ends_with("x86_64_linux") || k.as_str() == "x86_64_linux",
                ("linux", "arm64") => k.ends_with("arm64_linux"),
                _ => false,
            })
            .cloned()
    }

    pub async fn plan(&self, name: &str) -> Result<InstallPlan> {
        let (plan, _executables, _sha) = self.plan_inner(name).await?;
        Ok(plan)
    }

    async fn plan_inner(&self, name: &str) -> Result<(InstallPlan, Vec<String>, String)> {
        let f = self.fetch_formula(name).await?;
        let bottle = f
            .bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "formula '{}' has no bottle (source-only); maca cannot build from source yet",
                    f.name
                ))
            })?;
        let host = host_platform();
        let keys: std::collections::BTreeSet<String> = bottle.files.keys().cloned().collect();
        let key = Self::pick_bottle_key(&keys, host.os, host.arch, host.codename.as_deref())
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "no bottle for {} on {} (available: {})",
                    f.name,
                    host.label,
                    keys.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        let file = &bottle.files[&key];
        let plan = InstallPlan {
            name: f.name.clone(),
            version: f.versions.stable.clone(),
            backend: "brew".into(),
            url: file.url.clone(),
            sha256: Some(file.sha256.clone()),
            note: Some(format!("bottle key `{key}`")),
        };
        Ok((plan, f.executables.clone(), file.sha256.clone()))
    }

    pub async fn install(&self, name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
        let mut visited = Vec::new();
        self.install_with_deps(name, opts, &mut visited).await
    }

    /// Install `name` plus any Homebrew dependencies that are not already
    /// present in the ledger. `visited` guards against dependency cycles.
    async fn install_with_deps(
        &self,
        name: &str,
        opts: &InstallOptions,
        visited: &mut Vec<String>,
    ) -> Result<InstallOutcome> {
        let normalized = name.trim().to_ascii_lowercase();
        if visited.iter().any(|v| v == &normalized) {
            // Already handled in this run; return a placeholder outcome.
            return Ok(InstallOutcome {
                name: normalized.clone(),
                version: String::new(),
                backend: "brew".into(),
                cellar_path: crate::config::cellar_dir(&opts.prefix).join(&normalized),
                linked_bins: vec![],
            });
        }
        visited.push(normalized.clone());

        // Install dependencies first so opt/ symlinks and dylibs exist.
        let formula = self.fetch_formula(&normalized).await?;
        let ledger = state::Ledger::load()?;
        for dep in &formula.dependencies {
            if formula.uses_from_macos.iter().any(|u| {
                u.as_str()
                    .map(|s| s.eq_ignore_ascii_case(dep))
                    .unwrap_or(false)
            }) {
                // Provided by the OS; not a maca-managed dependency.
                continue;
            }
            if ledger.get(dep).is_some() {
                continue;
            }
            tracing::info!("installing dependency {dep} for {normalized}");
            Box::pin(self.install_with_deps(dep, opts, visited)).await?;
        }

        let (plan, executables, expected_sha) = self.plan_inner(&normalized).await?;
        if opts.dry_run {
            println!(
                "[dry-run] would install {} {} via {} (bottle {})",
                plan.name, plan.version, plan.backend, plan.url
            );
            return Ok(InstallOutcome {
                name: plan.name.clone(),
                version: plan.version.clone(),
                backend: plan.backend.clone(),
                cellar_path: crate::config::cellar_dir(&opts.prefix)
                    .join(&plan.name)
                    .join(&plan.version),
                linked_bins: vec![],
            });
        }
        // Anonymous GHCR token for OCI blob download.
        let token = ghcr_anonymous_token(&plan.name).await?;
        let bytes = install::download_with_bearer(&plan.url, Some(&token)).await?;
        install::verify_sha256(&plan.name, &bytes, Some(&expected_sha))?;
        let cellar = install::extract_bottle(&plan.name, &plan.version, &bytes, &opts.prefix)?;
        // Homebrew exposes each keg at `<prefix>/opt/<name>`; bottles reference
        // deps through that path, so create the same symlink.
        install::link_opt(&plan.name, &plan.version, &cellar, &opts.prefix)?;
        // Substitute @@HOMEBREW_CELLAR@@ / @@HOMEBREW_PREFIX@@ in Mach-O files.
        let rewritten = mach_o::rewrite_placeholders(&cellar, &opts.prefix)?;
        if rewritten > 0 {
            tracing::info!("rewrote Homebrew placeholders in {rewritten} Mach-O files");
        }
        let linked = install::link_bins(&cellar, &executables, &opts.prefix)?;
        let mut ledger = state::Ledger::load()?;
        ledger.upsert(state::record(
            &plan.name,
            &plan.version,
            "brew",
            cellar.clone(),
        ));
        ledger.save()?;
        Ok(InstallOutcome {
            name: plan.name,
            version: plan.version,
            backend: "brew".into(),
            cellar_path: cellar,
            linked_bins: linked,
        })
    }
}

#[derive(Debug, Deserialize)]
struct FormulaSummary {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    versions: Option<Versions>,
    #[serde(default)]
    disabled: Option<bool>,
}

impl FormulaSummary {
    fn stable(&self) -> String {
        self.versions
            .as_ref()
            .map(|v| v.stable.clone())
            .unwrap_or_else(|| "unknown".into())
    }
}

pub struct HostPlatform {
    pub os: &'static str,
    pub arch: &'static str,
    pub codename: Option<String>,
    pub label: String,
}

pub fn host_platform() -> HostPlatform {
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
        "x86_64"
    } else {
        "unknown"
    };
    let codename = macos_codename();
    let label = format!("{os}-{arch}");
    HostPlatform {
        os,
        arch,
        codename,
        label,
    }
}

fn macos_codename() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        // Map `sw_vers` product version major to the Homebrew bottle tag.
        // Bootstrapped without shelling out: use `std::process` only as a
        // fallback; prefer compile-time probing via `uname` is avoided.
        // We query `sw_vers -productVersion` (stable across macOS).
        let out = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let ver = String::from_utf8(out.stdout).ok()?;
        let major: u32 = ver.split('.').next()?.trim().parse().ok()?;
        let codename = match major {
            26 => "tahoe",
            15 => "sequoia",
            14 => "sonoma",
            13 => "ventura",
            12 => "monterey",
            11 => "big_sur",
            10 => "catalina",
            _ => "sequoia",
        };
        Some(codename.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Fetch an anonymous GHCR JWT for `homebrew/core/<name>`.
async fn ghcr_anonymous_token(formula_name: &str) -> Result<String> {
    let scope = format!("repository:homebrew/core/{formula_name}:pull");
    let url = format!("{GHCR_TOKEN}?service=ghcr.io&scope={scope}");
    let bytes = install::download(&url).await?;
    #[derive(Deserialize)]
    struct TokenResp {
        token: String,
    }
    let resp: TokenResp = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("decoding GHCR token: {e}")))?;
    Ok(resp.token)
}

use std::collections::BTreeSet;

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pick_arm64_macos_prefers_codename() {
        let files = keys(&["arm64_sonoma", "arm64_sequoia", "x86_64_linux"]);
        let got = BrewBackend::pick_bottle_key(&files, "macos", "arm64", Some("sequoia"));
        assert_eq!(got.as_deref(), Some("arm64_sequoia"));
    }

    #[test]
    fn pick_x86_linux() {
        let files = keys(&["arm64_linux", "x86_64_linux", "arm64_sequoia"]);
        let got = BrewBackend::pick_bottle_key(&files, "linux", "x86_64", None);
        assert_eq!(got.as_deref(), Some("x86_64_linux"));
    }

    #[test]
    fn pick_macos_x86_skips_linux_and_arm64() {
        let files = keys(&["sonoma", "arm64_sequoia", "x86_64_linux"]);
        let got = BrewBackend::pick_bottle_key(&files, "macos", "x86_64", None);
        assert_eq!(got.as_deref(), Some("sonoma"));
    }

    #[test]
    fn pick_returns_none_when_no_match() {
        let files = keys(&["arm64_linux"]);
        let got = BrewBackend::pick_bottle_key(&files, "macos", "arm64", None);
        // arm64_linux should NOT match macOS.
        assert_eq!(got, None);
    }
}
