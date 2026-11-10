//! Download, verify and extract helpers shared by all backends.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config;
use crate::{Error, Result};

/// SHA-256 hex of in-memory bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Verify bytes against an expected hex digest (case-insensitive).
pub fn verify_sha256(name: &str, bytes: &[u8], expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        let actual = sha256_hex(bytes);
        if actual.eq_ignore_ascii_case(expected.trim()) {
            Ok(())
        } else {
            Err(Error::Checksum {
                name: name.to_string(),
                expected: expected.to_string(),
                actual,
            })
        }
    } else {
        Ok(())
    }
}

/// Digest for `asset` from a checksum file.
///
/// Accepts `hex  filename`, `hex *filename`, or a file whose only line is the hex.
pub fn sha256_from_sums(text: &str, asset: &str) -> Option<String> {
    let want = std::path::Path::new(asset)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(asset);
    let mut lone = None;
    let mut lines = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        lines += 1;
        let mut parts = line.split_whitespace();
        let Some(hex) = parts.next() else {
            continue;
        };
        if !is_sha256_hex(hex) {
            continue;
        }
        match parts.next() {
            Some(name) => {
                let name = name.trim_start_matches('*');
                let base = std::path::Path::new(name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(name);
                if base == want || name == asset {
                    return Some(hex.to_ascii_lowercase());
                }
            }
            None => lone = Some(hex.to_ascii_lowercase()),
        }
    }
    if lines == 1 { lone } else { None }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Download a URL into memory (artifacts are typically < 200 MiB).
pub async fn download(url: &str) -> Result<Vec<u8>> {
    download_with_bearer(url, None).await
}

/// Total timeout for small catalog/API requests made through `http_client`.
const HTTP_TIMEOUT_SECS: u64 = 20;
/// Downloads have no total limit (bottles can be hundreds of MiB); they fail
/// when the connection cannot be made or stalls for this long between reads.
const CONNECT_TIMEOUT_SECS: u64 = 20;
const READ_IDLE_TIMEOUT_SECS: u64 = 60;

pub async fn download_with_bearer(url: &str, bearer: Option<&str>) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(format!("maca/{}", crate::VERSION))
        .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .read_timeout(std::time::Duration::from_secs(READ_IDLE_TIMEOUT_SECS))
        .build()
        .map_err(|e| Error::Network(e.to_string()))?;
    let mut req = client.get(url);
    if let Some(tok) = bearer {
        req = req.bearer_auth(tok);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Network(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Network(format!(
            "GET {url} -> HTTP {}",
            resp.status().as_u16()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    Ok(bytes.to_vec())
}

/// True when `s` is safe to use as a single path component (a package name,
/// version or executable name): exactly one normal component, no separators,
/// no control characters. Rejects ``, `.`, `..`, `a/b`, `/abs` and `C:\x`.
pub fn valid_component(s: &str) -> bool {
    if s.is_empty() || s.contains(['/', '\\']) || s.chars().any(|c| c.is_control()) {
        return false;
    }
    let mut comps = Path::new(s).components();
    matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(c)), None) if c == std::ffi::OsStr::new(s)
    )
}

fn require_component(what: &str, s: &str) -> Result<()> {
    if valid_component(s) {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "{what} {s:?} is not a valid path component"
        )))
    }
}

/// Write `bytes` to `path` through a temp file in the same directory plus a
/// rename, so a crash mid-write never leaves a truncated state file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let tmp = dir.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A private staging directory under `<prefix>/cellar`, unique per call so
/// concurrent installs never share or delete each other's work. Removed on
/// drop; the name matches the `.tmp-*` sweep in `maca clean`.
struct Staging(PathBuf);

