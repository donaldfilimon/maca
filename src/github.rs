//! GitHub Releases as a package source.
//!
//! Install specs are `owner/repo` or `owner--repo`. The ledger and cellar
//! name is always `owner--repo`, because a package name cannot contain `/`.
//! Catalog and downloads use the GitHub HTTP API. Nothing shells out to `gh`.
//!
//! `MACA_GITHUB_API` overrides the API origin (tests point it at a loopback
//! server). `MACA_GITHUB_TOKEN` or `GITHUB_TOKEN` is sent as a bearer token
//! when set. The token is never logged.

use std::path::{Path, PathBuf};

use crate::backend::{InstallOptions, InstallOutcome, InstallPlan, Package};
use crate::install::{self, sha256_from_sums};
use crate::state;
use crate::{Error, Result, VERSION};

#[derive(Default)]
pub struct GithubBackend;

impl GithubBackend {
    pub fn new() -> Self {
        Self
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Package>> {
        let limit = limit.clamp(1, 50);
        let url = format!(
            "{}/search/repositories?q={}+in:name&per_page={limit}",
            api_base(),
            form_encode(query)
        );
        let body = github_json(&url).await?;
        Ok(packages_from_search(&body, limit))
    }

    pub async fn info(&self, name: &str) -> Result<Package> {
        let (owner, repo, cellar) = split_spec(name)?;
        let repo_url = format!("{}/repos/{owner}/{repo}", api_base());
        let body = github_json(&repo_url).await?;
        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let homepage = body
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let version = match github_json(&format!("{repo_url}/releases/latest")).await {
            Ok(release) => sanitize_version(
                release
                    .get("tag_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            )?,
            Err(_) => "unknown".to_string(),
        };
        Ok(package(
            &cellar,
            &version,
            &description,
            &homepage,
            &owner,
            &repo,
        ))
    }

    /// The latest release version, with every lookup failure propagated
    /// (unlike `info`, which tolerates it). Used by upgrade checks.
    pub async fn latest_release_version(&self, name: &str) -> Result<String> {
        let (owner, repo, _) = split_spec(name)?;
        let url = format!("{}/repos/{owner}/{repo}/releases/latest", api_base());
        let release = github_json(&url).await?;
        version_from_release(&release, &format!("{owner}/{repo}"))
    }

    pub async fn plan(&self, name: &str) -> Result<InstallPlan> {
        Ok(self.plan_release(name, None).await?.plan)
    }

    async fn plan_release(&self, name: &str, version: Option<&str>) -> Result<PlannedRelease> {
        let (owner, repo, cellar) = split_spec(name)?;
        let body = fetch_release(&owner, &repo, version).await?;
        let (tag, assets) = assets_from_release(&body);
        plan_from_assets(
            &cellar,
            &owner,
            &repo,
            &tag,
            &assets,
            host_os(),
            host_arch(),
        )
    }

    pub async fn install(&self, name: &str, opts: &InstallOptions) -> Result<InstallOutcome> {
        if opts.build_from_source {
            return Err(Error::Unsupported(
                "github releases have no source-build recipe".into(),
            ));
        }
        let planned = self.plan_release(name, opts.version.as_deref()).await?;
        if opts.dry_run {
            println!(
                "[dry-run] would install {} {} via github ({})",
                planned.plan.name, planned.plan.version, planned.plan.url
            );
            let cellar_path = cellar_path(&opts.prefix, &planned.plan.name, &planned.plan.version);
            return Ok(InstallOutcome {
                name: planned.plan.name,
                version: planned.plan.version,
                backend: "github".into(),
                cellar_path,
                linked_bins: vec![],
            });
        }
        let bytes = install::download(&planned.plan.url).await?;
        let sha = match &planned.plan.sha256 {
            Some(sha) => Some(sha.clone()),
            None => match &planned.checksum_url {
                Some(url) => {
                    let text = String::from_utf8(install::download(url).await?).map_err(|e| {
                        Error::Other(anyhow::anyhow!("checksum file is not utf-8: {e}"))
                    })?;
                    sha256_from_sums(&text, &planned.asset_name)
                }
                None => None,
            },
        };
        if sha.is_none() {
            eprintln!(
                "maca: warning: github package {} declares no sha256; installing {} UNVERIFIED",
                planned.plan.name, planned.plan.url
            );
        }
        let keg_existed = opts
            .keg_root()
            .join(&planned.plan.name)
            .join(&planned.plan.version)
            .exists();
        let cellar = commit_artifact(
            &planned.plan.name,
            &planned.plan.version,
            &bytes,
            sha.as_deref(),
            &opts.keg_root(),
            planned.kind,
            &planned.raw_exe,
        )?;
        let linked = match planned.kind {
            ArtifactKind::Raw => {
                let exe = cellar.join("bin").join(&planned.raw_exe);
                vec![install::link_single_binary(
                    &exe,
                    &planned.raw_exe,
                    &opts.prefix,
                )?]
            }
            ArtifactKind::TarGz | ArtifactKind::Zip => {
                let mut bins = install::link_bins(&cellar, &[], &opts.prefix)?;
                if bins.is_empty() {
                    bins = install::link_root_executables(&cellar, &opts.prefix)?;
                }
                if bins.is_empty() {
                    // Only clean up a keg this call created.
                    if !keg_existed {
                        let _ = std::fs::remove_dir_all(&cellar);
                        if let Some(parent) = cellar.parent() {
                            let _ = std::fs::remove_dir(parent);
                        }
                    }
                    return Err(Error::Unsupported(format!(
                        "github archive for {} contains no executables at the root or in bin/",
                        planned.plan.name
                    )));
                }
                install::link_share(&cellar, &opts.prefix)?;
                bins
            }
        };
        state::upsert_record(state::record_with_kind(
            &planned.plan.name,
            &planned.plan.version,
            "github",
            cellar.clone(),
            opts.kind,
        ))?;
        Ok(InstallOutcome {
            name: planned.plan.name,
            version: planned.plan.version,
            backend: "github".into(),
            cellar_path: cellar,
            linked_bins: linked,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    TarGz,
    Zip,
    Raw,
}

#[derive(Debug)]
struct PlannedRelease {
    plan: InstallPlan,
    checksum_url: Option<String>,
    asset_name: String,
    kind: ArtifactKind,
    raw_exe: String,
}

#[derive(Debug, Clone)]
pub struct ListedAsset {
    pub name: String,
    pub url: String,
}

/// `owner/repo` and `owner--repo` both become the cellar name `owner--repo`.
pub fn cellar_name(spec: &str) -> Result<String> {
    let (_, _, name) = split_spec(spec)?;
    Ok(name)
}

pub fn split_spec(spec: &str) -> Result<(String, String, String)> {
    let spec = spec.trim().trim_start_matches("github:");
    let (owner, repo) = spec
        .split_once('/')
        .or_else(|| spec.split_once("--"))
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "github package {spec:?} must be owner/repo or owner--repo"
            ))
        })?;
    if owner.is_empty()
        || repo.is_empty()
        || owner.contains('/')
        || repo.contains('/')
        || owner.contains("--")
        || repo.contains("--")
        || owner.chars().any(char::is_whitespace)
        || repo.chars().any(char::is_whitespace)
        || !install::valid_component(owner)
        || !install::valid_component(repo)
    {
        return Err(Error::Unsupported(format!(
            "github package {spec:?} is not a valid owner/repo"
        )));
    }
    let name = format!("{owner}--{repo}");
    if !install::valid_component(&name) {
        return Err(Error::Unsupported(format!(
            "github package {name:?} is not a valid path component"
        )));
    }
    Ok((owner.to_string(), repo.to_string(), name))
}

fn version_from_release(release: &serde_json::Value, what: &str) -> Result<String> {
    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if tag.is_empty() {
        return Err(Error::NotFound(format!("no release tag for {what}")));
    }
    sanitize_version(tag)
}

pub fn sanitize_version(tag: &str) -> Result<String> {
    let trimmed = tag.trim();
    let stripped = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let version = if stripped.is_empty() {
        "unknown"
    } else {
        stripped
    };
    let cleaned: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if install::valid_component(&cleaned) {
        Ok(cleaned)
    } else {
        Err(Error::Unsupported(format!(
            "release tag {tag:?} is not a valid version"
        )))
    }
}

pub fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "unknown"
    }
}

