//! Swaps the app bundle after the app has quit (PLAN.md, Updates, Install).
//!
//! A copy of the `swarm` binary does the swap, because the app cannot replace
//! itself while it runs.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::{Error, Prepared, Result};

/// How long the helper waits for the app to quit.
const EXIT_WAIT: Duration = Duration::from_secs(120);

/// How often the helper looks if the app has quit.
const POLL: Duration = Duration::from_millis(200);

/// What `swarm update-install` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The app that must quit first.
    pub pid: u32,
    /// The installed `Swarm.app`, which goes to the Trash.
    pub old: &'a Path,
    /// The checked `Swarm.app` that takes its place.
    pub new: &'a Path,
    /// Open the new app when done.
    pub reopen: bool,
}

/// The steps that touch the system, so that tests can run [`install`] on a
/// temporary folder.
pub trait InstallSteps {
    /// Waits until the process is gone. `false` when `timeout` passed first.
    fn wait_for_exit(&self, pid: u32, timeout: Duration) -> bool;

    /// Moves a folder to the Trash and says where it went.
    fn move_to_trash(&self, path: &Path) -> io::Result<PathBuf>;

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Opens a new instance of the app.
    fn open(&self, app: &Path) -> io::Result<()>;
}

/// Replaces `old` by `new`, step by step, and writes each step to `log`.
///
/// If the old app cannot go to the Trash, nothing changes. If the new app
/// cannot take its place, the old app is moved back.
pub fn install(
    request: &InstallRequest<'_>,
    steps: &impl InstallSteps,
    log: &mut impl Write,
) -> Result<()> {
    let InstallRequest {
        pid,
        old,
        new,
        reopen,
    } = *request;

    if pid == 0 {
        return Err(fail(log, "the process id is not valid"));
    }
    if !new.is_dir() {
        return Err(fail(log, &format!("{} does not exist", new.display())));
    }

    note(log, &format!("waiting for process {pid} to exit"));
    if !steps.wait_for_exit(pid, EXIT_WAIT) {
        return Err(fail(log, "the app did not exit in time, nothing changed"));
    }

    note(log, &format!("moving {} to the Trash", old.display()));
    let trashed = match steps.move_to_trash(old) {
        Ok(trashed) => trashed,
        Err(error) => {
            let message = format!("cannot move the old app to the Trash, nothing changed: {error}");

            return Err(fail(log, &message));
        }
    };
    note(log, &format!("the old app is now at {}", trashed.display()));

    note(
        log,
        &format!("moving {} to {}", new.display(), old.display()),
    );
    if let Err(error) = steps.rename(new, old) {
        note(log, &format!("cannot move the new app: {error}"));

        return Err(put_back(steps, &trashed, old, log));
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

/// The new app could not take its place: the old app goes back.
fn put_back(steps: &impl InstallSteps, trashed: &Path, old: &Path, log: &mut impl Write) -> Error {
    match steps.rename(trashed, old) {
        Ok(()) => fail(log, "the new app was not installed, the old app is back"),
        Err(error) => {
            let message = format!(
                "the new app was not installed and the old app could not be put back ({error}): it is at {}",
                trashed.display()
            );

            fail(log, &message)
        }
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

/// The real steps: `kill(pid, 0)`, `NSFileManager`, `rename` and `open`.
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

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn open(&self, app: &Path) -> io::Result<()> {
        let status = Command::new("/usr/bin/open").arg("-n").arg(app).status()?;

        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("open exited with {status}")))
        }
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

/// The folder of the bundle is writable, so the app can be replaced.
pub fn can_replace(bundle: &Path) -> bool {
    let Some(parent) = bundle.parent() else {
        return false;
    };
    let Ok(parent) = CString::new(parent.as_os_str().as_bytes()) else {
        return false;
    };

    // SAFETY: `parent` is a valid NUL-terminated string that lives through the call.
    unsafe { libc::access(parent.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// Starts `swarm update-install` as a separate process, with no ties to the
/// app: it keeps running after the app quits.
///
/// The helper is a copy of the running bundle's `swarm`, in the update folder.
/// Its log is `install.log` next to it. After this returns `Ok`, the app should
/// quit; the helper waits for `pid` to exit.
pub fn launch_helper(prepared: &Prepared, bundle: &Path, pid: u32, reopen: bool) -> Result<()> {
    let folder = prepared
        .app
        .parent()
        .ok_or_else(|| Error::Install("the update has no folder".to_owned()))?;

    let helper = folder.join("swarm-installer");
    std::fs::copy(bundle.join("Contents/MacOS/swarm"), &helper)
        .map_err(|error| Error::io("cannot copy the install helper", error))?;
    let log = File::create(folder.join("install.log"))
        .map_err(|error| Error::io("cannot make the install log", error))?;

    let mut command = Command::new(&helper);
    command
        .arg("update-install")
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--old")
        .arg(bundle)
        .arg("--new")
        .arg(&prepared.app);
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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::Version;
    use crate::test_support::TempFolder;

    /// Moves "to the Trash" into a folder of the test, and can fail on request.
    #[derive(PartialEq)]
    enum Failure {
        AppNeverExits,
        Trash,
        /// Moving the new app to the old path.
        InstallRename,
        /// Moving the old app back from the Trash.
        RestoreRename,
        Open,
    }

    struct FakeSteps {
        trash: PathBuf,
        failures: Vec<Failure>,
        calls: RefCell<Vec<String>>,
        opened: Cell<u32>,
    }

    impl FakeSteps {
        fn new(trash: PathBuf) -> Self {
            Self {
                trash,
                failures: Vec::new(),
                calls: RefCell::default(),
                opened: Cell::new(0),
            }
        }

        fn failing(trash: PathBuf, failures: Vec<Failure>) -> Self {
            Self {
                failures,
                ..Self::new(trash)
            }
        }

        fn fails(&self, failure: &Failure) -> bool {
            self.failures.contains(failure)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl InstallSteps for FakeSteps {
        fn wait_for_exit(&self, pid: u32, _timeout: Duration) -> bool {
            self.calls.borrow_mut().push(format!("wait {pid}"));

            !self.fails(&Failure::AppNeverExits)
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

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("rename".to_owned());
            let restoring = from.starts_with(&self.trash);
            if restoring && self.fails(&Failure::RestoreRename) {
                return Err(io::Error::other("cannot restore"));
            }
            if !restoring && self.fails(&Failure::InstallRename) {
                return Err(io::Error::other("cannot rename"));
            }

            std::fs::rename(from, to)
        }

        fn open(&self, _app: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("open".to_owned());
            self.opened.set(self.opened.get() + 1);
            if self.fails(&Failure::Open) {
                return Err(io::Error::other("cannot open"));
            }

            Ok(())
        }
    }

    /// An "installed" app, a downloaded one and a trash folder.
    struct Setup {
        _folder: TempFolder,
        old: PathBuf,
        new: PathBuf,
        trash: PathBuf,
    }

    fn setup() -> Setup {
        let folder = TempFolder::new("install");
        let old = folder.path().join("Applications/Swarm.app");
        let new = folder.path().join("cache/Swarm.app");
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
        }
    }

    fn run(setup: &Setup, steps: &FakeSteps, reopen: bool) -> (Result<()>, String) {
        let mut log = Vec::new();
        let request = InstallRequest {
            pid: 4242,
            old: &setup.old,
            new: &setup.new,
            reopen,
        };

        let result = install(&request, steps, &mut log);

        (result, String::from_utf8(log).expect("utf8 log"))
    }

    fn mark(app: &Path) -> String {
        std::fs::read_to_string(app.join("mark")).expect("mark")
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

        assert_eq!(steps.calls(), ["wait 4242", "trash", "rename", "open"]);
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

        assert_eq!((without.opened.get(), with.opened.get()), (0, 1));
    }

    #[test]
    fn a_trash_failure_leaves_both_apps_untouched() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::Trash]);

        let (result, log) = run(&setup, &steps, true);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_eq!(mark(&setup.old), "old");
        assert_eq!(mark(&setup.new), "new");
        assert_eq!(steps.opened.get(), 0);
        assert!(log.contains("trash is full"), "{log}");
    }

    #[test]
    fn a_rename_failure_puts_the_old_app_back() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::InstallRename]);

        let (result, log) = run(&setup, &steps, true);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_eq!(mark(&setup.old), "old");
        assert_eq!(mark(&setup.new), "new");
        assert!(!setup.trash.join("Swarm.app").exists());
        assert_eq!(steps.opened.get(), 0);
        assert!(log.contains("the old app is back"), "{log}");
    }

    #[test]
    fn a_failed_restore_says_where_the_old_app_is() {
        let setup = setup();
        let steps = FakeSteps::failing(
            setup.trash.clone(),
            vec![Failure::InstallRename, Failure::RestoreRename],
        );

        let (result, log) = run(&setup, &steps, false);

        assert!(result.is_err());
        assert_eq!(mark(&setup.trash.join("Swarm.app")), "old");
        assert!(log.contains(&setup.trash.display().to_string()), "{log}");
    }

    #[test]
    fn an_app_that_does_not_exit_changes_nothing() {
        let setup = setup();
        let steps = FakeSteps::failing(setup.trash.clone(), vec![Failure::AppNeverExits]);

        let (result, _) = run(&setup, &steps, true);

        assert!(matches!(result, Err(Error::Install(_))), "{result:?}");
        assert_eq!(steps.calls(), ["wait 4242"]);
        assert_eq!(mark(&setup.old), "old");
    }

    #[test]
    fn a_missing_new_app_changes_nothing() {
        let setup = setup();
        std::fs::remove_dir_all(&setup.new).expect("remove");
        let steps = FakeSteps::new(setup.trash.clone());

        let (result, _) = run(&setup, &steps, true);

        assert!(result.is_err());
        assert!(steps.calls().is_empty());
        assert_eq!(mark(&setup.old), "old");
    }

    #[test]
    fn process_id_zero_is_refused() {
        let setup = setup();
        let steps = FakeSteps::new(setup.trash.clone());
        let request = InstallRequest {
            pid: 0,
            old: &setup.old,
            new: &setup.new,
            reopen: false,
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
    fn the_real_rename_moves_a_folder() {
        let setup = setup();
        let target = setup.trash.join("moved");

        SystemSteps.rename(&setup.new, &target).expect("rename");

        assert_eq!(mark(&target), "new");
    }

    #[test]
    fn a_writable_folder_allows_a_replace() {
        let setup = setup();

        assert!(can_replace(&setup.old));
    }

    #[test]
    fn a_read_only_folder_does_not_allow_a_replace() {
        let setup = setup();
        let locked = setup.trash.join("locked");
        std::fs::create_dir(&locked).expect("folder");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
            .expect("permissions");

        let allowed = can_replace(&locked.join("Swarm.app"));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("permissions");
        assert!(!allowed);
    }

    #[test]
    fn a_system_folder_does_not_allow_a_replace() {
        assert!(!can_replace(Path::new("/System/Applications/Swarm.app")));
    }

    #[test]
    fn the_helper_is_started_with_the_update_arguments() {
        let folder = TempFolder::new("launch");
        let bundle = folder.path().join("Swarm.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("bundle");
        std::fs::write(
            macos.join("swarm"),
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\necho from-helper >&2\n",
        )
        .expect("script");
        std::fs::set_permissions(macos.join("swarm"), std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let version_folder = folder.path().join("updates/1.2.0");
        std::fs::create_dir_all(version_folder.join("Swarm.app")).expect("update");
        let prepared = Prepared {
            version: Version::new(1, 2, 0),
            app: version_folder.join("Swarm.app"),
            notes_url: String::new(),
        };

        launch_helper(&prepared, &bundle, 4242, true).expect("launched");

        let args = version_folder.join("args.txt");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !args.is_file() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        let expected = format!(
            "update-install --pid 4242 --old {} --new {} --reopen\n",
            bundle.display(),
            prepared.app.display()
        );
        assert_eq!(std::fs::read_to_string(&args).expect("args"), expected);
        assert!(version_folder.join("swarm-installer").is_file());
        assert_eq!(
            std::fs::read_to_string(version_folder.join("install.log")).expect("log"),
            "from-helper\n"
        );
    }

    #[test]
    fn the_helper_is_started_without_reopen_when_not_asked() {
        let folder = TempFolder::new("launch-plain");
        let bundle = folder.path().join("Swarm.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("bundle");
        std::fs::write(
            macos.join("swarm"),
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\n",
        )
        .expect("script");
        std::fs::set_permissions(macos.join("swarm"), std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let version_folder = folder.path().join("updates/1.2.0");
        std::fs::create_dir_all(version_folder.join("Swarm.app")).expect("update");
        let prepared = Prepared {
            version: Version::new(1, 2, 0),
            app: version_folder.join("Swarm.app"),
            notes_url: String::new(),
        };

        launch_helper(&prepared, &bundle, 7, false).expect("launched");

        let args = version_folder.join("args.txt");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !args.is_file() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        let text = std::fs::read_to_string(&args).expect("args");
        assert!(!text.contains("--reopen"), "{text}");
    }

    #[test]
    fn a_bundle_without_swarm_cannot_launch_a_helper() {
        let folder = TempFolder::new("launch-missing");
        let prepared = Prepared {
            version: Version::new(1, 2, 0),
            app: folder.path().join("updates/1.2.0/Swarm.app"),
            notes_url: String::new(),
        };
        std::fs::create_dir_all(folder.path().join("updates/1.2.0")).expect("folder");

        let result = launch_helper(&prepared, &folder.path().join("Swarm.app"), 1, false);

        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
    }
}