impl Staging {
    fn new(prefix: &Path, name: &str, version: &str) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = config::cellar_dir(prefix).join(format!(
            ".tmp-{name}-{version}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unpack_tar_gz(name: &str, bytes: &[u8], into: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(into)
        .map_err(|e| Error::Other(anyhow::anyhow!("untarring {name}: {e}")))
}

/// Extract a `.tar.gz` bottle archive into `<prefix>/cellar/<name>/<version>/`.
/// Returns the cellar path. The keg appears at its final path only after a
/// complete unpack (staged, then renamed), so an existing keg is always whole.
pub fn extract_bottle(name: &str, version: &str, bytes: &[u8], prefix: &Path) -> Result<PathBuf> {
    require_component("package name", name)?;
    require_component("version", version)?;
    config::ensure_dirs(prefix)?;
    let dest = config::cellar_dir(prefix).join(name).join(version);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Bottle layout is `<name>/<version>/...` inside the archive.
    let staging = Staging::new(prefix, name, version)?;
    unpack_tar_gz(name, bytes, &staging.0)?;
    let staged = staging.0.join(name).join(version);
    if !staged.is_dir() {
        return Err(Error::Other(anyhow::anyhow!(
            "bottle for {name} did not contain {name}/{version} at expected path"
        )));
    }
    move_into_place(name, &staged, &dest)
}

/// Extract an arbitrary `.tar.gz` into a fresh `<prefix>/cellar/<name>/<version>/`.
/// A single leading directory (common for release tarballs) is stripped.
/// Staged and renamed like [`extract_bottle`], which also keeps symlinks and
/// permissions exactly as unpacked.
pub fn extract_tar_gz_to_cellar(
    name: &str,
    version: &str,
    bytes: &[u8],
    prefix: &Path,
) -> Result<PathBuf> {
    require_component("package name", name)?;
    require_component("version", version)?;
    config::ensure_dirs(prefix)?;
    let dest = config::cellar_dir(prefix).join(name).join(version);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = Staging::new(prefix, name, version)?;
    unpack_tar_gz(name, bytes, &staging.0)?;
    let mut top: Vec<PathBuf> = std::fs::read_dir(&staging.0)?
        .flatten()
        .map(|e| e.path())
        .collect();
    let source = match (top.pop(), top.is_empty()) {
        (Some(only), true) if only.is_dir() && !only.is_symlink() => only,
        (None, _) => {
            return Err(Error::Other(anyhow::anyhow!("tarball for {name} is empty")));
        }
        _ => staging.0.clone(),
    };
    move_into_place(name, &source, &dest)
}

/// Extract a `.zip` into a fresh `<prefix>/cellar/<name>/<version>/`.
/// A single leading directory is stripped, matching [`extract_tar_gz_to_cellar`].
/// Entries that escape the archive (`..`, absolute paths) are refused and
/// nothing is published.
pub fn extract_zip_to_cellar(
    name: &str,
    version: &str,
    bytes: &[u8],
    prefix: &Path,
) -> Result<PathBuf> {
    require_component("package name", name)?;
    require_component("version", version)?;
    config::ensure_dirs(prefix)?;
    let dest = config::cellar_dir(prefix).join(name).join(version);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = Staging::new(prefix, name, version)?;
    unpack_zip(name, bytes, &staging.0)?;
    let mut top: Vec<PathBuf> = std::fs::read_dir(&staging.0)?
        .flatten()
        .map(|e| e.path())
        .collect();
    let source = match (top.pop(), top.is_empty()) {
        (Some(only), true) if only.is_dir() && !only.is_symlink() => only,
        (None, _) => {
            return Err(Error::Other(anyhow::anyhow!("zip for {name} is empty")));
        }
        _ => staging.0.clone(),
    };
    move_into_place(name, &source, &dest)
}

fn unpack_zip(name: &str, bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::Other(anyhow::anyhow!("zip for {name}: {e}")))?;
    if archive.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("zip for {name} is empty")));
    }
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| Error::Other(anyhow::anyhow!("zip for {name}: {e}")))?;
        let Some(rel) = file.enclosed_name() else {
            return Err(Error::Unsupported(format!(
                "zip entry in {name} escapes the archive"
            )));
        };
        if rel.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            return Err(Error::Unsupported(format!(
                "zip entry {} escapes the archive",
                rel.display()
            )));
        }
        let out = dest.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut writer = std::fs::File::create(&out)?;
        std::io::copy(&mut file, &mut writer)?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

/// Rename a fully staged keg to its final path. If the rename fails because
/// another task installing the same keg (two packages sharing a dependency)
/// got there first, its complete keg is used and this copy is dropped.
fn move_into_place(name: &str, staged: &Path, dest: &Path) -> Result<PathBuf> {
    match std::fs::rename(staged, dest) {
        Ok(()) => Ok(dest.to_path_buf()),
        Err(_) if dest.is_dir() => Ok(dest.to_path_buf()),
        Err(e) => Err(Error::Other(anyhow::anyhow!(
            "moving {name} into place: {e}"
        ))),
    }
}

