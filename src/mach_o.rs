//! Mach-O placeholder rewriting for Homebrew bottles.
//!
//! Homebrew bottles are built with two placeholder tokens that the `brew`
//! installer substitutes at install time:
//! - `@@HOMEBREW_CELLAR@@` → `<prefix>/Cellar`
//! - `@@HOMEBREW_PREFIX@@`  → `<prefix>`
//!
//! Maca performs the same substitution with `install_name_tool` so bottle
//! binaries and dylibs resolve inside the maca prefix. On non-macOS hosts
//! this module is a no-op (Linux bottles are relocatable tarballs).

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "macos")]
use crate::Error;
use crate::Result;

// The placeholder helpers below are pure string and file logic, kept on every
// platform so their unit tests run in Linux CI; only macOS calls them.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const CELLAR_TOKEN: &str = "@@HOMEBREW_CELLAR@@";
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PREFIX_TOKEN: &str = "@@HOMEBREW_PREFIX@@";

/// Walk `cellar` and rewrite Homebrew placeholders in every Mach-O file.
/// Returns the number of files rewritten.
pub fn rewrite_placeholders(cellar: &Path, prefix: &Path) -> Result<usize> {
    #[cfg(target_os = "macos")]
    {
        rewrite_placeholders_macos(cellar, prefix)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (cellar, prefix);
        Ok(0)
    }
}

#[cfg(target_os = "macos")]
fn rewrite_placeholders_macos(cellar: &Path, prefix: &Path) -> Result<usize> {
    let cellar_dir = cellar_canonical(prefix);
    let mut rewritten = 0usize;
    let mut stack = vec![cellar.to_path_buf()];
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
            if !is_macho(&path) {
                continue;
            }
            if rewrite_one(&path, prefix, &cellar_dir)? {
                rewritten += 1;
            }
        }
    }
    Ok(rewritten)
}

/// Homebrew's Cellar is `<prefix>/Cellar`; maca uses `<prefix>/cellar`.
/// Bottles reference the Cellar token, so map it to our lowercase cellar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn cellar_canonical(prefix: &Path) -> PathBuf {
    prefix.join("cellar")
}

#[cfg(target_os = "macos")]
fn rewrite_one(path: &Path, prefix: &Path, cellar_dir: &Path) -> Result<bool> {
    let mut changed = false;
    // 1. Rewrite the dylib's own install name(s).
    if let Ok(out) = Command::new("otool").arg("-D").arg(path).output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let old = line.trim();
            if old.is_empty() {
                continue;
            }
            if let Some(new) = substitute(old, prefix, cellar_dir) {
                let status = Command::new("install_name_tool")
                    .args(["-id", &new])
                    .arg(path)
                    .status();
                match status {
                    Ok(s) if s.success() => changed = true,
                    Ok(s) => {
                        tracing::warn!("install_name_tool -id on {} failed: {s}", path.display());
                    }
                    Err(e) => {
                        return Err(Error::Other(anyhow::anyhow!("install_name_tool: {e}")));
                    }
                }
            }
        }
    }
    // 2. Rewrite dependent library references.
    if let Ok(out) = Command::new("otool").arg("-L").arg(path).output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let old = trimmed.split_whitespace().next().unwrap_or("").to_string();
            if old.is_empty() {
                continue;
            }
            if let Some(new) = substitute(&old, prefix, cellar_dir) {
                let status = Command::new("install_name_tool")
                    .args(["-change", &old, &new])
                    .arg(path)
                    .status();
                match status {
                    Ok(s) if s.success() => changed = true,
                    Ok(s) => {
                        tracing::warn!(
                            "install_name_tool -change on {} failed: {s}",
                            path.display()
                        );
                    }
                    Err(e) => {
                        return Err(Error::Other(anyhow::anyhow!("install_name_tool: {e}")));
                    }
                }
            }
        }
    }
    if changed {
        // Modifying a Mach-O invalidates its code signature; on Apple Silicon
        // every binary must carry at least an ad-hoc signature to run.
        match Command::new("codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(path)
            .status()
        {
            Ok(s) if s.success() => {}
            Ok(s) => {
                tracing::warn!("codesign on {} failed: {s}", path.display());
            }
            Err(e) => {
                tracing::warn!("could not run codesign on {}: {e}", path.display());
            }
        }
    }
    Ok(changed)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn substitute(old: &str, prefix: &Path, cellar_dir: &Path) -> Option<String> {
    if !old.contains(CELLAR_TOKEN) && !old.contains(PREFIX_TOKEN) {
        return None;
    }
    let mut s = old.replace(CELLAR_TOKEN, &cellar_dir.to_string_lossy());
    s = s.replace(PREFIX_TOKEN, &prefix.to_string_lossy());
    Some(s)
}

/// Detect a Mach-O file by its magic bytes (thin or fat, 32/64-bit).
/// Mach-O magic is stored in the file's native endianness, so accept both
/// byte orders for each magic value.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_macho(path: &Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    let be = u32::from_be_bytes(magic);
    let le = u32::from_le_bytes(magic);
    const MAGICS: [u32; 8] = [
        0xfeedface, // MH_MAGIC
        0xfeedfacf, // MH_MAGIC_64
        0xcafebabe, // FAT_MAGIC
        0xcafebabf, // FAT_MAGIC_64
        0xcefaedfe, // MH_CIGAM
        0xcffaedfe, // MH_CIGAM_64
        0xbebafeca, // FAT_CIGAM
        0xbfbafeca, // FAT_CIGAM_64
    ];
    MAGICS.contains(&be) || MAGICS.contains(&le)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_both_tokens() {
        let prefix = Path::new("/opt/maca");
        let cellar = Path::new("/opt/maca/cellar");
        let old = "@@HOMEBREW_CELLAR@@/jq/1.8.2/lib/libjq.1.dylib";
        assert_eq!(
            substitute(old, prefix, cellar).as_deref(),
            Some("/opt/maca/cellar/jq/1.8.2/lib/libjq.1.dylib")
        );
        let old2 = "@@HOMEBREW_PREFIX@@/opt/oniguruma/lib/libonig.5.dylib";
        assert_eq!(
            substitute(old2, prefix, cellar).as_deref(),
            Some("/opt/maca/opt/oniguruma/lib/libonig.5.dylib")
        );
    }

    #[test]
    fn substitute_leaves_plain_paths_alone() {
        assert!(
            substitute(
                "/usr/lib/libSystem.B.dylib",
                Path::new("/p"),
                Path::new("/p/cellar")
            )
            .is_none()
        );
    }
}
