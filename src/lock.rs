//! Cross-process install locking for a prefix.
//!
//! Every mutating command (`install`, `remove`, `upgrade`, `snapshot restore`)
//! takes an exclusive `flock`/`LockFileEx` on `<prefix>/.maca.lock` so two maca
//! processes cannot race the ledger, the cellar, or the bin links. The lock is
//! advisory and released when the guard is dropped.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub struct PrefixLock {
    _guard: fd_lock::RwLockWriteGuard<'static, std::fs::File>,
}

impl PrefixLock {
    /// Acquire the exclusive write lock, blocking until it is available.
    pub fn acquire(prefix: &Path) -> Result<Self> {
        let lock_path = lock_path(prefix);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        // Leak the lock so the returned guard can be 'static for the lifetime
        // of this CLI process. The OS releases the lock on exit regardless.
        let boxed: &'static mut fd_lock::RwLock<std::fs::File> =
            Box::leak(Box::new(fd_lock::RwLock::new(file)));
        let guard = boxed.write().map_err(|e| {
            Error::Other(anyhow::anyhow!(
                "prefix lock on {}: {e}",
                lock_path.display()
            ))
        })?;
        Ok(Self { _guard: guard })
    }

    /// Try to acquire without blocking; `None` if another writer holds it.
    pub fn try_acquire(prefix: &Path) -> Result<Option<Self>> {
        let lock_path = lock_path(prefix);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let boxed: &'static mut fd_lock::RwLock<std::fs::File> =
            Box::leak(Box::new(fd_lock::RwLock::new(file)));
        match boxed.try_write() {
            Ok(guard) => Ok(Some(Self { _guard: guard })),
            Err(_) => Ok(None),
        }
    }

    pub fn path(prefix: &Path) -> PathBuf {
        lock_path(prefix)
    }
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