/// Index of the asset that matches this OS and CPU.
///
/// Checksum files, signatures, and packages for another OS or CPU are skipped.
/// On Linux a `gnu` archive is preferred over `musl`.
pub fn pick_release_asset(names: &[String], os: &str, arch: &str) -> Option<usize> {
    let scored: Vec<(usize, i32)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| score_asset(name, os, arch).map(|score| (i, score)))
        .collect();
    // Drop an explicit wrong-CPU asset when any right-CPU asset exists.
    let any_right = scored
        .iter()
        .any(|(i, _)| arch_kind(&names[*i], arch) == ArchKind::Match);
    let best = scored
        .into_iter()
        .filter(|(i, _)| !any_right || arch_kind(&names[*i], arch) != ArchKind::Other)
        .max_by_key(|(i, score)| (*score, -(*i as i32)));
    best.map(|(i, _)| i)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchKind {
    Match,
    Other,
    Generic,
}

fn arch_kind(name: &str, arch: &str) -> ArchKind {
    let n = name.to_ascii_lowercase();
    let arm = n.contains("aarch64") || n.contains("arm64");
    let x64 = n.contains("x86_64") || n.contains("amd64") || contains_token(&n, "x64");
    match arch {
        "arm64" if arm => ArchKind::Match,
        "arm64" if x64 => ArchKind::Other,
        "x64" if x64 && !arm => ArchKind::Match,
        "x64" if arm => ArchKind::Other,
        _ => ArchKind::Generic,
    }
}

