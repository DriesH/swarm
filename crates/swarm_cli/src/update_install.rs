//! `swarm update-install`: a hidden command that the app starts to swap its own
//! bundle after it has quit (ADR 0010). It is a copy of this binary in the update
//! folder. Its log is its standard error.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use swarm_update::{InstallRequest, SystemSteps};

pub(crate) fn run(pid: u32, old: &Path, new: &Path, reopen: bool) -> ExitCode {
    let request = InstallRequest {
        pid,
        old,
        new,
        reopen,
    };

    match swarm_update::install(&request, &SystemSteps, &mut std::io::stderr()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            writeln!(std::io::stderr(), "swarm: {error}").ok();

            ExitCode::FAILURE
        }
    }
}
