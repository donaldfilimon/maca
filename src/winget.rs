//! winget catalog + installer backend via native HTTP APIs.
//!
//! Catalog: `https://api.winget.run/v2/packages` (search + versions + metadata).
//! Installers: winget-pkgs manifests on GitHub
//! (`https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/.../*.installer.yaml`)
//! parsed for `InstallerUrl` + `InstallerSha256` per architecture.
//! No `winget.exe` subprocess is required for search; actual installer
//! execution is Windows-only and gated behind `cfg(windows)`.

use serde::Deserialize;

use crate::backend::{InstallOptions, InstallOutcome, InstallPlan, Package};
use crate::install;
#[cfg(target_os = "windows")]
use crate::state;
use crate::{Error, Result};

const WINGET_API: &str = "https://api.winget.run/v2/packages";
const WINGET_PKGS_RAW: &str =
    "https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests";

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // total reserved for pagination
struct ApiEnvelope {
    #[serde(rename = "Packages")]
    packages: Vec<ApiPackage>,
    #[serde(rename = "Total", default)]
    total: Option<usize>,
}

/// winget.run emits `null` for some optional strings; accept and blank them.
fn de_opt_string<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

fn de_string_default<'de, D>(d: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_string(d)?.unwrap_or_default())
}

/// Accept `null` elements inside the versions array.
fn de_versions<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?;
    Ok(v.unwrap_or_default()
        .into_iter()
        .filter_map(|x| match x {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Null => None,
            other => Some(other.to_string()),
        })
        .collect())
}

#[derive(Debug, Deserialize, Clone)]
struct ApiPackage {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Versions", default, deserialize_with = "de_versions")]
    versions: Vec<String>,
    #[serde(rename = "Latest", default)]
    latest: Option<Latest>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)] // name/license reserved for richer info output
struct Latest {
    #[serde(rename = "Name", default, deserialize_with = "de_string_default")]
    name: String,
    #[serde(rename = "Publisher", default, deserialize_with = "de_string_default")]
    publisher: String,
    #[serde(
        rename = "Description",
        default,
        deserialize_with = "de_string_default"
    )]
    description: String,
    #[serde(rename = "Homepage", default, deserialize_with = "de_string_default")]
    homepage: String,
    #[serde(rename = "Tags", default)]
    tags: Vec<String>,
    #[serde(rename = "License", default, deserialize_with = "de_string_default")]
    license: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // manifest fields reserved for installer execution
struct InstallerManifest {
    #[serde(rename = "PackageIdentifier")]
    package_id: String,
    #[serde(rename = "PackageVersion")]
    package_version: String,
    #[serde(default)]
    #[serde(rename = "Installers")]
    installers: Vec<Installer>,
    #[serde(default, rename = "InstallerType")]
    installer_type: Option<String>,
    #[serde(default, rename = "Commands")]
    commands: Option<Vec<String>>,
    #[serde(default, rename = "InstallerSwitches")]
    switches: Option<InstallerSwitches>,
}

#[derive(Debug, Deserialize)]
pub struct Installer {
    #[serde(rename = "Architecture")]
    architecture: String,
    #[serde(rename = "InstallerUrl")]
    url: String,
    #[serde(rename = "InstallerSha256")]
    sha256: String,
    #[serde(default, rename = "Scope")]
    scope: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)] // silent-switch fields reserved for installer execution
struct InstallerSwitches {
    #[serde(default, rename = "Silent")]
    silent: Option<String>,
    #[serde(default, rename = "SilentWithProgress")]
    silent_with_progress: Option<String>,
    #[serde(default, rename = "Custom")]
    custom: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)] // fields reserved for richer locale output
