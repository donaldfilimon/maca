//! Self-update: replace the running `maca` binary from a GitHub release.
//!
//! Release asset convention (documented, not yet published):
//! ```text
//! maca-<target>.tar.gz      (contains the `maca` binary)
//! maca-<target>.tar.gz.sha256
//! ```
//! (`maca-<target>.zip` containing `maca.exe` on Windows) where `<target>` is
//! a Rust target triple such as `aarch64-apple-darwin`.
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
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
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

/// Fetch the latest stable release. `Ok(None)` when the repo has none.
/// GitHub's `/releases/latest` endpoint never returns a prerelease or draft.
/// The API origin honours `MACA_GITHUB_API`, like the github backend.
pub async fn fetch_latest(repo: &str) -> Result<Option<Release>> {
    let url = format!("{}/repos/{repo}/releases/latest", crate::github::api_base());
    let bytes = match install::download(&url).await {
        Ok(b) => b,
        Err(crate::Error::Network(m)) if install::is_http_404(&m) => return Ok(None),
        Err(e) => return Err(e),
    };
    let release = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("decoding release for {repo}: {e}")))?;
    Ok(Some(release))
}

/// Fetch the release `self-update` should consider: the latest stable one,
/// or with `include_prereleases` the highest-versioned published release of
/// any kind (the first page of the release list, newest 100).
pub async fn fetch_release(repo: &str, include_prereleases: bool) -> Result<Option<Release>> {
    if !include_prereleases {
        return fetch_latest(repo).await;
    }
    let url = format!(
        "{}/repos/{repo}/releases?per_page=100",
        crate::github::api_base()
    );
    let bytes = match install::download(&url).await {
        Ok(b) => b,
        Err(crate::Error::Network(m)) if install::is_http_404(&m) => return Ok(None),
        Err(e) => return Err(e),
    };
    let releases: Vec<Release> = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Network(format!("decoding releases for {repo}: {e}")))?;
    Ok(newest_release(releases))
}

/// The highest-versioned release that is not a draft and has a tag.
/// Prereleases are candidates; `0.2.0-alpha.1` sorts below `0.2.0`.
pub fn newest_release(releases: Vec<Release>) -> Option<Release> {
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.tag_name.trim().is_empty())
        .reduce(|best, r| {
            if is_newer(&r.tag_name, &best.tag_name) {
                r
            } else {
                best
            }
        })
}

/// Whether to look at prereleases: when asked to, or when the running build
/// is itself a prerelease (an alpha has no stable release to move to, and
/// would otherwise never see the next alpha).
pub fn tracks_prereleases(current_version: &str, requested: bool) -> bool {
    requested
        || semver::Version::parse(current_version.trim().trim_start_matches('v'))
            .is_ok_and(|v| !v.pre.is_empty())
}

/// What to say when there is nothing to update to.
pub fn no_release_message(repo: &str, include_prereleases: bool) -> String {
    if include_prereleases {
        format!("no release published for {repo}")
    } else {
        format!(
            "no stable release published for {repo} (pass --prerelease to consider prereleases)"
        )
    }
}

/// Compare a release tag against the current version. `true` if newer.
pub fn is_newer(tag: &str, current: &str) -> bool {
    crate::backend::is_newer_version(tag, current)
}

/// Download and install the newest binary. Returns the new version on success.
pub async fn run(
    repo: &str,
    current_version: &str,
    force: bool,
    include_prereleases: bool,
) -> Result<String> {
    let Some(release) = fetch_release(repo, include_prereleases).await? else {
        return Ok(no_release_message(repo, include_prereleases));
    };
    let tag = release.tag_name.clone();
    if tag.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "{}",
            no_release_message(repo, include_prereleases)
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

/// Upper bound on the bytes read for the extracted binary (a zip/gzip bomb
/// guard). The release binary is a few MiB; this leaves a wide margin.
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

/// Typed failures of archive extraction.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("update archive is corrupt or unreadable: {0}")]
    Corrupt(String),
    #[error("update archive did not contain a `{0}` binary")]
    MissingBinary(String),
    #[error("update archive contains more than one `{0}` (at the root or one directory down)")]
    Ambiguous(String),
    #[error("update archive entry has an unsafe path (absolute or `..`): {0:?}")]
    UnsafePath(String),
    #[error("update archive entry `{name}` is not a regular file")]
    NotRegularFile { name: String },
    #[error("update archive entry `{name}` exceeds the {limit}-byte limit")]
    TooLarge { name: String, limit: u64 },
    #[error("writing the extracted binary: {0}")]
    Io(String),
}

