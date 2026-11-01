//! Fake commands for tests: shell scripts in a folder of their own.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static NEXT: AtomicU32 = AtomicU32::new(0);

pub(crate) struct Scripts {
    dir: PathBuf,
}

impl Scripts {
    pub(crate) fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("swu-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test folder");

        Self { dir }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes an executable `/bin/sh` script. `$DIR` is the folder of the scripts.
    pub(crate) fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nDIR='{}'\n{body}\n", self.dir.display()),
        )
        .expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");

        path
    }

    /// Starts a script once with `warm-up` as its argument, which it must answer with
    /// `exit 0`. The first start of a new file takes about 250 ms on macOS, and more under
    /// load. A test with a short timeout would run out of time before the script ran.
    pub(crate) fn warm_up(path: &Path) {
        crate::process::spawn(
            Command::new(path)
                .arg("warm-up")
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .and_then(|mut child| child.wait())
        .expect("warm up the script");
    }

    pub(crate) fn write(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join(name), text).expect("file");
    }

    pub(crate) fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    /// Whether a process we started still exists (`kill -0` also finds zombies).
    pub(crate) fn process_exists(pid: &str) -> bool {
        crate::process::spawn(
            Command::new("/bin/kill")
                .args(["-0", pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .and_then(|mut child| child.wait())
        .is_ok_and(|status| status.success())
    }

    /// Waits up to 3 seconds for a process to go: its parent must still reap it.
    pub(crate) fn wait_until_gone(pid: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);

        while Self::process_exists(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::park_timeout(Duration::from_millis(20));
        }

        true
    }
}

impl Drop for Scripts {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