struct LocaleManifest {
    #[serde(rename = "PackageName", default)]
    package_name: Option<String>,
    #[serde(rename = "Publisher", default)]
    publisher: Option<String>,
    #[serde(rename = "ShortDescription", default)]
    description: Option<String>,
    #[serde(rename = "PackageUrl", default)]
    homepage: Option<String>,
    #[serde(rename = "License", default)]
    license: Option<String>,
    #[serde(rename = "Tags", default)]
    tags: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct WingetBackend;

impl WingetBackend {
    pub fn new() -> Self {
        Self
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Package>> {
        let q = query.trim();
        let url = if q.is_empty() {
            WINGET_API.to_string()
        } else {
            format!("{WINGET_API}?query={}", urlencoding_lite(q))
        };
        let data = install::download(&url).await?;
        let env: ApiEnvelope = serde_json::from_slice(&data)
            .map_err(|e| Error::Network(format!("decoding winget search: {e}")))?;
        let mut pkgs: Vec<Package> = env
            .packages
            .into_iter()
            .take(limit.max(1))
            .map(|p| {
                let latest = p.latest.clone().unwrap_or(Latest {
                    name: p.id.clone(),
                    publisher: String::new(),
                    description: String::new(),
                    homepage: String::new(),
                    tags: vec![],
                    license: String::new(),
                });
                let version = p
                    .versions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".into());
                Package {
                    name: p.id.clone(),
                    version,
                    description: format!("{} — {}", latest.description, latest.publisher),
                    homepage: latest.homepage,
                    backend: "winget".into(),
                    tags: latest.tags,
                }
            })
            .collect();
        pkgs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(pkgs)
    }

    /// Resolve an exact `Publisher.Name` id to its latest published version
    /// using the winget-pkgs repository layout (source of truth for ids).
    /// Returns `(id, version)`.
    pub async fn resolve_exact(id: &str) -> Result<(String, String)> {
        let url = Self::versions_url(id).ok_or_else(|| {
            Error::Unsupported(format!("winget id '{id}' is not in Publisher.Name form"))
        })?;
        let bytes = install::download(&url).await.map_err(|e| match e {
            Error::Network(m) if m.contains("404") => {
                Error::NotFound(format!("winget package '{id}' not found in winget-pkgs"))
            }
            other => other,
        })?;
        #[derive(Deserialize)]
        struct Entry {
            name: String,
            #[serde(rename = "type")]
            kind: String,
        }
        let entries: Vec<Entry> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Network(format!("decoding winget-pkgs listing: {e}")))?;
        let mut versions: Vec<String> = entries
            .into_iter()
            .filter(|e| e.kind == "dir")
            .map(|e| e.name)
            .filter(|v| semver::Version::parse(v).is_ok())
            .collect();
        versions.sort_by(|a, b| {
            semver::Version::parse(b)
                .unwrap()
                .cmp(&semver::Version::parse(a).unwrap())
        });
        let version = versions.into_iter().next().ok_or_else(|| {
            Error::NotFound(format!("no published versions for winget id '{id}'"))
        })?;
        Ok((id.to_string(), version))
    }

    /// winget-pkgs versions listing URL.
    pub fn versions_url(id: &str) -> Option<String> {
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let first = parts[0].chars().next()?.to_ascii_lowercase();
        Some(format!(
            "https://api.github.com/repos/microsoft/winget-pkgs/contents/manifests/{first}/{}/{}",
            parts[0], parts[1]
        ))
    }

    /// winget-pkgs locale manifest URL (description/homepage/publisher).
    pub fn locale_url(id: &str, version: &str) -> Option<String> {
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let first = parts[0].chars().next()?.to_ascii_lowercase();
        Some(format!(
            "{WINGET_PKGS_RAW}/{first}/{}/{}/{}/{}.locale.en-US.yaml",
            parts[0], parts[1], version, id
        ))
    }

