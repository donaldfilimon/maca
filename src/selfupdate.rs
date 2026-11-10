//! Self-update: replace the running `maca` binary from a GitHub release.
//!
//! Release asset convention (documented, not yet published):
//! ```text
//! maca-<target>.tar.gz      (contains the `maca` binary)
//! maca-<target>.tar.gz.sha256
//! ```
//! where `<target>` is a Rust target triple such as `aarch64-apple-darwin`.
//! The asset name carries no version; the release tag does.
//!
//! The current binary is backed up next to itself as `maca.bak.<timestamp>`
//! before replacement, so a bad update can be reverted manually.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::install;
use crate::{Error, Result};

const DEFAULT_REPO: &str = "donaldfilimon/maca";

#[derive(Debug, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    #[allow(dead_code)] // size reserved for progress reporting
    pub size: Option<u64>,
}

pub fn target_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// Pick the archive asset for this host, plus its companion checksum asset.
pub fn pick_asset(assets: &[Asset]) -> Option<(Asset, Option<Asset>)> {
    let triple = target_triple();
    let suffix = if cfg!(target_os = "windows") {
        format!("{triple}.zip")
    } else {
        format!("{triple}.tar.gz")
    };
    let base = format!("maca-{suffix}");
    let archive = assets
        .iter()
        .find(|a| a.name == base || a.name == format!("maca-{suffix}"))?
        .clone();
    let sha = assets
        .iter()
        .find(|a| a.name == format!("{base}.sha256"))
        .cloned();
    Some((archive, sha))
}

pub fn repo_from_env(cfg: &crate::config::Config, env_value: Option<&str>) -> String {
    env_value
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg.update_repo.clone())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

/// Live resolution: environment first, then config, then default.
pub fn repo_from_config(cfg: &crate::config::Config) -> String {
    let env = std::env::var("MACA_UPDATE_REPO").ok();
    repo_from_env(cfg, env.as_deref())
}

/// Fetch the latest release metadata. `Ok(None)` when the repo has no releases.
pub async fn fetch_latest(repo: &str) -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let bytes = match install::download(&url).await {
        Ok(b) => b,
        Err(crate::Error::Network(m)) if m.contains("404") => return Ok(None),
        Err(e) => return Err(e),
    };
    let release = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("decoding release for {repo}: {e}")))?;
    Ok(Some(release))
}

/// Compare a release tag against the current version. `true` if newer.
pub fn is_newer(tag: &str, current: &str) -> bool {
    crate::backend::is_newer_version(tag, current)
}

/// Download and install the latest binary. Returns the new version on success.
pub async fn run(repo: &str, current_version: &str, force: bool) -> Result<String> {
    let Some(release) = fetch_latest(repo).await? else {
        return Ok("no releases published for this repository".to_string());
    };
    let tag = release.tag_name.clone();
    if tag.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "no releases published for {repo}"
        )));
    }
    if !force && !is_newer(&tag, current_version) {
        return Ok(format!("already up to date ({current_version})"));
    }
    let (asset, sha_asset) = pick_asset(&release.assets).ok_or_else(|| {
        Error::Unsupported(format!(
            "release {tag} has no asset for target {}",
            target_triple()
        ))
    })?;
    let archive = install::download(&asset.browser_download_url).await?;
    // Verify against the companion .sha256 asset when present.
    if let Some(sha_asset) = sha_asset {
        let text =
            String::from_utf8_lossy(&install::download(&sha_asset.browser_download_url).await?)
                .to_string();
        let expected = text.split_whitespace().next().unwrap_or("").to_string();
        install::verify_sha256(&asset.name, &archive, Some(&expected))?;
    } else if force {
        eprintln!(
            "maca: warning: release {tag} has no .sha256 for {}; installing UNVERIFIED (--force)",
            asset.name
        );
    } else {
        return Err(Error::Unsupported(format!(
            "release {tag} has no .sha256 for {}; refusing to replace the running binary with unverified bytes (use --force to override)",
            asset.name
        )));
    }
    let exe = current_exe_path()?;
    let tmp = extract_binary(&archive, &exe)?;
    if let Err(e) = replace_with_backup(&exe, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tag)
}