/// True if `path` is absolute or climbs out with `..`.
fn path_is_unsafe(path: &Path) -> bool {
    use std::path::Component;
    path.components().any(|c| {
        matches!(
            c,
            Component::RootDir | Component::Prefix(_) | Component::ParentDir
        )
    })
}

/// True if `path` is exactly `bin_name`, at the archive root or one top
/// directory down (`./` is ignored). Deeper copies (docs/examples/maca) are
/// not the binary.
fn is_binary_path(path: &Path, bin_name: &str) -> bool {
    use std::path::Component;
    let parts: Vec<_> = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    matches!(parts.len(), 1 | 2)
        && parts
            .last()
            .is_some_and(|c| matches!(c, Component::Normal(n) if *n == bin_name))
}

/// Create the staging file. `create_new` refuses an existing file or a
/// symlink planted at the path, so nothing is written through a link.
fn create_staging(dest: &Path) -> std::result::Result<std::fs::File, ArchiveError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| ArchiveError::Io(e.to_string()))
}

/// Copy at most `max_bytes` from `entry` into a new file at `dest`. The size
/// the archive declares can lie, so the read itself is bounded. On any error
/// after the file was created it is removed again.
fn copy_bounded(
    entry: impl std::io::Read,
    name: &str,
    dest: &Path,
    max_bytes: u64,
) -> std::result::Result<(), ArchiveError> {
    let mut out = create_staging(dest)?;
    let result = (|| {
        let mut limited = std::io::Read::take(entry, max_bytes + 1);
        // A CRC or inflate failure surfaces here as an io error.
        let copied = std::io::copy(&mut limited, &mut out)
            .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
        if copied > max_bytes {
            return Err(ArchiveError::TooLarge {
                name: name.to_string(),
                limit: max_bytes,
            });
        }
        out.sync_all().map_err(|e| ArchiveError::Io(e.to_string()))
    })();
    if result.is_err() {
        drop(out);
        let _ = std::fs::remove_file(dest);
    }
    result
}

/// Extract `bin_name` from a `.zip` archive into a new file at `dest`.
///
/// Platform-independent (unit-tested on every host). Every entry's name must
/// be enclosed (no absolute path, no `..`) or the whole archive is rejected;
/// exactly one regular file named `bin_name` (archive root or one top
/// directory) is extracted, more than one is ambiguous; at most `max_bytes`
/// of it are read. `dest` must not exist; on error nothing is left at it.
/// A zip entry's name for error messages; an undecodable one is not fatal here.
#[cfg_attr(not(windows), allow(dead_code))]
fn entry_name<R: std::io::Read + ?Sized>(entry: &zip::read::ZipFile<'_, R>) -> String {
    entry
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "<undecodable name>".to_string())
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn extract_zip_binary(
    archive: &[u8],
    bin_name: &str,
    dest: &Path,
    max_bytes: u64,
) -> std::result::Result<(), ArchiveError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    // Validate every entry name first so a hostile archive is rejected even
    // if its bad entry comes after the binary.
    let mut target: Option<usize> = None;
    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
        let raw = entry_name(&entry);
        if crate::install::zip_name_is_unsafe(&raw) {
            return Err(ArchiveError::UnsafePath(raw));
        }
        let Some(path) = entry.enclosed_name() else {
            return Err(ArchiveError::UnsafePath(raw));
        };
        if entry.is_dir() || !is_binary_path(&path, bin_name) {
            continue;
        }
        // Symlinks (Unix mode bits in the zip) are not regular files.
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(ArchiveError::NotRegularFile {
                name: entry_name(&entry),
            });
        }
        if target.replace(i).is_some() {
            return Err(ArchiveError::Ambiguous(bin_name.to_string()));
        }
    }
    let index = target.ok_or_else(|| ArchiveError::MissingBinary(bin_name.to_string()))?;
    let entry = zip
        .by_index(index)
        .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    let name = entry_name(&entry);
    if entry.size() > max_bytes {
        return Err(ArchiveError::TooLarge {
            name,
            limit: max_bytes,
        });
    }
    copy_bounded(entry, &name, dest, max_bytes)
}