fn score_asset(name: &str, os: &str, arch: &str) -> Option<i32> {
    if rejected_asset(name) || !os_matches(name, os) {
        return None;
    }
    if arch_kind(name, arch) == ArchKind::Other {
        return None;
    }
    let n = name.to_ascii_lowercase();
    let mut score = match artifact_kind(name) {
        Some(ArtifactKind::TarGz) => 100,
        Some(ArtifactKind::Zip) => 80,
        Some(ArtifactKind::Raw) if os != "windows" || n.ends_with(".exe") || !n.contains('.') => 40,
        _ => return None,
    };
    if arch_kind(name, arch) == ArchKind::Match {
        score += 50;
    }
    if n.contains("unknown-linux-gnu") || n.contains("pc-windows-gnu") {
        score += 30;
    } else if n.contains("unknown-linux-musl") || n.contains("-musl") {
        score += 10;
    }
    if n.contains("apple-darwin") {
        score += 20;
    }
    if n.contains("pc-windows-msvc") {
        score += 15;
    }
    Some(score)
}

fn rejected_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const BAD: &[&str] = &[
        ".sha256",
        ".sha256sum",
        ".sha512",
        ".sig",
        ".asc",
        ".sbom",
        ".deb",
        ".rpm",
        ".dmg",
        ".pkg",
        ".msi",
        ".txt",
        ".pem",
        ".json",
    ];
    if BAD.iter().any(|ext| n.ends_with(ext)) {
        return true;
    }
    n.contains("checksum")
        || n.contains("sha256sum")
        || n.contains("-src.")
        || n.contains("-source")
        || n.contains(".src.")
}

fn os_matches(name: &str, os: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let darwin = n.contains("darwin") || n.contains("macos") || n.contains("osx");
    let linux = n.contains("linux") && !n.contains("darwin");
    let windows = n.contains("windows") || n.contains("win64") || n.contains("win32");
    match os {
        "macos" => darwin && !linux && !windows,
        "linux" => linux && !darwin && !windows,
        "windows" => windows && !darwin && !linux,
        _ => false,
    }
}

/// Archive or package formats maca cannot unpack. Installing one as a "raw"
/// executable would link a compressed file, so they are never candidates.
pub fn unsupported_archive(lower_name: &str) -> bool {
    const EXTS: &[&str] = &[
        ".tar.xz", ".txz", ".tar.bz2", ".tbz", ".tbz2", ".tar.zst", ".tzst", ".tar.lz", ".gz",
        ".bz2", ".xz", ".zst", ".7z", ".rar", ".apk", ".snap",
    ];
    EXTS.iter().any(|e| lower_name.ends_with(e))
}

pub fn artifact_kind(name: &str) -> Option<ArtifactKind> {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Some(ArtifactKind::TarGz)
    } else if n.ends_with(".zip") {
        Some(ArtifactKind::Zip)
    } else if unsupported_archive(&n) || rejected_asset(name) {
        None
    } else {
        Some(ArtifactKind::Raw)
    }
}

fn contains_token(hay: &str, token: &str) -> bool {
    hay.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == token)
}

pub fn assets_from_release(body: &serde_json::Value) -> (String, Vec<ListedAsset>) {
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let assets = body
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let name = a.get("name")?.as_str()?.to_string();
                    let url = a
                        .get("browser_download_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(ListedAsset { name, url })
                })
                .collect()
        })
        .unwrap_or_default();
    (tag, assets)
}