/// Extract the `maca` binary from a `.tar.gz` or `.zip` archive into a temp path.
fn extract_binary(archive: &[u8], exe: &Path) -> Result<PathBuf> {
    // Staged next to the running binary so the final rename never crosses a
    // filesystem boundary.
    let tmp = exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".maca-update-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(target_os = "windows")]
    {
        // Windows releases would be .zip, which the current deps cannot
        // unpack, so self-update is not available there.
        let _ = (archive, exe, tmp);
        Err(Error::Unsupported(
            "self-update is not supported on Windows yet (no .zip extraction)".into(),
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let gz = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(gz);
        let mut found = false;
        for entry in tar
            .entries()
            .map_err(|e| Error::Other(anyhow::anyhow!(e)))?
        {
            let mut entry = entry.map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            let path = entry
                .path()
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?
                .to_path_buf();
            // Accept the binary at the archive root or inside a single top dir.
            if path.file_name().and_then(|s| s.to_str()) != Some("maca") {
                continue;
            }
            entry
                .unpack(&tmp)
                .map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            found = true;
            break;
        }
        if !found {
            return Err(Error::Other(anyhow::anyhow!(
                "update archive did not contain a `maca` binary"
            )));
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        let _ = exe;
        Ok(tmp)
    }
}

/// Move `new_binary` into `exe`, keeping the previous binary as a backup.
fn replace_with_backup(exe: &Path, new_binary: &Path) -> Result<()> {
    if !exe.exists() {
        return Err(Error::Other(anyhow::anyhow!(
            "cannot find the running maca binary at {}",
            exe.display()
        )));
    }
    let backup = exe.with_extension(format!(
        "bak.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    // Back up the current binary, then atomically replace.
    #[cfg(not(windows))]
    {
        std::fs::copy(exe, &backup)?;
        std::fs::rename(new_binary, exe)?;
    }
    #[cfg(windows)]
    {
        // Windows cannot replace a running binary; copy the backup and write
        // the new bytes in place.
        std::fs::copy(exe, &backup)?;
        std::fs::copy(new_binary, exe)?;
    }
    Ok(())
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| Error::Other(anyhow::anyhow!("current_exe: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            browser_download_url: format!("https://example.com/{name}"),
            size: None,
        }
    }

    #[test]
    fn picks_matching_target_archive() {
        let triple = target_triple();
        let base = if cfg!(target_os = "windows") {
            format!("maca-{triple}.zip")
        } else {
            format!("maca-{triple}.tar.gz")
        };
        let assets = vec![
            asset("maca-x86_64-unknown-linux-gnu.tar.gz"),
            asset(&base),
            asset(&format!("{base}.sha256")),
            asset("maca-aarch64-apple-darwin.tar.gz"),
        ];
        let (archive, sha) = pick_asset(&assets).expect("should find host asset");
        assert_eq!(archive.name, base);
        assert!(sha.is_some());
    }

    #[test]
    fn returns_none_when_host_asset_missing() {
        let assets = vec![asset("maca-other-target.tar.gz")];
        assert!(pick_asset(&assets).is_none());
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        // A tag that cannot be ordered is not an update (use --force).
        assert!(!is_newer("custom", "0.1.0"));
    }

    #[test]
    fn repo_prefers_env() {
        let cfg = crate::config::Config::default();
        assert_eq!(repo_from_env(&cfg, Some("someone/else")), "someone/else");
        assert_eq!(repo_from_env(&cfg, Some("")), DEFAULT_REPO);
        let cfg2 = crate::config::Config {
            update_repo: Some("config/repo".into()),
            ..Default::default()
        };
        assert_eq!(repo_from_env(&cfg2, None), "config/repo");
        assert_eq!(repo_from_env(&cfg2, Some("env/repo")), "env/repo");
    }
}
