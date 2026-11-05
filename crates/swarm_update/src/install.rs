//! Swaps the app bundle after the app has quit (PLAN.md, Updates, Install).
//!
//! A copy of the `swarm` binary does the swap, because the app cannot replace
//! itself while it runs. The helper trusts nothing it is given: it checks the new
//! bundle, the old bundle and the place of the new one again before it swaps.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::lock::{UpdateLock, updates_folder};
use crate::prepare::FAILURE_FILE;
use crate::signature::{APP_IDENTIFIER, bundle_string};
use crate::{DeveloperIdVerifier, Error, Prepared, Result, SignatureVerifier, Version};

/// How long the helper waits for the app to quit.
const EXIT_WAIT: Duration = Duration::from_secs(120);

/// How long the helper waits for another process in the update folders.
const LOCK_WAIT: Duration = Duration::from_secs(120);

/// How often the helper looks if the app has quit.
const POLL: Duration = Duration::from_millis(200);

/// A step that failed, in words for the log and for the person.
type Step<T = ()> = std::result::Result<T, String>;

/// What the `Info.plist` of an app folder says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleInfo {
    /// `CFBundleIdentifier`.
    pub identifier: String,
    /// `CFBundleShortVersionString`, when it reads as `MAJOR.MINOR.PATCH`.
    pub version: Option<Version>,
}

/// What `swarm update-install` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The app that must quit first.
    pub pid: u32,
    /// The installed `Swarm.app`, which goes to the Trash.
    pub old: &'a Path,
    /// The downloaded `Swarm.app` that takes its place.
    pub new: &'a Path,
    /// The version that `new` must be, and that `old` must be older than.
    pub version: Version,
    /// The folder with `updates/<version>/`, where `new` must be.
    pub cache_root: &'a Path,
    /// Open the app when done. Also after a failure, when `old` is Swarm.
    pub reopen: bool,
}

/// The steps that touch the system, so that tests can run [`install`] on a
/// temporary folder.
pub trait InstallSteps {
    /// Waits until the process is gone. `false` when `timeout` passed first.
    fn wait_for_exit(&self, pid: u32, timeout: Duration) -> bool;

    /// Takes the lock on the update folders.
    fn lock(&self, cache_root: &Path) -> Result<UpdateLock>;

    /// Fails unless `app` is `Swarm.app` of `version`, signed by our team.
    fn verify(&self, app: &Path, version: &Version) -> Result<()>;

    /// What the `Info.plist` of `app` says. `None` when `app` is not a bundle.
    fn bundle_info(&self, app: &Path) -> Option<BundleInfo>;

    /// Swaps the two folders in one step: `new` holds the old content afterwards
    /// and `old` the new one. If it fails, nothing changed.
    fn swap(&self, new: &Path, old: &Path) -> io::Result<()>;

    /// Moves a folder to the Trash and says where it went.
    fn move_to_trash(&self, path: &Path) -> io::Result<PathBuf>;

    /// Opens the app. An app that runs already comes to the front.
    fn open(&self, app: &Path) -> io::Result<()>;
}

/// Replaces `old` by `new`, step by step, and writes each step to `log`.
///
/// Waits for the app to quit, takes the lock and checks everything again. Then
/// `new` and `old` swap places in one step, so there is never a moment without an
/// app, and the old app goes to the Trash. If any step fails before the swap,
/// nothing changed: the reason goes to `install-failed` in the version folder and,
/// with `reopen`, the old app opens again.
pub fn install(
    request: &InstallRequest<'_>,
    steps: &impl InstallSteps,
    log: &mut impl Write,
) -> Result<()> {
    let InstallRequest {
        pid,
        old,
        version,
        cache_root,
        reopen,
        ..
    } = *request;

    if pid == 0 {
        return Err(fail(log, "the process id is not valid"));
    }

    if let Err(reason) = replace(request, steps, log) {
        note(log, &format!("error: {reason}"));
        record_failure(cache_root, &version, &reason, log);
        if reopen {
            reopen_old(steps, old, log);
        }

        return Err(Error::Install(reason));
    }

    if reopen {
        note(log, &format!("opening {}", old.display()));
        if let Err(error) = steps.open(old) {
            let message = format!("installed, but cannot open the new app: {error}");

            return Err(fail(log, &message));
        }
    }

    note(log, "done");

    Ok(())
}

