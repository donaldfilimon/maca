//! Cross-process install locking for a prefix.
//!
//! Every mutating command (`install`, `remove`, `upgrade`, `snapshot restore`)
//! takes an exclusive `flock`/`LockFileEx` on `<prefix>/.maca.lock` so two maca
//! processes cannot race the ledger, the cellar, or the bin links. The lock is
//! advisory and released when the guard is dropped.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::Result;

/// Exclusive lock on `<prefix>/.maca.lock`, released when dropped (closing the
/// file releases the OS lock). Not re-entrant: a second acquire in the same
/// process blocks, so take it once, at the top of a command.
pub struct PrefixLock {
    _file: File,
}

impl PrefixLock {
    /// Acquire the exclusive write lock, blocking until it is available.
    pub fn acquire(prefix: &Path) -> Result<Self> {
        let file = open(prefix)?;
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Try to acquire without blocking; `None` if another writer holds it.
    pub fn try_acquire(prefix: &Path) -> Result<Option<Self>> {
        let file = open(prefix)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    pub fn path(prefix: &Path) -> PathBuf {
        lock_path(prefix)
    }
}

fn open(prefix: &Path) -> Result<File> {
    let lock_path = lock_path(prefix);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?)
}

fn lock_path(prefix: &Path) -> PathBuf {
    prefix.join(".maca.lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_roundtrip_in_tempdir() {
        let dir = std::env::temp_dir().join(format!("maca-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let lock = PrefixLock::acquire(&dir).unwrap();
            // A second blocking acquire would deadlock in a single-threaded
            // test, so assert the non-blocking variant reports contention.
            assert!(PrefixLock::try_acquire(&dir).unwrap().is_none());
            drop(lock);
        }
        // Released: the lock path exists and a fresh acquire succeeds.
        assert!(lock_path(&dir).exists());
        let _lock = PrefixLock::acquire(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