fn plan_from_assets(
    cellar: &str,
    owner: &str,
    repo: &str,
    tag: &str,
    assets: &[ListedAsset],
    os: &str,
    arch: &str,
) -> Result<PlannedRelease> {
    let names: Vec<String> = assets.iter().map(|a| a.name.clone()).collect();
    let Some(index) = pick_release_asset(&names, os, arch) else {
        let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
        return Err(Error::Unsupported(format!(
            "no {os}/{arch} asset in {owner}/{repo} release {tag} (have: {})",
            shown.join(", ")
        )));
    };
    let asset = &assets[index];
    let version = sanitize_version(tag)?;
    let url = if asset.url.is_empty() {
        format!(
            "https://github.com/{owner}/{repo}/releases/download/{tag}/{}",
            asset.name
        )
    } else {
        asset.url.clone()
    };
    let checksum_url = checksum_asset(assets, &asset.name).map(|a| {
        if a.url.is_empty() {
            format!(
                "https://github.com/{owner}/{repo}/releases/download/{tag}/{}",
                a.name
            )
        } else {
            a.url.clone()
        }
    });
    let kind = artifact_kind(&asset.name).unwrap_or(ArtifactKind::Raw);
    Ok(PlannedRelease {
        plan: InstallPlan {
            name: cellar.to_string(),
            version,
            backend: "github".into(),
            url,
            sha256: None,
            note: Some(format!("asset `{}`", asset.name)),
        },
        checksum_url,
        asset_name: asset.name.clone(),
        kind,
        raw_exe: repo.to_string(),
    })
}

fn checksum_asset<'a>(assets: &'a [ListedAsset], asset: &str) -> Option<&'a ListedAsset> {
    let want = [
        format!("{asset}.sha256"),
        format!("{asset}.sha256sum"),
        "checksums.txt".to_string(),
        "SHA256SUMS".to_string(),
        "sha256sums.txt".to_string(),
    ];
    assets.iter().find(|a| {
        want.iter().any(|w| a.name.eq_ignore_ascii_case(w))
            || a.name.to_ascii_lowercase().contains("checksum")
    })
}