/// Extract `bin_name` from a `.tar.gz` archive into a new file at `dest`,
/// with the same rules as [`extract_zip_binary`]: no absolute or `..` entry
/// anywhere, exactly one regular-file match (symlinks and other special
/// entries are refused), bounded read, `dest` created with `create_new`.
/// Platform-independent so it is unit-tested on every host.
#[cfg_attr(windows, allow(dead_code))]
pub fn extract_tar_gz_binary(
    archive: &[u8],
    bin_name: &str,
    dest: &Path,
    max_bytes: u64,
) -> std::result::Result<(), ArchiveError> {
    let corrupt = |e: std::io::Error| ArchiveError::Corrupt(e.to_string());
    let open = || tar::Archive::new(flate2::read::GzDecoder::new(archive));
    // Pass 1: validate every path and find the single match.
    let mut matches = 0usize;
    for entry in open().entries().map_err(corrupt)? {
        let entry = entry.map_err(corrupt)?;
        let path = entry.path().map_err(corrupt)?.to_path_buf();
        if path_is_unsafe(&path) {
            return Err(ArchiveError::UnsafePath(path.display().to_string()));
        }
        if entry.header().entry_type().is_dir() || !is_binary_path(&path, bin_name) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(ArchiveError::NotRegularFile {
                name: path.display().to_string(),
            });
        }
        matches += 1;
    }
    match matches {
        0 => return Err(ArchiveError::MissingBinary(bin_name.to_string())),
        1 => {}
        _ => return Err(ArchiveError::Ambiguous(bin_name.to_string())),
    }
    // Pass 2: extract it.
    for entry in open().entries().map_err(corrupt)? {
        let entry = entry.map_err(corrupt)?;
        let path = entry.path().map_err(corrupt)?.to_path_buf();
        if entry.header().entry_type().is_file() && is_binary_path(&path, bin_name) {
            if entry.size() > max_bytes {
                return Err(ArchiveError::TooLarge {
                    name: path.display().to_string(),
                    limit: max_bytes,
                });
            }
            return copy_bounded(entry, &path.display().to_string(), dest, max_bytes);
        }
    }
    Err(ArchiveError::MissingBinary(bin_name.to_string()))
}

/// Extract the `maca` binary (`maca.exe` on Windows) from the release archive
/// (`.tar.gz`, or `.zip` on Windows) into a temp path.
fn extract_binary(archive: &[u8], exe: &Path) -> Result<PathBuf> {
    // Staged next to the running binary so the final rename never crosses a
    // filesystem boundary. The name is unpredictable and the file is created
    // with `create_new`, so a planted file or symlink makes this fail instead
    // of being written through.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".maca-update-{}-{nanos:x}", std::process::id()));
    #[cfg(target_os = "windows")]
    {
        extract_zip_binary(archive, "maca.exe", &tmp, MAX_BINARY_BYTES)?;
        Ok(tmp)
    }
    #[cfg(not(target_os = "windows"))]
    {
        extract_tar_gz_binary(archive, "maca", &tmp, MAX_BINARY_BYTES)?;
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
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
        swap_in_by_rename(exe, new_binary, &backup)?;
    }
    prune_backups(exe, &backup);
    Ok(())
}