    pub async fn info(&self, name: &str) -> Result<Package> {
        let id = name.trim();
        if id.is_empty() {
            return Err(Error::NotFound("empty winget id".into()));
        }
        // Prefer an exact match from the search API (rich metadata).
        let search_url = format!("{}/{}", WINGET_API, urlencoding_lite(id));
        if let Ok(data) = install::download(&search_url).await
            && let Ok(env) = serde_json::from_slice::<ApiEnvelope>(&data)
            && let Some(exact) = env
                .packages
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(id))
                .cloned()
        {
            let latest = exact.latest.unwrap_or(Latest {
                name: exact.id.clone(),
                publisher: String::new(),
                description: String::new(),
                homepage: String::new(),
                tags: vec![],
                license: String::new(),
            });
            return Ok(Package {
                name: exact.id,
                version: exact
                    .versions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                description: latest.description,
                homepage: latest.homepage,
                backend: "winget".into(),
                tags: latest.tags,
            });
        }
        // Fall back to winget-pkgs + locale manifest.
        let (resolved_id, version) = Self::resolve_exact(id).await?;
        let mut description = String::new();
        let mut homepage = String::new();
        if let Some(url) = Self::locale_url(&resolved_id, &version)
            && let Ok(bytes) = install::download(&url).await
            && let Ok(locale) = serde_yaml::from_slice::<LocaleManifest>(&bytes)
        {
            description = locale.description.unwrap_or_default();
            homepage = locale.homepage.unwrap_or_default();
        }
        Ok(Package {
            name: resolved_id,
            version,
            description,
            homepage,
            backend: "winget".into(),
            tags: vec![],
        })
    }

    /// Build the winget-pkgs raw manifest URL for an id+version.
    /// winget-pkgs layout: `manifests/<first-char-lower>/<Publisher>/<Name>/<version>/<Id>.installer.yaml`
    pub fn manifest_url(id: &str, version: &str) -> Option<String> {
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() != 2 {
            return None;
        }
        let first = parts[0].chars().next()?.to_ascii_lowercase();
        Some(format!(
            "{WINGET_PKGS_RAW}/{first}/{}/{}/{}/{}.installer.yaml",
            parts[0], parts[1], version, id
        ))
    }

    pub async fn plan(&self, name: &str) -> Result<InstallPlan> {
        self.plan_inner(name).await
    }

    async fn plan_inner(&self, name: &str) -> Result<InstallPlan> {
        // Resolve the exact id + latest version from winget-pkgs (source of truth).
        let (id, version) = Self::resolve_exact(name).await?;
        let url = Self::manifest_url(&id, &version).ok_or_else(|| {
            Error::Unsupported(format!("winget id '{id}' is not in Publisher.Name form"))
        })?;
        let yaml_bytes = install::download(&url).await.map_err(|e| match e {
            Error::Network(m) if m.contains("404") => Error::NotFound(format!(
                "winget manifest for {id} {version} not found at {url}"
            )),
            other => other,
        })?;
        let manifest: InstallerManifest = serde_yaml::from_slice(&yaml_bytes)
            .map_err(|e| Error::Network(format!("decoding winget manifest: {e}")))?;
        let host = host_arch();
        let installer = pick_installer(&manifest.installers, host).ok_or_else(|| {
            Error::Unsupported(format!(
                "no winget installer for {id} {version} on arch {host}"
            ))
        })?;
        Ok(InstallPlan {
            name: id,
            version,
            backend: "winget".into(),
            url: installer.url.clone(),
            sha256: Some(installer.sha256.clone()),
            note: Some(format!(
                "installer {:?}, arch {}",
                manifest.installer_type.clone().unwrap_or_default(),
                installer.architecture
            )),
        })
    }