/// Everything up to and including the swap.
fn replace(request: &InstallRequest<'_>, steps: &impl InstallSteps, log: &mut impl Write) -> Step {
    let InstallRequest {
        pid,
        old,
        new,
        cache_root,
        ..
    } = *request;

    note(log, &format!("waiting for process {pid} to exit"));
    if !steps.wait_for_exit(pid, EXIT_WAIT) {
        return Err("the app did not exit in time, nothing changed".to_owned());
    }

    let _lock = steps.lock(cache_root).map_err(|error| error.to_string())?;

    check(request, steps)?;
    note(log, "the checks passed");

    note(
        log,
        &format!("swapping {} and {}", new.display(), old.display()),
    );
    steps
        .swap(new, old)
        .map_err(|error| format!("cannot swap the apps, nothing changed: {error}"))?;

    // The old app is at the path of `new` now.
    match steps.move_to_trash(new) {
        Ok(trashed) => note(log, &format!("the old app is now at {}", trashed.display())),
        Err(error) => note(
            log,
            &format!(
                "the new app is installed, but the old app stays at {}: {error}",
                new.display()
            ),
        ),
    }

    Ok(())
}

/// The new app is where the app put it and is the version it should be. The old
/// app is Swarm and older.
fn check(request: &InstallRequest<'_>, steps: &impl InstallSteps) -> Step {
    let InstallRequest {
        old,
        new,
        version,
        cache_root,
        ..
    } = *request;

    check_place(new, cache_root, &version)?;
    check_old(old, &version, steps)?;

    steps
        .verify(new, &version)
        .map_err(|error| error.to_string())
}

/// `new` is a folder right inside `<cache root>/updates/<version>/`, after links
/// are resolved.
fn check_place(new: &Path, cache_root: &Path, version: &Version) -> Step {
    let canonical = |path: &Path| {
        path.canonicalize()
            .map_err(|error| format!("cannot find {}: {error}", path.display()))
    };

    let folder = canonical(&updates_folder(cache_root).join(version.to_string()))?;
    let new_real = canonical(new)?;

    if new_real.is_dir() && new_real.parent() == Some(folder.as_path()) {
        Ok(())
    } else {
        Err(format!(
            "{} is not in the update folder {}",
            new.display(),
            folder.display()
        ))
    }
}

fn check_old(old: &Path, version: &Version, steps: &impl InstallSteps) -> Step {
    let Some(info) = steps.bundle_info(old) else {
        return Err(format!("{} is not an app", old.display()));
    };

    if info.identifier != APP_IDENTIFIER {
        return Err(format!("{} is not Swarm", old.display()));
    }

    match info.version {
        Some(installed) if installed < *version => Ok(()),
        Some(installed) => Err(format!(
            "the installed app is version {installed}, which is not older than {version}"
        )),
        None => Err(format!("{} has no version", old.display())),
    }
}

/// Says why the install failed, in the version folder, so that the next launch of
/// the app does not try again. Only the folder of this version is written to.
fn record_failure(cache_root: &Path, version: &Version, reason: &str, log: &mut impl Write) {
    let folder = updates_folder(cache_root).join(version.to_string());
    if !folder.is_dir() {
        return;
    }

    if let Err(error) = std::fs::write(folder.join(FAILURE_FILE), format!("{reason}\n")) {
        note(log, &format!("cannot record the failure: {error}"));
    }
}

/// After a failure the person still has Swarm. Only a bundle that is Swarm opens,
/// so that the helper never opens what it was only pointed at.
fn reopen_old(steps: &impl InstallSteps, old: &Path, log: &mut impl Write) {
    let is_swarm = steps
        .bundle_info(old)
        .is_some_and(|info| info.identifier == APP_IDENTIFIER);
    if !is_swarm {
        return;
    }

    note(log, &format!("opening {} again", old.display()));
    if let Err(error) = steps.open(old) {
        note(log, &format!("cannot open the old app: {error}"));
    }
}

fn note(log: &mut impl Write, message: &str) {
    if writeln!(log, "{message}").is_err() {
        log::debug!("cannot write the install log");
    }
}

fn fail(log: &mut impl Write, message: &str) -> Error {
    note(log, &format!("error: {message}"));

    Error::Install(message.to_owned())
}

/// The real steps: `kill(pid, 0)`, the Developer ID check, `renamex_np`,
/// `NSFileManager` and `open`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSteps;

impl InstallSteps for SystemSteps {
    fn wait_for_exit(&self, pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;

        while process_exists(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::park_timeout(POLL);
        }

        true
    }

    fn lock(&self, cache_root: &Path) -> Result<UpdateLock> {
        UpdateLock::acquire(cache_root, LOCK_WAIT)
    }

