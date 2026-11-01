//! A folder for one test, removed again when the test ends.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

pub(crate) struct TempFolder(PathBuf);

impl TempFolder {
    pub(crate) fn new(name: &str) -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("swd-{name}-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&path).expect("test folder");

        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_removed_when_the_value_goes() {
        let folder = TempFolder::new("gone");
        let path = folder.path().to_owned();
        std::fs::write(path.join("file"), "x").expect("file");
        assert!(path.is_dir());

        drop(folder);

        assert!(!path.exists());
    }
}
