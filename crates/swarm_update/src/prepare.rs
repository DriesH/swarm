//! Download, check and unpack a new `Swarm.app` (PLAN.md, Updates).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::{Error, Fetcher, Manifest, Result, SignatureVerifier, Version, user_agent};

const APP_NAME: &str = "Swarm.app";

/// A new `Swarm.app` that is downloaded and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub version: Version,
    /// The checked `Swarm.app` in the update folder.
    pub app: PathBuf,
    pub notes_url: String,
}

/// Makes the new `Swarm.app` ready in `<cache_root>/updates/<version>/`.
///
/// A folder that holds a verified `Swarm.app` of that version is reused. Any
/// other failure removes the folder and returns the error.
pub fn prepare(
    manifest: &Manifest,
    cache_root: &Path,
    fetcher: &impl Fetcher,
    verifier: &impl SignatureVerifier,
) -> Result<Prepared> {
    let folder = cache_root
        .join("updates")
        .join(manifest.version.to_string());
    let app = folder.join(APP_NAME);
    let prepared = Prepared {
        version: manifest.version,
        app: app.clone(),
        notes_url: manifest.notes_url.clone(),
    };

    if app.is_dir() && verifier.verify(&app, &manifest.version).is_ok() {
        return Ok(prepared);
    }

    // Whatever is left in the folder is from an attempt that did not finish.
    remove_folder(&folder)?;

    match fetch_and_unpack(manifest, &folder, fetcher, verifier) {
        Ok(()) => Ok(prepared),
        Err(error) => {
            if let Err(cleanup) = remove_folder(&folder) {
                log::warn!("cannot remove the failed update: {cleanup}");
            }

            Err(error)
        }
    }
}

fn fetch_and_unpack(
    manifest: &Manifest,
    folder: &Path,
    fetcher: &impl Fetcher,
    verifier: &impl SignatureVerifier,
) -> Result<()> {
    std::fs::create_dir_all(folder)
        .map_err(|error| Error::io("cannot make the update folder", error))?;

    let zip = folder.join(format!("Swarm-{}.zip", manifest.version));
    fetcher.download(&manifest.url, &user_agent(), &zip)?;

    if sha256_of(&zip)? != manifest.sha256 {
        return Err(Error::HashMismatch);
    }

    unpack(&zip, folder)?;
    let app = single_bundle(folder, &zip)?;

    verifier.verify(&app, &manifest.version)
}

fn remove_folder(folder: &Path) -> Result<()> {
    match std::fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(Error::io("cannot remove the update folder", error))
        }
        _ => Ok(()),
    }
}

/// Lowercase hex SHA-256 of a file.
fn sha256_of(path: &Path) -> Result<String> {
    let read_error = |error| Error::io("cannot read the download", error);

    let mut file = File::open(path).map_err(read_error)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(read_error)?;

    Ok(format!("{:x}", hasher.finalize()))
}

fn unpack(zip: &Path, folder: &Path) -> Result<()> {
    let output = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(folder)
        .output()
        .map_err(|error| Error::Unpack(format!("cannot start ditto: {error}")))?;

    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr);

        Err(Error::Unpack(message.trim().to_owned()))
    }
}