/// Make `link` a symlink to `target`, tolerating a concurrent task that just
/// created the same link.
#[cfg(not(windows))]
fn symlink_or_same(target: &Path, link: &Path) -> Result<()> {
    match std::os::unix::fs::symlink(target, link) {
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists
                && std::fs::read_link(link).is_ok_and(|t| t == target) =>
        {
            Ok(())
        }
        other => Ok(other?),
    }
}

/// Put a symlink at `link` pointing to `target` without destroying anything
/// maca does not own. An existing entry is replaced only when it is a symlink
/// that is dangling or points inside `own_root` (this package's own
/// `cellar/<name>` directory, so reinstalls and upgrades relink cleanly).
/// Anything else (another package's link, a user's file) is left alone and
/// `Ok(false)` is returned.
#[cfg(not(windows))]
fn place_link(target: &Path, link: &Path, own_root: &Path) -> Result<bool> {
    if let Ok(meta) = link.symlink_metadata() {
        let replaceable = meta.file_type().is_symlink()
            && match std::fs::read_link(link) {
                Ok(existing) => existing.starts_with(own_root) || !link.exists(),
                Err(_) => false,
            };
        if !replaceable {
            eprintln!(
                "maca: not linking {}: it already exists and belongs to something else",
                link.display()
            );
            return Ok(false);
        }
        match std::fs::remove_file(link) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    symlink_or_same(target, link)?;
    Ok(true)
}

/// Windows has no unprivileged symlinks, so entries are copies. A copy
/// carries no record of which package made it, so on Windows an existing
/// entry is replaced; see the README's platform notes.
#[cfg(windows)]
fn place_link(target: &Path, link: &Path, _own_root: &Path) -> Result<bool> {
    if link.exists() {
        let _ = std::fs::remove_file(link);
    }
    std::fs::copy(target, link)?;
    Ok(true)
}

/// Windows: true when `copy` is a regular file with the same bytes as
/// `original`, i.e. the copy maca made of it.
#[cfg(windows)]
pub(crate) fn is_copy_of(copy: &Path, original: &Path) -> bool {
    match (std::fs::metadata(copy), std::fs::metadata(original)) {
        (Ok(a), Ok(b)) if a.is_file() && b.is_file() && a.len() == b.len() => {
            matches!((std::fs::read(copy), std::fs::read(original)), (Ok(x), Ok(y)) if x == y)
        }
        _ => false,
    }
}

/// `cellar/<name>` for a keg at `cellar/<name>/<version>`.
fn own_root(keg: &Path) -> &Path {
    keg.parent().unwrap_or(keg)
}

/// Create `<prefix>/opt/<name>` → `<cellar>/<name>/<version>` (Homebrew layout).
pub fn link_opt(name: &str, version: &str, cellar: &Path, prefix: &Path) -> Result<PathBuf> {
    if name.contains('/') || name.is_empty() {
        return Ok(cellar.to_path_buf());
    }
    let opt_dir = prefix.join("opt");
    std::fs::create_dir_all(&opt_dir)?;
    let link = opt_dir.join(name);
    if link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&link);
    }
    // Target is the cellar keg path (not the generic `cellar/<name>/<version>`
    // reconstruction) so callers can pass the real extracted path.
    #[cfg(windows)]
    {
        // Windows: create a directory junction is overkill; copy is wrong for
        // a symlink. Use a directory symlink when possible.
        let _ = std::os::windows::fs::symlink_dir(cellar, &link);
    }
    #[cfg(not(windows))]
    {
        symlink_or_same(cellar, &link)?;
    }
    let _ = version;
    Ok(link)
}

/// Symlink `<cellar>/bin/<exe>` (and any discovered binaries or sbin entries)
/// into `<prefix>/bin/<exe>`. On Windows, copy instead (symlinks need privileges).
pub fn link_bins(cellar: &Path, executables: &[String], prefix: &Path) -> Result<Vec<PathBuf>> {
    config::ensure_dirs(prefix)?;
    let bin_dir = config::bin_dir(prefix);
    let mut targets: Vec<(PathBuf, String)> = Vec::new();

    if executables.is_empty() {
        let bin = cellar.join("bin");
        if bin.exists() {
            for name in discover_binaries(&bin) {
                targets.push((bin.join(&name), name));
            }
        }
    } else {
        for exe in executables {
            if exe.contains('/') || exe.contains('\\') || exe.is_empty() {
                continue;
            }
            let target = cellar.join("bin").join(exe);
            if target.exists() {
                targets.push((target, exe.clone()));
            }
        }
    }

    // Also check sbin if present
    let sbin = cellar.join("sbin");
    if sbin.exists() {
        for name in discover_binaries(&sbin) {
            if !targets.iter().any(|(_, n)| n == &name) {
                targets.push((sbin.join(&name), name));
            }
        }
    }

    let mut linked = Vec::new();
    for (target, exe) in targets {
        let link = bin_dir.join(&exe);
        if place_link(&target, &link, own_root(cellar))? {
            linked.push(link);
        }
    }
    Ok(linked)
}

