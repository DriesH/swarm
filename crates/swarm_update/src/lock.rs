//! The lock on `<cache root>/updates/`, so that two Swarm apps and the install
//! helper never work in the update folders at the same time.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// How often a waiting process tries the lock again.
const POLL: Duration = Duration::from_millis(100);

/// The folder that holds one folder per downloaded version.
pub(crate) fn updates_folder(cache_root: &Path) -> PathBuf {
    cache_root.join("updates")
}

/// An exclusive `flock` on `<cache root>/updates/.lock`. It ends when this value
/// is dropped, or when the process ends.
#[derive(Debug)]
pub struct UpdateLock {
    _file: File,
}

impl UpdateLock {
    /// Waits up to `timeout` for the lock.
    pub fn acquire(cache_root: &Path, timeout: Duration) -> Result<Self> {
        let folder = updates_folder(cache_root);
        std::fs::create_dir_all(&folder)
            .map_err(|error| Error::io("cannot make the update folder", error))?;

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(folder.join(".lock"))
            .map_err(|error| Error::io("cannot open the update lock", error))?;

        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::park_timeout(POLL);
                }
                Err(TryLockError::WouldBlock) => return Err(Error::Busy),
                Err(TryLockError::Error(error)) => {
                    return Err(Error::io("cannot lock the update folder", error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempFolder;

    #[test]
    fn a_second_lock_waits_and_gives_up() {
        let cache = TempFolder::new("lock-busy");
        let _first = UpdateLock::acquire(cache.path(), Duration::from_secs(1)).expect("first");

        let second = UpdateLock::acquire(cache.path(), Duration::from_millis(250));

        assert!(matches!(second, Err(Error::Busy)), "{second:?}");
    }

    #[test]
    fn the_lock_is_free_again_after_the_first_ends() {
        let cache = TempFolder::new("lock-free");
        let first = UpdateLock::acquire(cache.path(), Duration::from_secs(1)).expect("first");
        drop(first);

        let second = UpdateLock::acquire(cache.path(), Duration::from_millis(250));

        assert!(second.is_ok(), "{second:?}");
    }

    #[test]
    fn a_waiting_lock_is_taken_when_the_holder_lets_go() {
        let cache = TempFolder::new("lock-wait");
        let first = UpdateLock::acquire(cache.path(), Duration::from_secs(1)).expect("first");

        let root = cache.path().to_owned();
        let waiter =
            std::thread::spawn(move || UpdateLock::acquire(&root, Duration::from_secs(10)).is_ok());
        std::thread::park_timeout(Duration::from_millis(300));
        drop(first);

        assert!(waiter.join().expect("thread"));
    }

    #[test]
    fn the_lock_file_sits_in_the_updates_folder() {
        let cache = TempFolder::new("lock-place");

        let _lock = UpdateLock::acquire(cache.path(), Duration::from_secs(1)).expect("lock");

        assert!(cache.path().join("updates/.lock").is_file());
    }
}
