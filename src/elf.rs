//! ELF placeholder rewriting for Homebrew bottles on Linux.
//!
//! Linux bottles carry the same `@@HOMEBREW_PREFIX@@` / `@@HOMEBREW_CELLAR@@`
//! tokens as macOS ones, in two places the dynamic loader reads: the program
//! interpreter (`@@HOMEBREW_PREFIX@@/lib/ld.so`) and the library search path
//! (`RPATH`/`RUNPATH`). Left alone, the kernel cannot even start the binary
//! ("not found"). `brew` rewrites both at install time; maca does the same
//! with `patchelf`, the way `mach_o.rs` uses `install_name_tool` on macOS.
//!
//! The interpreter becomes the host's own loader (the one this `maca` binary
//! runs under) and the search path points into the maca prefix and cellar.
//! A bottle that needs this and a host without `patchelf` is an error, not a
//! keg full of binaries that cannot run. On other hosts this is a no-op.

use std::path::Path;

use crate::Result;

// Pure helpers are kept on every platform so their unit tests run everywhere;
// only Linux calls them.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const TOKEN_MARK: &str = "@@HOMEBREW_";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CELLAR_TOKEN: &str = "@@HOMEBREW_CELLAR@@";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PREFIX_TOKEN: &str = "@@HOMEBREW_PREFIX@@";

