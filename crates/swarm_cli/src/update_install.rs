//! `swarm update-install`: a hidden command that the app starts to swap its own
//! bundle after it has quit (ADR 0010). It is a copy of this binary in the update
//! folder. Its log is its standard error.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use swarm_update::{InstallRequest, SystemSteps, Version};

pub(crate) fn run(pid: u32, old: &Path, new: &Path, version: Version, reopen: bool) -> ExitCode {
    let mut stderr = std::io::stderr();

    let Some(cache_root) = swarm_update::cache_root() else {
        writeln!(
            stderr,
            "swarm: cannot find the cache folder: HOME is not set"
        )
        .ok();

        return ExitCode::FAILURE;
    };
    let request = InstallRequest {
        pid,
        old,
        new,
        version,
        cache_root: &cache_root,
        reopen,
    };

    match swarm_update::install(&request, &SystemSteps, &mut stderr) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            writeln!(stderr, "swarm: {error}").ok();

            ExitCode::FAILURE
        }
    }
}