    fn verify(&self, app: &Path, version: &Version) -> Result<()> {
        DeveloperIdVerifier.verify(app, version)
    }

    fn bundle_info(&self, app: &Path) -> Option<BundleInfo> {
        let identifier = bundle_string(app, "CFBundleIdentifier")?;
        let version = bundle_string(app, "CFBundleShortVersionString")
            .and_then(|version| version.parse().ok());

        Some(BundleInfo {
            identifier,
            version,
        })
    }

    fn swap(&self, new: &Path, old: &Path) -> io::Result<()> {
        let (new, old) = (c_path(new)?, c_path(old)?);

        // SAFETY: both are valid NUL-terminated strings that live through the call.
        let result = unsafe { libc::renamex_np(new.as_ptr(), old.as_ptr(), libc::RENAME_SWAP) };
        if result == 0 {
            Ok(())
        } else {
            Err(swap_error(io::Error::last_os_error()))
        }
    }

    /// Not covered by tests: they must not touch the real Trash.
    fn move_to_trash(&self, path: &Path) -> io::Result<PathBuf> {
        let text = path
            .to_str()
            .ok_or_else(|| io::Error::other("the path is not text"))?;
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(text), true);

        let mut resulting = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
            .map_err(|error| io::Error::other(error.localizedDescription().to_string()))?;

        resulting
            .and_then(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| io::Error::other("the Trash did not say where the app went"))
    }

    fn open(&self, app: &Path) -> io::Result<()> {
        let status = Command::new("/usr/bin/open").arg(app).status()?;

        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("open exited with {status}")))
        }
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the path has a NUL byte"))
}

/// A swap across two volumes is not possible: say so in plain words.
fn swap_error(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(libc::EXDEV) {
        io::Error::other("the update and the app are on different volumes")
    } else {
        error
    }
}

fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };

    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };

    // EPERM: the process exists but belongs to someone else.
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The bundle and the folder of the bundle are writable, so the app can be replaced.
pub fn can_replace(bundle: &Path) -> bool {
    let Some(parent) = bundle.parent() else {
        return false;
    };

    writable_folder(parent) && writable_folder(bundle)
}