/// Remove earlier `<name>.bak.<seconds>` files next to `exe`, keeping `keep`
/// (the backup just written), so repeated updates leave one backup behind
/// instead of one per update. Only regular files with exactly that name shape
/// are touched; failures are ignored (on Windows the previous backup can
/// still be the running image).
fn prune_backups(exe: &Path, keep: &Path) {
    let (Some(dir), Some(stem)) = (exe.parent(), exe.file_stem().and_then(|s| s.to_str())) else {
        return;
    };
    let prefix = format!("{stem}.bak.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let is_backup = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
        let is_file = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
        if is_backup && is_file {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Replace `exe` with `new_binary` by renaming, which Windows allows even for
/// a running executable (writing to or deleting it is what it refuses): the
/// running binary is renamed to `backup`, then the new one takes its name. If
/// the second rename fails the original is moved back. Platform-independent
/// so it is unit-tested on every host; Windows is its only caller.
#[cfg_attr(not(windows), allow(dead_code))]
fn swap_in_by_rename(exe: &Path, new_binary: &Path, backup: &Path) -> Result<()> {
    std::fs::rename(exe, backup)?;
    if let Err(e) = std::fs::rename(new_binary, exe) {
        // Put the original back so a failed update leaves a working binary.
        let _ = std::fs::rename(backup, exe);
        return Err(e.into());
    }
    Ok(())
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| Error::Other(anyhow::anyhow!("current_exe: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            assets: vec![],
            body: None,
            draft,
            prerelease,
        }
    }

    #[test]
    fn newest_release_orders_by_version_and_skips_drafts() {
        // GitHub lists by creation date, so the list order is not the answer.
        let picked = newest_release(vec![
            release("v0.1.0-alpha.2", false, true),
            release("v0.2.0", true, false),
            release("v0.1.0", false, false),
            release("", false, false),
            release("v0.1.0-alpha.1", false, true),
        ]);
        assert_eq!(picked.map(|r| r.tag_name).as_deref(), Some("v0.1.0"));
        let only_alphas = newest_release(vec![
            release("v0.1.0-alpha.1", false, true),
            release("v0.1.0-alpha.2", false, true),
        ]);
        assert_eq!(
            only_alphas.map(|r| r.tag_name).as_deref(),
            Some("v0.1.0-alpha.2")
        );
        assert!(newest_release(vec![release("v9.0.0", true, false)]).is_none());
    }

    #[test]
    fn a_prerelease_build_tracks_prereleases_without_the_flag() {
        assert!(tracks_prereleases("0.1.0-alpha.1", false));
        assert!(tracks_prereleases("v0.1.0-rc.1", false));
        assert!(!tracks_prereleases("0.1.0", false));
        assert!(tracks_prereleases("0.1.0", true));
        assert!(!tracks_prereleases("not-a-version", false));
    }

    #[test]
    fn prune_backups_keeps_the_new_backup_and_unrelated_files() {
        let dir = std::env::temp_dir().join(format!("maca-bak-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("maca");
        let keep = dir.join("maca.bak.200");
        for name in [
            "maca",
            "maca.bak.100",
            "maca.bak.200",
            "maca.bak.notes",
            "maca.bak.",
            "other.bak.100",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("maca.bak.300")).unwrap();
        prune_backups(&exe, &keep);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "maca",
                "maca.bak.",
                "maca.bak.200",
                "maca.bak.300",
                "maca.bak.notes",
                "other.bak.100"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    // ---- zip extraction (platform-independent; run on the host, never on
    // Windows as of writing) ----

    use std::io::Write;

    /// Build a zip. The writer (zip 9) sanitizes `..` and a leading `/` out of
    /// entry names, which would make hostile archives impossible to build, so
    /// such names are written as same-length stand-ins and patched back in the
    /// finished bytes (local and central headers both carry the name).
    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut patches: Vec<(String, String)> = Vec::new();
        for (name, data) in entries {
            let mut stand_in = name.replace("..", "ZZ");
            if let Some(rest) = stand_in.strip_prefix('/') {
                stand_in = format!("Z{rest}");
            }
            if stand_in != *name {
                patches.push((stand_in.clone(), name.to_string()));
            }
            w.start_file(stand_in, opts).unwrap();
            w.write_all(data).unwrap();
        }
        let mut bytes = w.finish().unwrap().into_inner();
        for (from, to) in patches {
            let (from, to) = (from.as_bytes(), to.as_bytes());
            let mut at = 0;
            while let Some(pos) = bytes[at..].windows(from.len()).position(|w| w == from) {
                bytes[at + pos..at + pos + from.len()].copy_from_slice(to);
                at += pos + from.len();
            }
        }
        bytes
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("maca-zip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn zip_extracts_binary_at_root_and_in_top_dir() {
        let dir = scratch("ok");
        for (tag, name) in [("root", "maca.exe"), ("nested", "maca-x86_64/maca.exe")] {
            let zip = zip_with(&[("README.txt", b"hi"), (name, b"MZ-binary-bytes")]);
            let dest = dir.join(tag);
            extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), b"MZ-binary-bytes");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_without_the_binary_is_a_typed_error() {
        let dir = scratch("missing");
        let dest = dir.join("out");
        let zip = zip_with(&[("README.txt", b"hi"), ("maca", b"wrong platform name")]);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::MissingBinary(ref n) if n == "maca.exe"));
        assert!(!dest.exists());
        // It converts into the crate error with its message intact.
        let e: Error = ArchiveError::MissingBinary("maca.exe".into()).into();
        assert!(matches!(e, Error::Archive(ArchiveError::MissingBinary(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_with_traversal_or_absolute_entry_is_rejected() {
        let dir = scratch("traversal");
        let dest = dir.join("out");
        for bad in ["../maca.exe", "a/../../maca.exe", "/abs/maca.exe"] {
            let zip = zip_with(&[(bad, b"evil")]);
            let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
            assert!(matches!(err, ArchiveError::UnsafePath(_)), "{bad}: {err}");
            assert!(!dest.exists());
        }
        // A bad entry after a good binary still rejects the archive.
        let zip = zip_with(&[("maca.exe", b"ok"), ("../evil.txt", b"x")]);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::UnsafePath(_)));
        assert!(!dest.exists());
        assert!(!dir.parent().unwrap().join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_zip_entry_is_refused_and_leaves_nothing() {
        let dir = scratch("big");
        let dest = dir.join("out");
        // 1 MiB of zeros deflates to ~1 KiB: the zip-bomb shape.
        let zip = zip_with(&[("maca.exe", &vec![0u8; 1024 * 1024])]);
        assert!(zip.len() < 8 * 1024);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 64 * 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::TooLarge { limit, .. } if limit == 64 * 1024));
        assert!(!dest.exists());
        // Exactly at the limit is fine.
        let zip = zip_with(&[("maca.exe", &vec![1u8; 4096])]);
        extract_zip_binary(&zip, "maca.exe", &dest, 4096).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 4096);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_zip_is_a_typed_error() {
        let dir = scratch("corrupt");
        let dest = dir.join("out");
        let err = extract_zip_binary(b"definitely not a zip", "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::Corrupt(_)));
        let mut zip = zip_with(&[("maca.exe", &vec![7u8; 2048])]);
        zip.truncate(zip.len() / 2);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 4096).unwrap_err();
        assert!(matches!(err, ArchiveError::Corrupt(_)));
        // Flip a byte inside the compressed data of a full archive.
        let mut zip = zip_with(&[("maca.exe", b"some reasonably long binary payload")]);
        zip[40] ^= 0xff;
        let res = extract_zip_binary(&zip, "maca.exe", &dest, 4096);
        assert!(res.is_err());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_ignores_deep_copies_and_rejects_ambiguity() {
        let dir = scratch("depth");
        let dest = dir.join("out");
        // A deeper sample listed first is not the binary.
        let zip = zip_with(&[
            ("docs/examples/maca.exe", b"fixture"),
            ("maca.exe", b"real"),
        ]);
        extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"real");
        std::fs::remove_file(&dest).unwrap();
        // Only a deep copy: missing.
        let zip = zip_with(&[("a/b/maca.exe", b"deep")]);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::MissingBinary(_)));
        // Two candidates at allowed depths: ambiguous, nothing written.
        let zip = zip_with(&[("maca.exe", b"one"), ("top/maca.exe", b"two")]);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::Ambiguous(_)));
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_with_a_lying_size_header_is_bounded_by_the_read() {
        let dir = scratch("lie");
        let dest = dir.join("out");
        let mut zip = zip_with(&[("maca.exe", &vec![0u8; 1024 * 1024])]);
        let patch = |zip: &mut Vec<u8>, sig: &[u8; 4], off: usize| {
            let at = zip.windows(4).position(|w| w == sig).unwrap() + off;
            zip[at..at + 4].copy_from_slice(&100u32.to_le_bytes());
        };
        // Declared uncompressed size: local header +22, central directory +24.
        patch(&mut zip, b"PK\x03\x04", 22);
        patch(&mut zip, b"PK\x01\x02", 24);
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(&zip)).unwrap();
        assert_eq!(z.by_index(0).unwrap().size(), 100, "header patch applied");
        drop(z);
        // The declared 100 bytes pass the header check; the 1 MiB inflate
        // must still be stopped by the bounded read.
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 64 * 1024).unwrap_err();
        // zip 9 also notices the lie itself and reports the archive corrupt;
        // either way the entry is refused and nothing is written.
        assert!(
            matches!(
                err,
                ArchiveError::TooLarge { .. } | ArchiveError::Corrupt(_)
            ),
            "{err}"
        );
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_file_is_never_written_through_an_existing_path() {
        let dir = scratch("create-new");
        let dest = dir.join("out");
        std::fs::write(&dest, b"planted").unwrap();
        let zip = zip_with(&[("maca.exe", b"new")]);
        let err = extract_zip_binary(&zip, "maca.exe", &dest, 1024).unwrap_err();
        assert!(matches!(err, ArchiveError::Io(_)));
        // The planted file is untouched (and not deleted by the cleanup).
        assert_eq!(std::fs::read(&dest).unwrap(), b"planted");
        #[cfg(unix)]
        {
            let target = dir.join("victim");
            std::fs::write(&target, b"victim").unwrap();
            let link = dir.join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(extract_zip_binary(&zip, "maca.exe", &link, 1024).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"victim");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tar_gz(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, ty, data) in entries {
            let mut h = tar::Header::new_gnu();
            // Write the name raw so `..` and absolute paths can be tested.
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_entry_type(*ty);
            h.set_size(if ty.is_file() { data.len() as u64 } else { 0 });
            h.set_mode(0o644);
            if ty.is_symlink() {
                h.set_link_name("/etc/hosts").unwrap();
            }
            h.set_cksum();
            b.append(&h, if ty.is_file() { *data } else { &b""[..] })
                .unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn tar_extracts_regular_binary_and_refuses_hostile_entries() {
        use tar::EntryType::{Regular, Symlink};
        let dir = scratch("tar");
        let dest = dir.join("out");
        let f = |e: &[(&str, tar::EntryType, &[u8])], max| {
            let r = extract_tar_gz_binary(&tar_gz(e), "maca", &dest, max);
            let ok = r.is_ok();
            if !ok {
                assert!(!dest.exists(), "nothing left behind on error");
            }
            r.map(|()| std::fs::read(&dest).unwrap_or_default())
                .inspect(|_| {
                    let _ = std::fs::remove_file(&dest);
                })
        };
        assert_eq!(
            f(
                &[
                    ("maca-x/maca", Regular, b"bin"),
                    ("maca-x/README", Regular, b"r")
                ],
                1024
            )
            .unwrap(),
            b"bin"
        );
        assert_eq!(f(&[("./maca", Regular, b"bin")], 1024).unwrap(), b"bin");
        assert!(matches!(
            f(&[("maca", Symlink, b"")], 1024),
            Err(ArchiveError::NotRegularFile { .. })
        ));
        assert!(matches!(
            f(
                &[("maca", Regular, b"bin"), ("../evil", Regular, b"x")],
                1024
            ),
            Err(ArchiveError::UnsafePath(_))
        ));
        assert!(matches!(
            f(&[("/abs/maca", Regular, b"bin")], 1024),
            Err(ArchiveError::UnsafePath(_))
        ));
        assert!(matches!(
            f(&[("docs/ex/maca", Regular, b"x")], 1024),
            Err(ArchiveError::MissingBinary(_))
        ));
        assert!(matches!(
            f(&[("maca", Regular, b"1"), ("t/maca", Regular, b"2")], 1024),
            Err(ArchiveError::Ambiguous(_))
        ));
        assert!(matches!(
            f(&[("maca", Regular, &[0u8; 4096])], 64),
            Err(ArchiveError::TooLarge { .. })
        ));
        assert!(matches!(
            extract_tar_gz_binary(b"not gzip", "maca", &dest, 64),
            Err(ArchiveError::Corrupt(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_swap_replaces_and_restores_on_failure() {
        let dir = scratch("swap");
        let exe = dir.join("maca.exe");
        let new = dir.join("new");
        let backup = dir.join("maca.bak.1");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();
        swap_in_by_rename(&exe, &new, &backup).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old");
        assert!(!new.exists());

        // Failure of the second rename puts the original back.
        let exe2 = dir.join("maca2.exe");
        std::fs::write(&exe2, b"keep").unwrap();
        let err = swap_in_by_rename(&exe2, &dir.join("missing"), &dir.join("maca2.bak"));
        assert!(err.is_err());
        assert_eq!(std::fs::read(&exe2).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