/// The folder must hold the zip and `Swarm.app`, and nothing else.
fn single_bundle(folder: &Path, zip: &Path) -> Result<PathBuf> {
    let list_error = |error| Error::io("cannot list the update folder", error);

    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(list_error)? {
        let entry = entry.map_err(list_error)?;
        if entry.path() != zip {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }

    let app = folder.join(APP_NAME);
    let is_real_folder = std::fs::symlink_metadata(&app).is_ok_and(|meta| meta.is_dir());
    if names != [APP_NAME] || !is_real_folder {
        names.sort();

        return Err(Error::Bundle(format!("found {names:?}")));
    }

    Ok(app)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::SystemFetcher;
    use crate::test_support::{Reply, TempFolder, TestServer};

    /// Records its calls and answers with a fixed result.
    struct FakeVerifier {
        accept: Cell<bool>,
        seen: RefCell<Vec<(PathBuf, Version)>>,
    }

    impl FakeVerifier {
        fn accepting() -> Self {
            Self {
                accept: Cell::new(true),
                seen: RefCell::default(),
            }
        }

        fn refusing() -> Self {
            let verifier = Self::accepting();
            verifier.accept.set(false);

            verifier
        }
    }

    impl SignatureVerifier for FakeVerifier {
        fn verify(&self, app: &Path, version: &Version) -> Result<()> {
            self.seen.borrow_mut().push((app.to_owned(), *version));

            if self.accept.get() {
                Ok(())
            } else {
                Err(Error::Signature("refused".to_owned()))
            }
        }
    }

    /// A zip made with `ditto`, as the release script makes it.
    fn zip_of(app_names: &[&str]) -> Vec<u8> {
        let folder = TempFolder::new("zip-source");
        let source = folder.path().join("source");
        for name in app_names {
            let contents = source.join(name).join("Contents");
            std::fs::create_dir_all(&contents).expect("folders");
            std::fs::write(contents.join("Info.plist"), "plist").expect("file");
        }
        let zip = folder.path().join("out.zip");

        let mut command = Command::new("/usr/bin/ditto");
        command.args(["-c", "-k"]);
        if let [name] = app_names {
            command.arg("--keepParent").arg(source.join(name));
        } else {
            command.arg(&source);
        }
        let status = command.arg(&zip).status().expect("ditto");
        assert!(status.success());

        std::fs::read(zip).expect("zip")
    }

    fn manifest_for(server: &TestServer, zip: &[u8]) -> Manifest {
        Manifest {
            version: Version::new(1, 2, 0),
            url: server.url("/Swarm-1.2.0.zip"),
            sha256: format!("{:x}", Sha256::digest(zip)),
            minimum_macos: Version::new(13, 0, 0),
            published: "2026-12-01T10:00:00Z".to_owned(),
            notes_url: "https://example.test/notes".to_owned(),
        }
    }

    fn update_folder(cache: &TempFolder) -> PathBuf {
        cache.path().join("updates/1.2.0")
    }

    #[test]
    fn a_good_zip_is_unpacked_and_checked() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-good");
        let verifier = FakeVerifier::accepting();

        let prepared = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &verifier,
        )
        .expect("prepared");

        let app = update_folder(&cache).join("Swarm.app");
        assert_eq!(
            prepared,
            Prepared {
                version: Version::new(1, 2, 0),
                app: app.clone(),
                notes_url: "https://example.test/notes".to_owned(),
            }
        );
        assert!(app.join("Contents/Info.plist").is_file());
        assert_eq!(*verifier.seen.borrow(), [(app, Version::new(1, 2, 0))]);
    }

    #[test]
    fn the_zip_is_kept_in_the_version_folder() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-zip");

        prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        )
        .expect("prepared");

        assert!(update_folder(&cache).join("Swarm-1.2.0.zip").is_file());
    }

    #[test]
    fn a_wrong_hash_removes_the_folder() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-hash");
        let mut manifest = manifest_for(&server, &zip);
        manifest.sha256 = "0".repeat(64);
        let verifier = FakeVerifier::accepting();

        let result = prepare(
            &manifest,
            cache.path(),
            &SystemFetcher::loopback(),
            &verifier,
        );

        assert!(matches!(result, Err(Error::HashMismatch)), "{result:?}");
        assert!(!update_folder(&cache).exists());
        assert!(verifier.seen.borrow().is_empty());
    }

    #[test]
    fn a_zip_without_swarm_app_removes_the_folder() {
        let zip = zip_of(&["Other.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-other");
        let verifier = FakeVerifier::accepting();

        let result = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &verifier,
        );

        assert!(matches!(result, Err(Error::Bundle(_))), "{result:?}");
        assert!(!update_folder(&cache).exists());
        assert!(verifier.seen.borrow().is_empty());
    }

    #[test]
    fn a_zip_with_two_apps_removes_the_folder() {
        let zip = zip_of(&["Swarm.app", "Extra.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-two");

        let result = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        );

        assert!(matches!(result, Err(Error::Bundle(_))), "{result:?}");
        assert!(!update_folder(&cache).exists());
    }

    #[test]
    fn a_zip_that_is_not_a_zip_removes_the_folder() {
        let zip = b"not a zip".to_vec();
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-garbage");

        let result = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        );

        assert!(matches!(result, Err(Error::Unpack(_))), "{result:?}");
        assert!(!update_folder(&cache).exists());
    }

    #[test]
    fn a_refusing_verifier_removes_the_folder() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-refused");

        let result = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::refusing(),
        );

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
        assert!(!update_folder(&cache).exists());
    }

    #[test]
    fn a_failed_download_removes_the_folder() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Status(500))]);
        let cache = TempFolder::new("prepare-download");

        let result = prepare(
            &manifest_for(&server, &zip),
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        );

        assert!(matches!(result, Err(Error::Fetch(_))), "{result:?}");
        assert!(!update_folder(&cache).exists());
    }

    #[test]
    fn a_verified_app_in_the_folder_is_reused_without_a_download() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-reuse");
        let manifest = manifest_for(&server, &zip);
        let first = prepare(
            &manifest,
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        )
        .expect("first");
        let asked = server.requests().len();

        let second = prepare(
            &manifest,
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        )
        .expect("second");

        assert_eq!(second, first);
        assert_eq!(server.requests().len(), asked);
    }

    #[test]
    fn an_app_in_the_folder_that_no_longer_verifies_is_downloaded_again() {
        let zip = zip_of(&["Swarm.app"]);
        let server = TestServer::start([("/Swarm-1.2.0.zip", Reply::Body(zip.clone()))]);
        let cache = TempFolder::new("prepare-again");
        let manifest = manifest_for(&server, &zip);
        prepare(
            &manifest,
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::accepting(),
        )
        .expect("first");
        let asked = server.requests().len();
        std::fs::write(
            update_folder(&cache).join("Swarm.app/Contents/tampered"),
            "x",
        )
        .expect("tamper");

        let second = prepare(
            &manifest,
            cache.path(),
            &SystemFetcher::loopback(),
            &FakeVerifier::refusing(),
        );

        assert!(second.is_err());
        assert_eq!(server.requests().len(), asked + 1);
        assert!(!update_folder(&cache).exists());
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        let folder = TempFolder::new("sha");
        std::fs::write(folder.path().join("f"), "abc").expect("file");

        assert_eq!(
            sha256_of(&folder.path().join("f")).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