fn discover_binaries(dir: &Path) -> Vec<String> {
    let mut exes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type()
                && (ft.is_file() || ft.is_symlink())
                && let Some(name) = entry.file_name().to_str()
                && !name.starts_with('.')
            {
                exes.push(name.to_string());
            }
        }
    }
    exes.sort();
    exes
}

/// Recursively symlink documentation and share assets (`man`, `info`, completions)
/// from `<cellar>/share` into `<prefix>/share`.
pub fn link_share(cellar: &Path, prefix: &Path) -> Result<Vec<PathBuf>> {
    let cellar_share = cellar.join("share");
    if !cellar_share.exists() {
        return Ok(vec![]);
    }
    let prefix_share = prefix.join("share");
    let mut linked = Vec::new();
    let mut stack = vec![cellar_share.clone()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if let Ok(rel) = path.strip_prefix(&cellar_share) {
                let dest = prefix_share.join(rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if place_link(&path, &dest, own_root(cellar))? {
                    linked.push(dest);
                }
            }
        }
    }
    Ok(linked)
}

/// Windows: share entries are copies, so remove each `<prefix>/share/<rel>`
/// that is still an exact copy of `<cellar>/share/<rel>`.
#[cfg(windows)]
pub fn unlink_share(cellar: &Path, prefix: &Path) -> Result<usize> {
    let cellar_share = cellar.join("share");
    let prefix_share = prefix.join("share");
    let mut count = 0;
    let mut stack = vec![cellar_share.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(&cellar_share) {
                let dest = prefix_share.join(rel);
                if is_copy_of(&dest, &path) && std::fs::remove_file(&dest).is_ok() {
                    count += 1;
                    let mut parent = dest.parent();
                    while let Some(p) = parent.filter(|p| *p != prefix_share) {
                        if std::fs::remove_dir(p).is_err() {
                            break;
                        }
                        parent = p.parent();
                    }
                }
            }
        }
    }
    Ok(count)
}

/// Remove symlinks under `<prefix>/share` that point into `<cellar>`.
#[cfg(not(windows))]
pub fn unlink_share(cellar: &Path, prefix: &Path) -> Result<usize> {
    let prefix_share = prefix.join("share");
    if !prefix_share.exists() {
        return Ok(0);
    }
    let mut count = 0;
    let mut stack = vec![prefix_share.clone()];
    let mut dirs_to_prune = Vec::new();

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                dirs_to_prune.push(path.clone());
                stack.push(path);
                continue;
            }
            if let Ok(target) = std::fs::read_link(&path)
                && target.starts_with(cellar)
            {
                let _ = std::fs::remove_file(&path);
                count += 1;
            }
        }
    }

    // Prune empty directories in reverse traversal order
    dirs_to_prune.reverse();
    for dir in dirs_to_prune {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(count)
}