pub fn packages_from_search(body: &serde_json::Value, limit: usize) -> Vec<Package> {
    let mut out = Vec::new();
    let Some(items) = body.get("items").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in items {
        let Some(full) = item.get("full_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok((owner, repo, cellar)) = split_spec(full) else {
            continue;
        };
        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let homepage = item
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(package(
            &cellar,
            "latest",
            &description,
            &homepage,
            &owner,
            &repo,
        ));
        if out.len() >= limit {
            break;
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn package(
    name: &str,
    version: &str,
    description: &str,
    homepage: &str,
    owner: &str,
    repo: &str,
) -> Package {
    Package {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        homepage: if homepage.is_empty() {
            format!("https://github.com/{owner}/{repo}")
        } else {
            homepage.to_string()
        },
        backend: "github".into(),
        tags: vec![format!("github.com/{owner}/{repo}")],
    }
}

/// Verify `bytes` and only then publish a keg. A checksum mismatch leaves no keg.
pub fn commit_artifact(
    name: &str,
    version: &str,
    bytes: &[u8],
    expected_sha: Option<&str>,
    cellar_root: &Path,
    kind: ArtifactKind,
    raw_exe: &str,
) -> Result<PathBuf> {
    install::verify_sha256(name, bytes, expected_sha)?;
    match kind {
        ArtifactKind::TarGz => install::extract_tar_gz_to_cellar(name, version, bytes, cellar_root),
        ArtifactKind::Zip => install::extract_zip_to_cellar(name, version, bytes, cellar_root),
        ArtifactKind::Raw => write_raw(name, version, bytes, raw_exe, cellar_root),
    }
}

fn write_raw(
    name: &str,
    version: &str,
    bytes: &[u8],
    exe: &str,
    cellar_root: &Path,
) -> Result<PathBuf> {
    if !install::valid_component(exe) {
        return Err(Error::Unsupported(format!(
            "binary name {exe:?} is not a valid path component"
        )));
    }
    let dest = cellar_root.join(name).join(version);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bin_dir = dest.join("bin");
    let result = (|| -> Result<PathBuf> {
        std::fs::create_dir_all(&bin_dir)?;
        let path = bin_dir.join(exe);
        install::atomic_write(&path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(dest.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    result
}

fn cellar_path(prefix: &Path, name: &str, version: &str) -> PathBuf {
    crate::config::cellar_dir(prefix).join(name).join(version)
}

/// Tag spellings a release might be published under for one version.
///
/// Cellar versions have no leading `v` (`sanitize_version` strips it), but
/// release tags usually have one. Both are tried; the first that exists wins.
pub fn tag_candidates(version: &str) -> Vec<String> {
    let version = version.trim();
    let bare = version.strip_prefix('v').unwrap_or(version);
    let mut out = Vec::new();
    if !bare.is_empty() {
        out.push(format!("v{bare}"));
        if bare != version || !version.starts_with('v') {
            out.push(bare.to_string());
        }
    }
    out.dedup();
    out
}

/// The release to install: the current one, or the one a caller named.
async fn fetch_release(
    owner: &str,
    repo: &str,
    version: Option<&str>,
) -> Result<serde_json::Value> {
    let base = api_base();
    let Some(version) = version.map(str::trim).filter(|s| !s.is_empty()) else {
        return github_json(&format!("{base}/repos/{owner}/{repo}/releases/latest")).await;
    };
    let mut last = Error::NotFound(format!(
        "no github release of {owner}/{repo} matches version {version}"
    ));
    for tag in tag_candidates(version) {
        match github_json(&format!("{base}/repos/{owner}/{repo}/releases/tags/{tag}")).await {
            Ok(body) => return Ok(body),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub(crate) fn api_base() -> String {
    std::env::var("MACA_GITHUB_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
        .trim_end_matches('/')
        .to_string()
}

fn github_token() -> Option<String> {
    for key in ["MACA_GITHUB_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    None
}

async fn github_json(url: &str) -> Result<serde_json::Value> {
    let client = install::http_client()?;
    let mut req = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", format!("maca/{VERSION}"));
    if let Some(token) = github_token() {
        req = req.bearer_auth(token);
    }
    let response = req
        .send()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(Error::Network(format!("GET {url} returned {status}")));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn form_encode(query: &str) -> String {
    let mut out = String::new();
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsupported_archives_are_not_raw_executables() {
        assert_eq!(artifact_kind("tool-linux-amd64.tar.xz"), None);
        assert_eq!(artifact_kind("tool-linux-amd64.tar.zst"), None);
        assert_eq!(artifact_kind("tool-linux-amd64.7z"), None);
        assert_eq!(artifact_kind("tool-linux-amd64.gz"), None);
        assert_eq!(artifact_kind("x.AppImage"), Some(ArtifactKind::Raw));
        assert_eq!(artifact_kind("tool.tar.gz"), Some(ArtifactKind::TarGz));
    }

    #[test]
    fn plan_refuses_a_release_whose_only_asset_is_tar_xz() {
        let assets = vec![ListedAsset {
            name: "tool-1.0-x86_64-unknown-linux-gnu.tar.xz".into(),
            url: "https://example.test/a.tar.xz".into(),
        }];
        assert!(
            plan_from_assets("o--tool", "o", "tool", "v1.0", &assets, "linux", "x86_64").is_err()
        );
    }

    #[test]
    fn release_version_requires_a_tag() {
        let ok = serde_json::json!({"tag_name": "v1.2.3"});
        assert_eq!(version_from_release(&ok, "o/r").unwrap(), "1.2.3");
        for bad in [serde_json::json!({}), serde_json::json!({"tag_name": ""})] {
            assert!(version_from_release(&bad, "o/r").is_err());
        }
    }

    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_version_is_looked_up_under_both_tag_spellings() {
        assert_eq!(tag_candidates("1.2.3"), vec!["v1.2.3", "1.2.3"]);
        assert_eq!(tag_candidates("v1.2.3"), vec!["v1.2.3", "1.2.3"]);
        assert_eq!(tag_candidates("  v1.2.3 "), vec!["v1.2.3", "1.2.3"]);
        assert!(tag_candidates("v").is_empty());
    }

    #[test]
    fn slash_spec_is_cellar_safe_and_double_dash_roundtrips() {
        let name = cellar_name("BurntSushi/ripgrep").unwrap();
        assert_eq!(name, "BurntSushi--ripgrep");
        assert!(install::valid_component(&name));
        assert_eq!(cellar_name("BurntSushi--ripgrep").unwrap(), name);
        assert!(cellar_name("../x").is_err());
        assert!(cellar_name("a/b/c").is_err());
        assert!(cellar_name("has space/repo").is_err());
    }

    #[test]
    fn picks_arm64_darwin_tarball_over_checksum_and_wrong_cpu() {
        let assets = names(&[
            "ripgrep-14.1.0-x86_64-apple-darwin.tar.gz",
            "ripgrep-14.1.0-aarch64-apple-darwin.tar.gz.sha256",
            "ripgrep-14.1.0-aarch64-apple-darwin.tar.gz",
            "ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz",
        ]);
        let i = pick_release_asset(&assets, "macos", "arm64").unwrap();
        assert!(assets[i].contains("aarch64-apple-darwin.tar.gz"));
        assert!(!assets[i].contains("sha256"));
    }

    #[test]
    fn prefers_gnu_over_musl_and_windows_zip_over_nothing() {
        let linux = names(&[
            "tool-1.0.0-x86_64-unknown-linux-musl.tar.gz",
            "tool-1.0.0-x86_64-unknown-linux-gnu.tar.gz",
            "checksums.txt",
        ]);
        let i = pick_release_asset(&linux, "linux", "x64").unwrap();
        assert!(linux[i].contains("linux-gnu"));
        let windows = names(&[
            "tool-1.0.0-x86_64-pc-windows-msvc.zip",
            "tool-1.0.0-x86_64-unknown-linux-gnu.tar.gz",
            "SHA256SUMS",
        ]);
        let i = pick_release_asset(&windows, "windows", "x64").unwrap();
        assert!(windows[i].ends_with(".zip"));
        assert!(pick_release_asset(&names(&["checksums.txt"]), "linux", "x64").is_none());
    }

    #[test]
    fn plan_uses_cellar_name_and_strips_v() {
        let assets = vec![ListedAsset {
            name: "hello-1.2.3-aarch64-apple-darwin.tar.gz".into(),
            url: "https://example.test/hello.tar.gz".into(),
        }];
        let planned = plan_from_assets(
            "acme--hello",
            "acme",
            "hello",
            "v1.2.3",
            &assets,
            "macos",
            "arm64",
        )
        .unwrap();
        assert_eq!(planned.plan.name, "acme--hello");
        assert_eq!(planned.plan.version, "1.2.3");
        assert_eq!(planned.plan.url, "https://example.test/hello.tar.gz");
        assert!(install::valid_component(&planned.plan.name));
    }

    #[test]
    fn unmatched_asset_names_the_release() {
        let assets = vec![ListedAsset {
            name: "only-linux.tar.gz".into(),
            url: "https://example.test/only.tar.gz".into(),
        }];
        let err = plan_from_assets("a--b", "a", "b", "v1", &assets, "macos", "arm64").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no macos/arm64"), "{text}");
        assert!(text.contains("only-linux.tar.gz"), "{text}");
    }

    #[test]
    fn checksum_file_picks_the_asset_line() {
        let text = "aaaa  other.tar.gz\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  hello.tar.gz\n";
        let hex = sha256_from_sums(text, "hello.tar.gz").unwrap();
        assert!(hex.starts_with("0123456789abcdef"));
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn bad_checksum_leaves_no_keg() {
        let prefix = std::env::temp_dir().join(format!(
            "maca-github-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let err = commit_artifact(
            "acme--hello",
            "1.0.0",
            b"not-a-tarball",
            Some("deadbeef"),
            &prefix.join("cellar"),
            ArtifactKind::TarGz,
            "hello",
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(
            !prefix
                .join("cellar")
                .join("acme--hello")
                .join("1.0.0")
                .exists()
        );
        assert!(
            !prefix.join("cellar").join("acme--hello").exists()
                || prefix
                    .join("cellar")
                    .join("acme--hello")
                    .read_dir()
                    .unwrap()
                    .next()
                    .is_none()
        );
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn version_sanitizes_odd_tags() {
        assert_eq!(sanitize_version("v1.2.3").unwrap(), "1.2.3");
        let cleaned = sanitize_version("v1.2.3 build").unwrap();
        assert!(install::valid_component(&cleaned), "{cleaned}");
        assert!(!cleaned.contains(' '));
    }

    #[test]
    fn search_json_skips_unsafe_names() {
        let body = serde_json::json!({
            "items": [
                {"full_name": "acme/hello", "description": "hi", "html_url": "https://github.com/acme/hello"},
                {"full_name": "bad/../../x", "description": "no"}
            ]
        });
        let pkgs = packages_from_search(&body, 10);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "acme--hello");
        assert_eq!(pkgs[0].backend, "github");
        assert_eq!(pkgs[0].tags, vec!["github.com/acme/hello".to_string()]);
    }
}
