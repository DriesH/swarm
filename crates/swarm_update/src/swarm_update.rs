//! Swarm's own updater (ADR 0010, ADR 0011). No GPUI: the app calls this crate.
//!
//! The steps, in the order the app runs them:
//!
//! 1. [`running_build`] says if this build may update itself, and [`can_replace`]
//!    if its folder allows it.
//! 2. Read the manifest with [`Fetcher::get`] on [`MANIFEST_URL`], then
//!    [`parse_manifest`] with [`REPO_PREFIX`] and [`evaluate`].
//! 3. For an [`Offer::Available`], [`prepare`] downloads, checks and unpacks the
//!    new `Swarm.app`.
//! 4. On restart, [`launch_helper`] starts `swarm update-install`. The helper
//!    calls [`install`] after the app has exited. It checks the new app, the old
//!    app and the place of the new app again, swaps the two in one step and
//!    opens the old app again if anything fails.
//! 5. [`clean_up`] removes the folders of versions that are installed.
//!
//! [`prepare`], [`clean_up`] and [`install`] hold the [`UpdateLock`], so two
//! apps never work in `<cache root>/updates/` at the same time.
//!
//! Every function blocks. The app calls them from a background thread.

mod fetch;
mod install;
mod lock;
mod manifest;
mod prepare;
mod signature;

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use objc2_foundation::NSProcessInfo;

pub use crate::fetch::{Fetcher, SystemFetcher};
pub use crate::install::{
    BundleInfo, InstallRequest, InstallSteps, SystemSteps, can_replace, install, launch_helper,
};
pub use crate::lock::UpdateLock;
pub use crate::manifest::{Manifest, Offer, Version, VersionParseError, evaluate, parse_manifest};
pub use crate::prepare::{Prepared, clean_up, prepare};
pub use crate::signature::{DeveloperIdVerifier, RunningBuild, SignatureVerifier, running_build};

/// Where the app reads the manifest. GitHub redirects it to the newest release.
pub const MANIFEST_URL: &str =
    "https://github.com/DriesH/swarm/releases/latest/download/latest.json";

/// `url` and `notes_url` of a manifest must start with this.
pub const REPO_PREFIX: &str = "https://github.com/DriesH/swarm/";

/// What can go wrong between the manifest and the install.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the manifest is not valid JSON: {0}")]
    ManifestJson(serde_json::Error),
    #[error("manifest field `{field}` {reason}")]
    ManifestField { field: &'static str, reason: String },
    #[error("download failed: {0}")]
    Fetch(String),
    #[error("the download does not match the SHA-256 of the manifest")]
    HashMismatch,
    #[error("cannot unpack the download: {0}")]
    Unpack(String),
    #[error("the download must hold exactly one Swarm.app: {0}")]
    Bundle(String),
    #[error("the signature check failed: {0}")]
    Signature(String),
    #[error("cannot install: {0}")]
    Install(String),
    #[error("another update is in progress")]
    Busy,
    #[error("{context}: {source}")]
    Io { context: String, source: io::Error },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// `~/Library/Caches/Swarm`, where downloaded updates wait. `None` without `HOME`.
pub fn cache_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;

    cache_root_in(&home)
}

fn cache_root_in(home: &OsStr) -> Option<PathBuf> {
    let home = Path::new(home);

    home.is_absolute()
        .then(|| home.join("Library/Caches/Swarm"))
}

/// The only thing a request to GitHub says about us: `Swarm/<version>`.
pub fn user_agent() -> String {
    format!("Swarm/{}", Version::current())
}

/// The macOS this process runs on.
pub fn macos_version() -> Version {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();

    Version::new(
        u32::try_from(version.majorVersion).unwrap_or(0),
        u32::try_from(version.minorVersion).unwrap_or(0),
        u32::try_from(version.patchVersion).unwrap_or(0),
    )
}

/// A folder for one test, and a small HTTP server on this Mac.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// What the test server answers for one path.
    pub(crate) enum Reply {
        Body(Vec<u8>),
        Status(u16),
        Redirect(String),
    }

    /// Answers `GET`s on `127.0.0.1` and keeps the request heads it saw.
    pub(crate) struct TestServer {
        port: u16,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestServer {
        pub(crate) fn start<const N: usize>(routes: [(&str, Reply); N]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("address").port();
            let routes: HashMap<String, Reply> = routes
                .into_iter()
                .map(|(path, reply)| (path.to_owned(), reply))
                .collect();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));

            let thread = {
                let (requests, stop) = (Arc::clone(&requests), Arc::clone(&stop));
                std::thread::spawn(move || serve(&listener, &routes, &requests, &stop))
            };

            Self {
                port,
                requests,
                stop,
                thread: Some(thread),
            }
        }

        pub(crate) fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        pub(crate) fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("requests").clone()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // A connection wakes the blocked `accept`.
            TcpStream::connect(("127.0.0.1", self.port)).ok();
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
        }
    }

    fn serve(
        listener: &TcpListener,
        routes: &HashMap<String, Reply>,
        requests: &Mutex<Vec<String>>,
        stop: &AtomicBool,
    ) {
        for stream in listener.incoming() {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if let Ok(stream) = stream {
                answer(stream, routes, requests);
            }
        }
    }

    fn answer(
        mut stream: TcpStream,
        routes: &HashMap<String, Reply>,
        requests: &Mutex<Vec<String>>,
    ) {
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

        let mut head = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 65536 {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => head.extend_from_slice(&chunk[..count]),
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();

        let (status, extra, body) = match routes.get(&path) {
            Some(Reply::Body(body)) => ("200 OK", String::new(), body.as_slice()),
            Some(Reply::Status(code)) => (status_line(*code), String::new(), &[][..]),
            Some(Reply::Redirect(location)) => (
                "302 Found",
                format!("Location: {location}\r\nSet-Cookie: a=b\r\n"),
                &[][..],
            ),
            None => ("404 Not Found", String::new(), &[][..]),
        };
        let header = format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        requests.lock().expect("requests").push(head);
        stream.write_all(header.as_bytes()).ok();
        stream.write_all(body).ok();
        stream.flush().ok();
        stream.shutdown(Shutdown::Both).ok();
    }

    fn status_line(code: u16) -> &'static str {
        match code {
            404 => "404 Not Found",
            _ => "500 Internal Server Error",
        }
    }

    pub(crate) struct TempFolder(PathBuf);

    impl TempFolder {
        pub(crate) fn new(name: &str) -> Self {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("swu-{name}-{}-{number}", std::process::id()));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_carries_only_the_app_version() {
        assert_eq!(user_agent(), format!("Swarm/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_cache_root_is_under_the_home_folder() {
        assert_eq!(
            cache_root_in(OsStr::new("/Users/me")),
            Some(PathBuf::from("/Users/me/Library/Caches/Swarm"))
        );
    }

    #[test]
    fn a_home_that_is_not_absolute_gives_no_cache_root() {
        assert_eq!(cache_root_in(OsStr::new("")), None);
        assert_eq!(cache_root_in(OsStr::new("relative")), None);
    }

    #[test]
    fn the_macos_version_is_not_zero() {
        assert!(macos_version() >= Version::new(13, 0, 0));
    }

    #[test]
    fn the_manifest_url_lives_under_the_repository_prefix() {
        assert!(MANIFEST_URL.starts_with(REPO_PREFIX));
    }
}