/// Create `<prefix>/bin` symlink for a single-binary custom install.
pub fn link_single_binary(binary: &Path, name: &str, prefix: &Path) -> Result<PathBuf> {
    config::ensure_dirs(prefix)?;
    let bin_dir = config::bin_dir(prefix);
    require_component("binary name", name)?;
    let link = bin_dir.join(name);
    let keg = binary.parent().unwrap_or(binary);
    if !place_link(binary, &link, own_root(keg))? {
        return Err(Error::Other(anyhow::anyhow!(
            "{} already exists and is not owned by {name}",
            link.display()
        )));
    }
    Ok(link)
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("maca/{}", crate::VERSION))
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| Error::Network(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_abc() {
        // Known vector: sha256("abc").
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_rejects_mismatch() {
        let err = verify_sha256("x", b"abc", Some("deadbeef")).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn link_bins_rejects_traversal() {
        let temp = std::env::temp_dir().join(format!("maca-test-trav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let prefix = temp.join("prefix");
        let cellar = config::cellar_dir(&prefix).join("pkg/1.0");
        std::fs::create_dir_all(cellar.join("bin/a")).unwrap();
        std::fs::write(cellar.join("bin/real"), b"x").unwrap();
        std::fs::write(cellar.join("evil"), b"x").unwrap(); // target of ../evil
        std::fs::write(cellar.join("bin/a/b"), b"x").unwrap();
        let exes = [
            "../evil".to_string(),
            "a/b".into(),
            String::new(),
            "real".into(),
        ];
        let linked = link_bins(&cellar, &exes, &prefix).unwrap();
        assert_eq!(linked, vec![config::bin_dir(&prefix).join("real")]);
        assert!(!config::bin_dir(&prefix).join("evil").exists());
        assert!(!config::bin_dir(&prefix).join("b").exists());
        assert!(!prefix.join("evil").exists());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn link_bins_fallback_discovers_unlisted_binaries() {
        let temp = std::env::temp_dir().join(format!("maca-test-bins-{}", std::process::id()));
        let cellar = temp.join("cellar/pkg/1.0.0");
        let prefix = temp.join("prefix");
        let bin_dir = cellar.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("cmd_a"), b"echo a").unwrap();
        std::fs::write(bin_dir.join("cmd_b"), b"echo b").unwrap();

        let linked = link_bins(&cellar, &[], &prefix).unwrap();
        assert_eq!(linked.len(), 2);
        assert!(prefix.join("bin/cmd_a").exists());
        assert!(prefix.join("bin/cmd_b").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn share_linking_and_unlinking_roundtrip() {
        let temp = std::env::temp_dir().join(format!("maca-test-share-{}", std::process::id()));
        let cellar = temp.join("cellar/pkg/1.0.0");
        let prefix = temp.join("prefix");
        let man_dir = cellar.join("share/man/man1");
        std::fs::create_dir_all(&man_dir).unwrap();
        std::fs::write(man_dir.join("foo.1"), b"man foo").unwrap();

        let linked = link_share(&cellar, &prefix).unwrap();
        assert_eq!(linked.len(), 1);
        assert!(prefix.join("share/man/man1/foo.1").exists());

        let unlinked = unlink_share(&cellar, &prefix).unwrap();
        assert_eq!(unlinked, 1);
        assert!(!prefix.join("share/man/man1/foo.1").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("maca-ut-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a tar.gz from `(path, contents)` files and `(path, target)` symlinks.
    fn tar_gz(files: &[(&str, &[u8])], links: &[(&str, &str)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (path, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, path, *data).unwrap();
        }
        for (path, target) in links {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            tar.append_link(&mut h, path, target).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn valid_component_rejects_traversal() {
        for ok in ["jq", "1.8.2", "1.2.3+build", "v1.0_1", "node@20"] {
            assert!(valid_component(ok), "{ok} should be accepted");
        }
        for bad in [
            "", ".", "..", "../evil", "a/b", "/abs", "a\\b", "x\n", "a\0b",
        ] {
            assert!(!valid_component(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn extraction_refuses_traversing_names() {
        let prefix = temp("trav");
        let bytes = tar_gz(&[("pkg/bin/tool", b"x")], &[]);
        assert!(extract_tar_gz_to_cellar("../evil", "1.0", &bytes, &prefix).is_err());
        assert!(extract_tar_gz_to_cellar("ok", "../../evil", &bytes, &prefix).is_err());
        assert!(extract_bottle("a/b", "1.0", &bytes, &prefix).is_err());
        assert!(!prefix.parent().unwrap().join("evil").exists());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[cfg(unix)]
    #[test]
    fn tarball_symlinks_survive_and_single_top_dir_is_stripped() {
        let prefix = temp("symlink");
        let bytes = tar_gz(
            &[
                ("pkg-1.0/lib/libfoo.so.1", b"lib"),
                ("pkg-1.0/bin/tool", b"#!/bin/sh\n"),
            ],
            &[
                ("pkg-1.0/lib/libfoo.so", "libfoo.so.1"),
                ("pkg-1.0/dangling", "nowhere"),
            ],
        );
        let keg = extract_tar_gz_to_cellar("pkg", "1.0", &bytes, &prefix).unwrap();
        assert!(keg.join("bin/tool").is_file());
        let link = keg.join("lib/libfoo.so");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("libfoo.so.1")
        );
        assert!(keg.join("dangling").symlink_metadata().is_ok());
        // No staging directory is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(config::cellar_dir(&prefix))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "staging dir leaked: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn failed_extraction_leaves_no_keg() {
        let prefix = temp("partial");
        // Truncated gzip stream: unpack fails part-way.
        let mut bytes = tar_gz(&[("pkg/bin/tool", &[7u8; 4096])], &[]);
        bytes.truncate(bytes.len() / 2);
        assert!(extract_tar_gz_to_cellar("pkg", "1.0", &bytes, &prefix).is_err());
        assert!(!config::cellar_dir(&prefix).join("pkg/1.0").exists());
        // A bottle without the expected <name>/<version> layout also leaves nothing.
        let wrong = tar_gz(&[("other/9/bin/x", b"x")], &[]);
        assert!(extract_bottle("pkg", "1.0", &wrong, &prefix).is_err());
        assert!(!config::cellar_dir(&prefix).join("pkg/1.0").exists());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[cfg(unix)]
    #[test]
    fn links_never_clobber_foreign_files_or_other_packages() {
        let prefix = temp("clobber");
        let a = config::cellar_dir(&prefix).join("a/1.0");
        let b = config::cellar_dir(&prefix).join("b/1.0");
        for keg in [&a, &b] {
            std::fs::create_dir_all(keg.join("bin")).unwrap();
            std::fs::write(keg.join("bin/tool"), b"x").unwrap();
            std::fs::write(keg.join("bin/mine"), b"x").unwrap();
        }
        std::fs::create_dir_all(config::bin_dir(&prefix)).unwrap();
        let user_file = config::bin_dir(&prefix).join("mine");
        std::fs::write(&user_file, b"user data").unwrap();

        // A links `tool`; the user's regular file `mine` is left untouched.
        let linked = link_bins(&a, &[], &prefix).unwrap();
        assert_eq!(linked, vec![config::bin_dir(&prefix).join("tool")]);
        assert_eq!(std::fs::read(&user_file).unwrap(), b"user data");

        // B must not steal A's link.
        assert!(link_bins(&b, &[], &prefix).unwrap().is_empty());
        let tool = config::bin_dir(&prefix).join("tool");
        assert!(std::fs::read_link(&tool).unwrap().starts_with(&a));

        // A new version of A may replace A's own link.
        let a2 = config::cellar_dir(&prefix).join("a/2.0");
        std::fs::create_dir_all(a2.join("bin")).unwrap();
        std::fs::write(a2.join("bin/tool"), b"x").unwrap();
        assert_eq!(link_bins(&a2, &[], &prefix).unwrap().len(), 1);
        assert!(std::fs::read_link(&tool).unwrap().starts_with(&a2));
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = temp("atomic");
        let path = dir.join("state.json");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_checksum_is_not_a_mismatch_but_wrong_one_is() {
        assert!(verify_sha256("x", b"abc", None).is_ok());
        assert!(verify_sha256("x", b"abc", Some(&sha256_hex(b"abc"))).is_ok());
        assert!(matches!(
            verify_sha256("x", b"abc", Some("00")),
            Err(Error::Checksum { .. })
        ));
    }

    #[test]
    fn zip_slip_leaves_nothing_outside_the_archive() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            writer.start_file("../evil.txt", opts).unwrap();
            use std::io::Write;
            writer.write_all(b"nope").unwrap();
            writer.finish().unwrap();
        }
        let prefix = temp("zip-slip");
        let outside = prefix.join("evil.txt");
        let err = extract_zip_to_cellar("pkg", "1.0.0", &cursor.into_inner(), &prefix);
        assert!(err.is_err(), "zip-slip must fail");
        assert!(!outside.exists());
        assert!(!config::cellar_dir(&prefix).join("pkg/1.0.0").exists());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn zip_extracts_and_strips_one_leading_directory() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            writer.start_file("hello-1.0.0/bin/hello", opts).unwrap();
            use std::io::Write;
            writer.write_all(b"hi").unwrap();
            writer.finish().unwrap();
        }
        let prefix = temp("zip-ok");
        let dest = extract_zip_to_cellar("pkg", "1.0.0", &cursor.into_inner(), &prefix).unwrap();
        assert_eq!(std::fs::read(dest.join("bin/hello")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&prefix);
    }
}
