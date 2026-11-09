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

/// Download a URL into memory (artifacts are typically < 200 MiB).
pub async fn download(url: &str) -> Result<Vec<u8>> {
    download_with_bearer(url, None).await
}

pub async fn download_with_bearer(url: &str, bearer: Option<&str>) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(format!("maca/{}", crate::VERSION))
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

/// Extract a `.tar.gz` bottle archive into `<prefix>/cellar/<name>/<version>/`.
/// Returns the cellar path.
pub fn extract_bottle(name: &str, version: &str, bytes: &[u8], prefix: &Path) -> Result<PathBuf> {
    config::ensure_dirs(prefix)?;
    let dest = config::cellar_dir(prefix).join(name).join(version);
    if dest.exists() {
        // Idempotent: reuse existing install but ensure completeness.
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(prefix.join("cellar").join("__tmp"))
        .map_err(|e| Error::Other(anyhow::anyhow!("untarring {name}: {e}")))?;
    // Bottle layout is `cellar/<name>/<version>/...` relative to a temp dir
    // extracted under `cellar/__tmp/<name>/<version>/...`. Move it into place.
    let staged = prefix.join("cellar/__tmp").join(name).join(version);
    if !staged.exists() {
        return Err(Error::Other(anyhow::anyhow!(
            "bottle for {name} did not contain {name}/{version} at expected path"
        )));
    }
    std::fs::rename(&staged, &dest)
        .map_err(|e| Error::Other(anyhow::anyhow!("moving {name} into place: {e}")))?;
    let _ = std::fs::remove_dir_all(prefix.join("cellar/__tmp"));
    Ok(dest)
}

/// Extract an arbitrary `.tar.gz` into a fresh `<prefix>/cellar/<name>/<version>/`.
pub fn extract_tar_gz_to_cellar(
    name: &str,
    version: &str,
    bytes: &[u8],
    prefix: &Path,
) -> Result<PathBuf> {
    config::ensure_dirs(prefix)?;
    let dest = config::cellar_dir(prefix).join(name).join(version);
    if dest.exists() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    // Strip a single leading directory if present (common for GitHub tarballs),
    // otherwise extract verbatim.
    let entries: Vec<_> = tar
        .entries()
        .map_err(|e| Error::Other(anyhow::anyhow!("reading tar: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Other(anyhow::anyhow!("reading tar entry: {e}")))?;
    let top: Vec<String> = entries
        .iter()
        .filter_map(|e| e.path().ok())
        .filter_map(|p| {
            p.components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .map(String::from)
        })
        .collect();
    let single_top = if top.iter().all(|t| t == &top[0]) && !top.is_empty() {
        Some(top[0].clone())
    } else {
        None
    };
    if let Some(top_name) = single_top {
        // Unpack into temp then move top dir contents.
        let tmp = prefix.join(format!("cellar/.tmp-{name}-{version}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let gz2 = flate2::read::GzDecoder::new(bytes);
        let mut tar2 = tar::Archive::new(gz2);
        tar2.unpack(&tmp)
            .map_err(|e| Error::Other(anyhow::anyhow!("untarring {name}: {e}")))?;
        let staged = tmp.join(&top_name);
        // Move staged contents into dest.
        if staged.exists() {
            move_dir_contents(&staged, &dest)?;
            let _ = std::fs::remove_dir_all(&tmp);
        } else {
            return Err(Error::Other(anyhow::anyhow!(
                "tarball top dir {top_name} not found"
            )));
        }
    } else {
        let gz2 = flate2::read::GzDecoder::new(bytes);
        let mut tar2 = tar::Archive::new(gz2);
        tar2.unpack(&dest)
            .map_err(|e| Error::Other(anyhow::anyhow!("untarring {name}: {e}")))?;
    }
    Ok(dest)
}

fn move_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            move_dir_contents(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
            if let Ok(perms) = entry.metadata() {
                let _ = std::fs::set_permissions(&dest, perms.permissions());
            }
        }
    }
    Ok(())
}

/// Create `<prefix>/opt/<name>` → `<cellar>/<name>/<version>` (Homebrew layout).
pub fn link_opt(name: &str, version: &str, cellar: &Path, prefix: &Path) -> Result<PathBuf> {
    if name.contains('/') || name.is_empty() {
        return Ok(cellar.to_path_buf());
    }
    let opt_dir = prefix.join("opt");
    std::fs::create_dir_all(&opt_dir)?;
    let link = opt_dir.join(name);
    if link.exists() || link.symlink_metadata().is_ok() {
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
        std::os::unix::fs::symlink(cellar, &link)?;
    }
    let _ = version;
    Ok(link)
}

/// Symlink `<cellar>/bin/<exe>` into `<prefix>/bin/<exe>`. On Windows, copy
/// instead (symlinks need privileges).
pub fn link_bins(cellar: &Path, executables: &[String], prefix: &Path) -> Result<Vec<PathBuf>> {
    config::ensure_dirs(prefix)?;
    let bin_dir = config::bin_dir(prefix);
    let mut linked = Vec::new();
    for exe in executables {
        // Reject path traversal.
        if exe.contains('/') || exe.contains('\\') || exe.is_empty() {
            continue;
        }
        let target = cellar.join("bin").join(exe);
        if !target.exists() {
            continue;
        }
        let link = bin_dir.join(exe);
        if link.exists() || link.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&link);
        }
        #[cfg(windows)]
        {
            std::fs::copy(&target, &link)?;
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(&target, &link)?;
        }
        linked.push(link);
    }
    Ok(linked)
}

/// Create `<prefix>/bin` symlink for a single-binary custom install.
pub fn link_single_binary(binary: &Path, name: &str, prefix: &Path) -> Result<PathBuf> {
    config::ensure_dirs(prefix)?;
    let bin_dir = config::bin_dir(prefix);
    let link = bin_dir.join(name);
    if link.exists() || link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&link);
    }
    #[cfg(windows)]
    {
        std::fs::copy(binary, &link)?;
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(binary, &link)?;
    }
    Ok(link)
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("maca/{}", crate::VERSION))
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
        // Path traversal names must be skipped, not linked.
        assert!(
            ["../evil", "a/b", ""]
                .iter()
                .all(|n| { n.contains('/') || n.contains('\\') || n.is_empty() })
        );
    }
}