/// Rewrite Homebrew placeholders in every ELF file under `keg`. Returns the
/// number of files rewritten.
pub fn rewrite_placeholders(keg: &Path, prefix: &Path, cellar_dir: &Path) -> Result<usize> {
    #[cfg(target_os = "linux")]
    {
        linux::rewrite(keg, prefix, cellar_dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (keg, prefix, cellar_dir);
        Ok(0)
    }
}

/// Substitute the tokens in an interpreter or search path. Homebrew's Cellar
/// is `<prefix>/Cellar`; maca's is `cellar_dir` (lowercase, and possibly
/// shared outside the prefix), so both spellings map there.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn relocate(value: &str, prefix: &Path, cellar_dir: &Path) -> Option<String> {
    if !value.contains(TOKEN_MARK) {
        return None;
    }
    let prefix = prefix.to_string_lossy();
    let cellar = cellar_dir.to_string_lossy();
    Some(
        value
            .replace(&format!("{PREFIX_TOKEN}/Cellar"), &cellar)
            .replace(CELLAR_TOKEN, &cellar)
            .replace(PREFIX_TOKEN, &prefix),
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

/// True for an ELF file that still holds a Homebrew token anywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn needs_relocation(bytes: &[u8]) -> bool {
    is_elf(bytes)
        && bytes
            .windows(TOKEN_MARK.len())
            .any(|w| w == TOKEN_MARK.as_bytes())
}

/// The program interpreter (`PT_INTERP`) of a 64-bit little-endian ELF file,
/// which is every Linux target maca builds for. `None` for anything else, a
/// file without one (a shared library), or a truncated header.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn interpreter(bytes: &[u8]) -> Option<String> {
    const PT_INTERP: u32 = 3;
    if !is_elf(bytes) || bytes.get(4) != Some(&2) || bytes.get(5) != Some(&1) {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    let phoff = usize::try_from(u64_at(0x20)?).ok()?;
    let phentsize = usize::from(u16_at(0x36)?);
    let phnum = usize::from(u16_at(0x38)?);
    for index in 0..phnum {
        let at = phoff.checked_add(index.checked_mul(phentsize)?)?;
        if u32_at(at)? != PT_INTERP {
            continue;
        }
        let offset = usize::try_from(u64_at(at.checked_add(0x08)?)?).ok()?;
        let size = usize::try_from(u64_at(at.checked_add(0x20)?)?).ok()?;
        let raw = bytes.get(offset..offset.checked_add(size)?)?;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        return String::from_utf8(raw[..end].to_vec()).ok();
    }
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{TOKEN_MARK, interpreter, needs_relocation, relocate};
    use crate::{Error, Result};

    pub(super) fn rewrite(keg: &Path, prefix: &Path, cellar_dir: &Path) -> Result<usize> {
        let targets = files_needing_relocation(keg);
        if targets.is_empty() {
            return Ok(0);
        }
        let patchelf = find_on_path("patchelf").ok_or_else(|| {
            Error::Unsupported(format!(
                "{} ELF file(s) in {} must be relocated before they can run, and that needs \
                 `patchelf` on PATH (for example `apt install patchelf`); alternatively \
                 install with --build-from-source or from another backend",
                targets.len(),
                keg.display()
            ))
        })?;
        let loader = host_loader().ok_or_else(|| {
            Error::Unsupported(
                "cannot tell which dynamic loader this host uses (maca itself has no ELF \
                 interpreter), so a Homebrew bottle cannot be relocated"
                    .into(),
            )
        })?;
        let mut rewritten = 0usize;
        for (path, old_interpreter) in targets {
            let mode = std::fs::metadata(&path)?.permissions().mode();
            // Bottle files are read-only; patchelf rewrites in place.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode | 0o200))?;
            let outcome = rewrite_one(
                &patchelf,
                &path,
                old_interpreter.as_deref(),
                &loader,
                prefix,
                cellar_dir,
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            if outcome? {
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Regular ELF files under `keg` that still hold a token, with the
    /// interpreter each one names. Symlinks are skipped: their targets are
    /// visited as files, or lie outside the keg.
    fn files_needing_relocation(keg: &Path) -> Vec<(PathBuf, Option<String>)> {
        let mut found = Vec::new();
        let mut stack = vec![keg.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file()
                    && let Ok(bytes) = std::fs::read(&path)
                    && needs_relocation(&bytes)
                {
                    found.push((path, interpreter(&bytes)));
                }
            }
        }
        found.sort();
        found
    }

    fn rewrite_one(
        patchelf: &Path,
        path: &Path,
        old_interpreter: Option<&str>,
        loader: &str,
        prefix: &Path,
        cellar_dir: &Path,
    ) -> Result<bool> {
        let mut changed = false;
        if old_interpreter.is_some_and(|i| i.contains(TOKEN_MARK)) {
            run(patchelf, &["--set-interpreter", loader], path)?;
            changed = true;
        }
        let rpath = run(patchelf, &["--print-rpath"], path)?;
        if let Some(new) = relocate(rpath.trim(), prefix, cellar_dir) {
            run(patchelf, &["--set-rpath", &new], path)?;
            changed = true;
        }
        Ok(changed)
    }

    fn run(patchelf: &Path, args: &[&str], path: &Path) -> Result<String> {
        let out = Command::new(patchelf)
            .args(args)
            .arg(path)
            .output()
            .map_err(|e| Error::Other(anyhow::anyhow!("could not run patchelf: {e}")))?;
        if !out.status.success() {
            return Err(Error::Other(anyhow::anyhow!(
                "patchelf {} on {} failed: {}",
                args.first().copied().unwrap_or_default(),
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The loader this process runs under, which is by definition one the
    /// host can use.
    fn host_loader() -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        interpreter(&std::fs::read(exe).ok()?).filter(|i| Path::new(i).exists())
    }

    fn find_on_path(program: &str) -> Option<PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64-bit little-endian ELF: header, one program header, and
    /// the interpreter string it points at.
    fn elf_with_interpreter(interp: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x40 + 0x38];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes()); // e_phoff
        bytes[0x36..0x38].copy_from_slice(&0x38u16.to_le_bytes()); // e_phentsize
        bytes[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let offset = bytes.len() as u64;
        let size = interp.len() as u64 + 1;
        bytes[0x40..0x44].copy_from_slice(&3u32.to_le_bytes()); // PT_INTERP
        bytes[0x48..0x50].copy_from_slice(&offset.to_le_bytes()); // p_offset
        bytes[0x60..0x68].copy_from_slice(&size.to_le_bytes()); // p_filesz
        bytes.extend_from_slice(interp.as_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn interpreter_is_read_from_the_program_header() {
        let bytes = elf_with_interpreter("@@HOMEBREW_PREFIX@@/lib/ld.so");
        assert_eq!(
            interpreter(&bytes).as_deref(),
            Some("@@HOMEBREW_PREFIX@@/lib/ld.so")
        );
        assert!(needs_relocation(&bytes));
        let plain = elf_with_interpreter("/lib/ld-linux-aarch64.so.1");
        assert_eq!(
            interpreter(&plain).as_deref(),
            Some("/lib/ld-linux-aarch64.so.1")
        );
        assert!(!needs_relocation(&plain));
    }

    #[test]
    fn interpreter_of_a_non_elf_or_truncated_file_is_none() {
        assert_eq!(interpreter(b"#!/bin/sh\necho @@HOMEBREW_PREFIX@@\n"), None);
        assert!(!needs_relocation(b"#!/bin/sh\necho @@HOMEBREW_PREFIX@@\n"));
        let mut bytes = elf_with_interpreter("/lib/ld.so");
        bytes.truncate(0x50);
        assert_eq!(interpreter(&bytes), None);
        // A program header that points past the end of the file.
        let mut wild = elf_with_interpreter("/lib/ld.so");
        wild[0x48..0x50].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(interpreter(&wild), None);
    }

    #[test]
    fn relocate_maps_both_cellar_spellings_and_the_prefix() {
        let prefix = Path::new("/home/u/.local/share/maca/prefix");
        let cellar = Path::new("/srv/shared-cellar");
        let rpath = "@@HOMEBREW_PREFIX@@/Cellar/jq/1.8.2/lib:@@HOMEBREW_CELLAR@@/jq/1.8.2/lib:\
                     @@HOMEBREW_PREFIX@@/opt/oniguruma/lib:@@HOMEBREW_PREFIX@@/lib";
        assert_eq!(
            relocate(rpath, prefix, cellar).as_deref(),
            Some(
                "/srv/shared-cellar/jq/1.8.2/lib:/srv/shared-cellar/jq/1.8.2/lib:\
                 /home/u/.local/share/maca/prefix/opt/oniguruma/lib:\
                 /home/u/.local/share/maca/prefix/lib"
            )
        );
        assert_eq!(relocate("$ORIGIN/../lib", prefix, cellar), None);
        assert_eq!(relocate("", prefix, cellar), None);
    }
}