    pub async fn install(&self, name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
        let plan = self.plan_inner(name).await?;
        if opts.dry_run {
            println!(
                "[dry-run] would install {} {} via {} ({})",
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
        // Installer execution is Windows-only. On macOS/Linux the catalog
        // works but installs are gated.
        #[cfg(not(target_os = "windows"))]
        {
            Err(Error::Unsupported(format!(
                "winget install of '{}' requires Windows (winget installers are MSI/EXE). \
                 Run on Windows or use --dry-run to preview the manifest.",
                plan.name
            )))
        }
        #[cfg(target_os = "windows")]
        {
            self.install_windows(&plan, opts).await
        }
    }

    #[cfg(target_os = "windows")]
    async fn install_windows(
        &self,
        plan: &InstallPlan,
        opts: &InstallOptions,
    ) -> Result<InstallOutcome> {
        let bytes = install::download(&plan.url).await?;
        install::verify_sha256(&plan.name, &bytes, plan.sha256.as_deref())?;
        // Record the verified installer in the cellar and register in ledger.
        // Execution policy (msiexec vs direct EXE) is determined by extension.
        crate::config::ensure_dirs(&opts.prefix)?;
        let cellar = crate::config::cellar_dir(&opts.prefix)
            .join(&plan.name)
            .join(&plan.version);
        std::fs::create_dir_all(&cellar)?;
        let ext = std::path::Path::new(&plan.url)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let installer_path = cellar.join(format!("installer.{ext}"));
        std::fs::write(&installer_path, &bytes)?;
        // Best-effort silent install; winget installer types include
        // inno/nsis/msi. This keeps maca honest without reimplementing
        // the full winget engine.
        let status = if ext == "msi" {
            std::process::Command::new("msiexec")
                .args([
                    "/i",
                    &installer_path.to_string_lossy(),
                    "/quiet",
                    "/norestart",
                ])
                .status()
        } else {
            std::process::Command::new(&installer_path)
                .args(["/S"])
                .status()
        };
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                return Err(Error::Other(anyhow::anyhow!(
                    "installer for {} exited with {s}",
                    plan.name
                )));
            }
            Err(e) => {
                return Err(Error::Other(anyhow::anyhow!(
                    "launching installer for {}: {e}",
                    plan.name
                )));
            }
        }
        state::upsert_record(state::record_with_kind(
            &plan.name,
            &plan.version,
            "winget",
            cellar.clone(),
            opts.kind,
        ))?;
        Ok(InstallOutcome {
            name: plan.name.clone(),
            version: plan.version.clone(),
            backend: "winget".into(),
            cellar_path: cellar,
            linked_bins: vec![],
        })
    }
}

pub fn pick_installer<'a>(installers: &'a [Installer], host_arch: &str) -> Option<&'a Installer> {
    // Prefer user scope, then machine.
    let mut candidates: Vec<&Installer> = installers
        .iter()
        .filter(|i| i.architecture.eq_ignore_ascii_case(host_arch))
        .collect();
    if candidates.is_empty() {
        // neutral/x64 fallback: try x64 when host is arm64 (emulation).
        if host_arch == "arm64" {
            candidates = installers
                .iter()
                .filter(|i| i.architecture.eq_ignore_ascii_case("x64"))
                .collect();
        }
        if candidates.is_empty() {
            return None;
        }
    }
    candidates.sort_by_key(|i| match i.scope.as_deref() {
        Some("user") => 0,
        Some("machine") => 1,
        _ => 2,
    });
    candidates.into_iter().next()
}

fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "neutral"
    }
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_url_shape() {
        let u = WingetBackend::manifest_url("Microsoft.VisualStudioCode", "1.99.0").unwrap();
        assert_eq!(
            u,
            "https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/m/Microsoft/VisualStudioCode/1.99.0/Microsoft.VisualStudioCode.installer.yaml"
        );
    }

    #[test]
    fn manifest_url_rejects_bad_id() {
        assert!(WingetBackend::manifest_url("NoDots", "1.0").is_none());
    }

    #[test]
    fn pick_prefers_user_scope() {
        let installers = vec![
            Installer {
                architecture: "x64".into(),
                url: "https://example.com/machine.exe".into(),
                sha256: "aa".into(),
                scope: Some("machine".into()),
            },
            Installer {
                architecture: "x64".into(),
                url: "https://example.com/user.exe".into(),
                sha256: "bb".into(),
                scope: Some("user".into()),
            },
        ];
        let got = pick_installer(&installers, "x64").unwrap();
        assert_eq!(got.url, "https://example.com/user.exe");
    }

    #[test]
    fn pick_arm64_falls_back_to_x64() {
        let installers = vec![Installer {
            architecture: "x64".into(),
            url: "https://example.com/x64.exe".into(),
            sha256: "aa".into(),
            scope: None,
        }];
        assert!(pick_installer(&installers, "arm64").is_some());
    }
}