fn writable_folder(folder: &Path) -> bool {
    let Ok(folder) = c_path(folder) else {
        return false;
    };

    // SAFETY: `folder` is a valid NUL-terminated string that lives through the call.
    unsafe { libc::access(folder.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// Starts `swarm update-install` as a separate process, with no ties to the
/// app: it keeps running after the app quits.
///
/// The helper is a copy of the running bundle's `swarm`, in the update folder. It
/// and its log carry the process id of the app, so that a second app never
/// rewrites a file that a helper uses. After this returns `Ok`, the app should
/// quit; the helper waits for `pid` to exit.
pub fn launch_helper(prepared: &Prepared, bundle: &Path, pid: u32, reopen: bool) -> Result<()> {
    let folder = prepared
        .app
        .parent()
        .ok_or_else(|| Error::Install("the update has no folder".to_owned()))?;

    let helper = copy_helper(&bundle.join("Contents/MacOS/swarm"), folder, pid)?;
    let log = File::create(folder.join(format!("install-{pid}.log")))
        .map_err(|error| Error::io("cannot make the install log", error))?;

    let mut command = Command::new(&helper);
    command
        .arg("update-install")
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--old")
        .arg(bundle)
        .arg("--new")
        .arg(&prepared.app)
        .arg("--version")
        .arg(prepared.version.to_string());
    if reopen {
        command.arg("--reopen");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));

    // SAFETY: `setsid` is async-signal-safe and touches no memory of this process.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    let mut child = command
        .spawn()
        .map_err(|error| Error::io("cannot start the install helper", error))?;

    // Reaps the helper if it ends while the app still runs.
    std::thread::spawn(move || {
        if let Err(error) = child.wait() {
            log::debug!("cannot wait for the install helper: {error}");
        }
    });

    Ok(())
}

/// Copies `swarm` to a temporary name and renames it to `swarm-installer-<pid>`.
fn copy_helper(source: &Path, folder: &Path, pid: u32) -> Result<PathBuf> {
    let staging = folder.join(format!(".swarm-installer-{pid}.tmp"));
    let helper = folder.join(format!("swarm-installer-{pid}"));

    let moved = std::fs::copy(source, &staging).and_then(|_| std::fs::rename(&staging, &helper));
    if let Err(error) = moved {
        if let Err(cleanup) = std::fs::remove_file(&staging) {
            log::debug!("cannot remove the staged install helper: {cleanup}");
        }

        return Err(Error::io("cannot copy the install helper", error));
    }

    Ok(helper)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::test_support::TempFolder;

    const SWARM: &str = "com.swarm.app";

    /// Moves "to the Trash" into a folder of the test, and can fail on request.
    #[derive(PartialEq)]
    enum Failure {
        AppNeverExits,
        Verify,
        Swap,
        Trash,
        Open,
    }

    struct FakeSteps {
        trash: PathBuf,
        failures: Vec<Failure>,
        old_info: Option<BundleInfo>,
        calls: RefCell<Vec<String>>,
        opened: RefCell<Vec<PathBuf>>,
    }

    impl FakeSteps {
        fn new(trash: PathBuf) -> Self {
            Self {
                trash,
                failures: Vec::new(),
                old_info: Some(BundleInfo {
                    identifier: SWARM.to_owned(),
                    version: Some(Version::new(1, 0, 0)),
                }),
                calls: RefCell::default(),
                opened: RefCell::default(),
            }
        }

        fn failing(trash: PathBuf, failures: Vec<Failure>) -> Self {
            Self {
                failures,
                ..Self::new(trash)
            }
        }

        fn with_old(trash: PathBuf, identifier: &str, version: Option<Version>) -> Self {
            Self {
                old_info: Some(BundleInfo {
                    identifier: identifier.to_owned(),
                    version,
                }),
                ..Self::new(trash)
            }
        }

        fn fails(&self, failure: &Failure) -> bool {
            self.failures.contains(failure)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn opened(&self) -> Vec<PathBuf> {
            self.opened.borrow().clone()
        }
    }

    impl InstallSteps for FakeSteps {
        fn wait_for_exit(&self, pid: u32, _timeout: Duration) -> bool {
            self.calls.borrow_mut().push(format!("wait {pid}"));

            !self.fails(&Failure::AppNeverExits)
        }

        fn lock(&self, cache_root: &Path) -> Result<UpdateLock> {
            self.calls.borrow_mut().push("lock".to_owned());

            UpdateLock::acquire(cache_root, Duration::from_millis(250))
        }

        fn verify(&self, _app: &Path, _version: &Version) -> Result<()> {
            self.calls.borrow_mut().push("verify".to_owned());
            if self.fails(&Failure::Verify) {
                return Err(Error::Signature("the signature is wrong".to_owned()));
            }

            Ok(())
        }

        fn bundle_info(&self, _app: &Path) -> Option<BundleInfo> {
            self.old_info.clone()
        }

        fn swap(&self, new: &Path, old: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("swap".to_owned());
            if self.fails(&Failure::Swap) {
                return Err(io::Error::other("cannot swap"));
            }

            let parked = new.with_extension("parked");
            std::fs::rename(new, &parked)?;
            std::fs::rename(old, new)?;

            std::fs::rename(&parked, old)
        }

        fn move_to_trash(&self, path: &Path) -> io::Result<PathBuf> {
            self.calls.borrow_mut().push("trash".to_owned());
            if self.fails(&Failure::Trash) {
                return Err(io::Error::other("trash is full"));
            }

            let target = self.trash.join("Swarm.app");
            std::fs::rename(path, &target)?;

            Ok(target)
        }

        fn open(&self, app: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("open".to_owned());
            self.opened.borrow_mut().push(app.to_owned());
            if self.fails(&Failure::Open) {
                return Err(io::Error::other("cannot open"));
            }

            Ok(())
        }
    }

    /// An "installed" app, a downloaded one in the cache and a trash folder.
    struct Setup {
        _folder: TempFolder,
        old: PathBuf,
        new: PathBuf,
        trash: PathBuf,
        cache: PathBuf,
    }

    fn setup() -> Setup {
        let folder = TempFolder::new("install");
        let old = folder.path().join("Applications/Swarm.app");
        let cache = folder.path().join("cache");
        let new = cache.join("updates/1.2.0/Swarm.app");
        let trash = folder.path().join("trash");
        for (app, mark) in [(&old, "old"), (&new, "new")] {
            std::fs::create_dir_all(app).expect("app");
            std::fs::write(app.join("mark"), mark).expect("mark");
        }
        std::fs::create_dir_all(&trash).expect("trash");

        Setup {
            _folder: folder,
            old,
            new,
            trash,
            cache,
        }
    }

    fn request<'a>(setup: &'a Setup, new: &'a Path, reopen: bool) -> InstallRequest<'a> {
        InstallRequest {
            pid: 4242,
            old: &setup.old,
            new,
            version: Version::new(1, 2, 0),
            cache_root: &setup.cache,
            reopen,
        }
    }

    fn run(setup: &Setup, steps: &FakeSteps, reopen: bool) -> (Result<()>, String) {
        run_with(&request(setup, &setup.new, reopen), steps)
    }

    fn run_with(request: &InstallRequest<'_>, steps: &FakeSteps) -> (Result<()>, String) {
        let mut log = Vec::new();

        let result = install(request, steps, &mut log);

        (result, String::from_utf8(log).expect("utf8 log"))
    }

    fn mark(app: &Path) -> String {
        std::fs::read_to_string(app.join("mark")).expect("mark")
    }

    fn failure_file(setup: &Setup) -> Option<String> {
        std::fs::read_to_string(setup.cache.join("updates/1.2.0").join(FAILURE_FILE)).ok()
    }

    /// Nothing moved: both apps are where they were.
    fn assert_untouched(setup: &Setup) {
        assert_eq!(mark(&setup.old), "old");
        assert_eq!(mark(&setup.new), "new");
        assert!(!setup.trash.join("Swarm.app").exists());
    }

    #[test]
    fn the_new_app_takes_the_place_of_the_old_one() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run(&setup, &steps, false);

        result.expect("installed");
        assert_eq!(mark(&setup.old), "new");
        assert_eq!(mark(&setup.trash.join("Swarm.app")), "old");
        assert!(!setup.new.exists());
    }

    #[test]
    fn the_steps_run_in_order() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());

        run(&setup, &steps, true).0.expect("installed");

        assert_eq!(
            steps.calls(),
            ["wait 4242", "lock", "verify", "swap", "trash", "open"]
        );
    }

    #[test]
    fn each_step_is_logged() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());

        let (_, log) = run(&setup, &steps, true);

        assert_eq!(log.lines().count(), 6, "{log}");
        assert!(log.contains("waiting for process 4242"), "{log}");
        assert!(log.ends_with("done\n"), "{log}");
    }

    #[test]
    fn the_new_app_is_opened_only_when_asked() {
        let setup = setup();
        let without = FakeSteps::new(setup.trash.clone());
        run(&setup, &without, false).0.expect("installed");

        let setup = self::setup();
        let with = FakeSteps::new(setup.trash.clone());
        run(&setup, &with, true).0.expect("installed");

        assert_eq!(
            (without.opened().len(), with.opened()),
            (0, vec![setup.old.clone()])
        );
    }

    #[test]
    fn a_successful_install_leaves_no_failure_file() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());

        run(&setup, &steps, true).0.expect("installed");

        assert_eq!(failure_file(&setup), None);
    }

    #[test]
    fn an_app_that_does_not_exit_changes_nothing() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::AppNeverExits]);

        let (result, _) = run(&setup, &steps, false);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_eq!(steps.calls(), ["wait 4242"]);
        assert_untouched(&setup);
    }

    #[test]
    fn an_app_that_does_not_exit_is_opened_again() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::AppNeverExits]);

        run(&setup, &steps, true).0.expect_err("failed");

        assert_eq!(steps.opened(), std::slice::from_ref(&setup.old));
    }

    #[test]
    fn a_failed_check_swaps_nothing() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Verify]);

        let (result, log) = run(&setup, &steps, false);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_eq!(steps.calls(), ["wait 4242", "lock", "verify"]);
        assert_untouched(&setup);
        assert!(log.contains("the signature is wrong"), "{log}");
    }

    #[test]
    fn a_failed_check_opens_the_old_app_again() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Verify]);

        run(&setup, &steps, true).0.expect_err("failed");

        assert_eq!(steps.opened(), std::slice::from_ref(&setup.old));
    }

    #[test]
    fn a_failed_check_is_written_to_the_failure_file() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Verify]);

        run(&setup, &steps, false).0.expect_err("failed");

        let text = failure_file(&setup).expect("failure file");
        assert!(text.contains("the signature is wrong"), "{text}");
    }

    #[test]
    fn a_failed_swap_changes_nothing_and_opens_the_old_app_again() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Swap]);

        let (result, log) = run(&setup, &steps, true);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_untouched(&setup);
        assert_eq!(steps.opened(), std::slice::from_ref(&setup.old));
        assert!(!steps.calls().contains(&"trash".to_owned()));
        assert!(log.contains("nothing changed"), "{log}");
        assert!(failure_file(&setup).is_some());
    }

    #[test]
    fn a_trash_failure_after_the_swap_still_installs_the_app() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Trash]);

        let (result, log) = run(&setup, &steps, true);

        result.expect("installed");
        assert_eq!(mark(&setup.old), "new");
        assert_eq!(mark(&setup.new), "old");
        assert!(log.contains("the old app stays at"), "{log}");
        assert_eq!(failure_file(&setup), None);
    }

    #[test]
    fn an_installed_app_that_is_not_older_is_refused() {
        let setup = setup();
        let steps = FakeSteps::with_old(setup.trash.clone(), SWARM, Some(Version::new(1, 2, 0)));

        let (result, log) = run(&setup, &steps, false);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
        assert_untouched(&setup);
        assert!(log.contains("not older"), "{log}");
    }

    #[test]
    fn an_installed_app_without_a_version_is_refused() {
        let setup = setup();
        let steps = FakeSteps::with_old(setup.trash.clone(), SWARM, None);

        let (result, _) = run(&setup, &steps, false);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
    }

    #[test]
    fn an_old_path_that_is_not_swarm_is_refused_and_never_opened() {
        let setup = setup();
        let steps = FakeSteps::with_old(
            setup.trash.clone(),
            "com.other.app",
            Some(Version::new(1, 0, 0)),
        );

        let (result, log) = run(&setup, &steps, true);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
        assert!(steps.opened().is_empty());
        assert_untouched(&setup);
        assert!(log.contains("is not Swarm"), "{log}");
    }

    #[test]
    fn an_old_path_that_is_not_a_bundle_is_refused_and_never_opened() {
        let setup = setup();
        let mut steps = FakeSteps::new(setup.trash.clone());
        steps.old_info = None;

        let (result, _) = run(&setup, &steps, true);

        assert!(result.is_err());
        assert!(steps.opened().is_empty());
        assert_untouched(&setup);
    }

    #[test]
    fn a_new_app_outside_the_update_folder_is_refused() {
        let setup = setup();
        let elsewhere = setup.trash.join("Elsewhere/Swarm.app");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, log) = run_with(&request(&setup, &elsewhere, false), &steps);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
        assert_eq!(mark(&setup.old), "old");
        assert!(log.contains("is not in the update folder"), "{log}");
    }

    #[test]
    fn a_new_app_in_the_folder_of_another_version_is_refused() {
        let setup = setup();
        let other = setup.cache.join("updates/1.3.0/Swarm.app");
        std::fs::create_dir_all(&other).expect("other");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run_with(&request(&setup, &other, false), &steps);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
    }

    #[test]
    fn a_new_app_that_is_a_link_out_of_the_update_folder_is_refused() {
        let setup = setup();
        let outside = setup.trash.join("Outside.app");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::remove_dir_all(&setup.new).expect("remove");
        std::os::unix::fs::symlink(&outside, &setup.new).expect("link");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run(&setup, &steps, false);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
        assert_eq!(mark(&setup.old), "old");
    }

    #[test]
    fn the_update_folder_itself_is_not_a_new_app() {
        let setup = setup();
        let folder = setup.cache.join("updates/1.2.0");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run_with(&request(&setup, &folder, false), &steps);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock"]);
    }

    #[test]
    fn a_busy_update_lock_stops_the_install() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());
        let _held = UpdateLock::acquire(&setup.cache, Duration::from_secs(1)).expect("lock");

        let (result, log) = run(&setup, &steps, true);

        assert!(result.is_err());
        assert_eq!(steps.calls(), ["wait 4242", "lock", "open"]);
        assert_untouched(&setup);
        assert!(log.contains("another update is in progress"), "{log}");
    }

    #[test]
    fn a_missing_new_app_changes_nothing() {
        let setup = setup();
        std::fs::remove_dir_all(&setup.new).expect("remove");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run(&setup, &steps, false);

        assert!(result.is_err());
        assert!(!steps.calls().contains(&"swap".to_owned()));
        assert_eq!(mark(&setup.old), "old");
    }

    #[test]
    fn process_id_zero_is_refused() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());
        let request = InstallRequest {
            pid: 0,
            ..request(&setup, &setup.new, true)
        };

        let result = install(&request, &steps, &mut Vec::new());

        assert!(result.is_err());
        assert!(steps.calls().is_empty());
    }

    #[test]
    fn a_failed_reopen_is_an_error_after_the_install() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Open]);

        let (result, _) = run(&setup, &steps, true);

        assert!(result.is_err());
        assert_eq!(mark(&setup.old), "new");
        assert_eq!(failure_file(&setup), None);
    }

    #[test]
    fn this_process_exists_and_a_finished_one_does_not() {
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");

        assert!(process_exists(std::process::id()));
        assert!(!process_exists(pid));
    }

    #[test]
    fn waiting_for_a_finished_process_returns_at_once() {
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");

        assert!(SystemSteps.wait_for_exit(pid, Duration::from_secs(5)));
    }

    #[test]
    fn waiting_for_a_running_process_times_out() {
        assert!(!SystemSteps.wait_for_exit(std::process::id(), Duration::from_millis(300)));
    }

    #[test]
    fn waiting_sees_a_process_end() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        drop(child.stdin.take());

        let waiter =
            std::thread::spawn(move || SystemSteps.wait_for_exit(pid, Duration::from_secs(10)));
        child.wait().expect("wait");

        assert!(waiter.join().expect("thread"));
    }

    #[test]
    fn the_real_swap_exchanges_two_folders() {
        let setup = setup();

        SystemSteps.swap(&setup.new, &setup.old).expect("swap");

        assert_eq!(mark(&setup.old), "new");
        assert_eq!(mark(&setup.new), "old");
    }

    #[test]
    fn the_real_swap_with_a_missing_folder_changes_nothing() {
        let setup = setup();
        let missing = setup.trash.join("missing");

        let result = SystemSteps.swap(&missing, &setup.old);

        assert!(result.is_err());
        assert_eq!(mark(&setup.old), "old");
        assert!(!missing.exists());
    }

    #[test]
    fn a_swap_across_volumes_is_explained() {
        let error = swap_error(io::Error::from_raw_os_error(libc::EXDEV));

        assert!(error.to_string().contains("different volumes"), "{error}");
    }

    #[test]
    fn another_swap_error_stays_as_it_is() {
        let error = swap_error(io::Error::from_raw_os_error(libc::EACCES));

        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }

    #[test]
    fn the_real_verify_refuses_a_folder_that_is_not_an_app() {
        let setup = setup();

        let result = SystemSteps.verify(&setup.old, &Version::new(1, 2, 0));

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn the_real_lock_is_taken_in_the_cache() {
        let setup = setup();

        let lock = SystemSteps.lock(&setup.cache);

        assert!(lock.is_ok(), "{lock:?}");
        assert!(setup.cache.join("updates/.lock").is_file());
    }

    fn write_plist(app: &Path, body: &str) {
        std::fs::create_dir_all(app.join("Contents")).expect("contents");
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>{body}</dict></plist>"#
            ),
        )
        .expect("plist");
    }

    #[test]
    fn the_real_bundle_info_reads_the_identifier_and_the_version() {
        let folder = TempFolder::new("bundle-info");
        let app = folder.path().join("Swarm.app");
        write_plist(
            &app,
            "<key>CFBundleIdentifier</key><string>com.swarm.app</string>\
             <key>CFBundleShortVersionString</key><string>1.4.2</string>",
        );

        assert_eq!(
            SystemSteps.bundle_info(&app),
            Some(BundleInfo {
                identifier: SWARM.to_owned(),
                version: Some(Version::new(1, 4, 2)),
            })
        );
    }

    #[test]
    fn a_version_that_does_not_read_is_none() {
        let folder = TempFolder::new("bundle-bad-version");
        let app = folder.path().join("Swarm.app");
        write_plist(
            &app,
            "<key>CFBundleIdentifier</key><string>com.swarm.app</string>\
             <key>CFBundleShortVersionString</key><string>one</string>",
        );

        let info = SystemSteps.bundle_info(&app).expect("info");

        assert_eq!(info.version, None);
    }

    #[test]
    fn a_folder_without_a_plist_has_no_bundle_info() {
        let folder = TempFolder::new("bundle-none");

        assert_eq!(SystemSteps.bundle_info(folder.path()), None);
    }

    #[test]
    fn a_writable_bundle_allows_a_replace() {
        let setup = setup();

        assert!(can_replace(&setup.old));
    }

    #[test]
    fn a_read_only_folder_does_not_allow_a_replace() {
        let setup = setup();
        let locked = setup.trash.join("locked");
        std::fs::create_dir(&locked).expect("folder");
        std::fs::create_dir(locked.join("Swarm.app")).expect("bundle");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
            .expect("permissions");

        let allowed = can_replace(&locked.join("Swarm.app"));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("permissions");
        assert!(!allowed);
    }

    #[test]
    fn a_read_only_bundle_in_a_writable_folder_does_not_allow_a_replace() {
        let setup = setup();
        std::fs::set_permissions(&setup.old, std::fs::Permissions::from_mode(0o555))
            .expect("permissions");

        let allowed = can_replace(&setup.old);

        std::fs::set_permissions(&setup.old, std::fs::Permissions::from_mode(0o755))
            .expect("permissions");
        assert!(!allowed);
    }

    #[test]
    fn a_system_folder_does_not_allow_a_replace() {
        assert!(!can_replace(Path::new("/System/Applications/Swarm.app")));
    }

    /// A bundle whose `swarm` is a script that writes its arguments next to itself.
    fn script_bundle(folder: &TempFolder, script: &str) -> PathBuf {
        let bundle = folder.path().join("Swarm.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("bundle");
        std::fs::write(macos.join("swarm"), script).expect("script");
        std::fs::set_permissions(macos.join("swarm"), std::fs::Permissions::from_mode(0o755))
            .expect("executable");

        bundle
    }

    fn prepared_in(folder: &TempFolder) -> (Prepared, PathBuf) {
        let version_folder = folder.path().join("cache/updates/1.2.0");
        std::fs::create_dir_all(version_folder.join("Swarm.app")).expect("update");
        let prepared = Prepared {
            version: Version::new(1, 2, 0),
            app: version_folder.join("Swarm.app"),
            notes_url: String::new(),
            install_failure: None,
        };

        (prepared, version_folder)
    }

    fn wait_for(file: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !file.is_file() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }

    #[test]
    fn the_helper_is_started_with_the_update_arguments() {
        let folder = TempFolder::new("launch");
        let bundle = script_bundle(
            &folder,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\necho from-helper >&2\n",
        );
        let (prepared, version_folder) = prepared_in(&folder);

        launch_helper(&prepared, &bundle, 4242, true).expect("launched");

        let args = version_folder.join("args.txt");
        wait_for(&args);
        let expected = format!(
            "update-install --pid 4242 --old {} --new {} --version 1.2.0 --reopen\n",
            bundle.display(),
            prepared.app.display()
        );
        assert_eq!(std::fs::read_to_string(&args).expect("args"), expected);
        assert!(version_folder.join("swarm-installer-4242").is_file());
        assert_eq!(
            std::fs::read_to_string(version_folder.join("install-4242.log")).expect("log"),
            "from-helper\n"
        );
    }

    #[test]
    fn the_helper_is_started_without_reopen_when_not_asked() {
        let folder = TempFolder::new("launch-plain");
        let bundle = script_bundle(
            &folder,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\n",
        );
        let (prepared, version_folder) = prepared_in(&folder);

        launch_helper(&prepared, &bundle, 7, false).expect("launched");

        let args = version_folder.join("args.txt");
        wait_for(&args);
        let text = std::fs::read_to_string(&args).expect("args");
        assert!(!text.contains("--reopen"), "{text}");
    }

    #[test]
    fn two_apps_get_two_helpers_and_two_logs() {
        let folder = TempFolder::new("launch-two");
        let bundle = script_bundle(&folder, "#!/bin/sh\necho \"$$\" >&2\n");
        let (prepared, version_folder) = prepared_in(&folder);

        launch_helper(&prepared, &bundle, 11, false).expect("first");
        launch_helper(&prepared, &bundle, 12, false).expect("second");

        for name in [
            "swarm-installer-11",
            "swarm-installer-12",
            "install-11.log",
            "install-12.log",
        ] {
            assert!(version_folder.join(name).is_file(), "{name}");
        }
    }

    #[test]
    fn no_staged_copy_of_the_helper_is_left_behind() {
        let folder = TempFolder::new("launch-staged");
        let bundle = script_bundle(&folder, "#!/bin/sh\n");
        let (prepared, version_folder) = prepared_in(&folder);

        launch_helper(&prepared, &bundle, 21, false).expect("launched");

        assert!(!version_folder.join(".swarm-installer-21.tmp").exists());
    }

    #[test]
    fn a_bundle_without_swarm_cannot_launch_a_helper() {
        let folder = TempFolder::new("launch-missing");
        let (prepared, version_folder) = prepared_in(&folder);

        let result = launch_helper(&prepared, &folder.path().join("Swarm.app"), 1, false);

        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
        assert!(!version_folder.join(".swarm-installer-1.tmp").exists());
    }
}
