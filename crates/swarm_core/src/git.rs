//! Runs the `git` binary. User text only ever travels as an argument, never through a shell.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use swarm_protocol::DiffStat;

use crate::Error;

/// Apps started from Finder get a minimal `PATH`, so known install folders come second.
const FALLBACK_BINARIES: [&str; 3] = [
    "/usr/bin/git",
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
];

const FALLBACK_BASE_BRANCH: &str = "main";

fn binary() -> Result<&'static Path, Error> {
    static BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();

    BINARY
        .get_or_init(find_binary)
        .as_deref()
        .ok_or(Error::GitMissing)
}

fn find_binary() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|paths| {
        std::env::split_paths(&paths)
            .map(|folder| folder.join("git"))
            .collect::<Vec<_>>()
    });
    let known = FALLBACK_BINARIES.iter().map(PathBuf::from);

    on_path.chain(known).find(|candidate| candidate.is_file())
}

struct Output {
    success: bool,
    code: Option<i32>,
    stdout: String,
    /// Untouched, for NUL-separated output.
    raw: Vec<u8>,
    stderr: String,
}

fn execute<I, S>(directory: &Path, arguments: I) -> Result<(Vec<OsString>, Output), Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments: Vec<OsString> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();

    let output = Command::new(binary()?)
        .arg("-C")
        .arg(directory)
        .args(&arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|source| Error::Io {
            action: "run git".to_owned(),
            source,
        })?;

    let output = Output {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        raw: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    };

    Ok((arguments, output))
}

fn failure(arguments: &[OsString], output: &Output) -> Error {
    let command = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    Error::Git {
        command,
        stderr: output.stderr.clone(),
    }
}

/// Runs git in `directory` and returns trimmed stdout.
fn run<I, S>(directory: &Path, arguments: I) -> Result<String, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (arguments, output) = execute(directory, arguments)?;

    if !output.success {
        return Err(failure(&arguments, &output));
    }

    Ok(output.stdout)
}

/// Larger untracked files count as one file with no lines.
const MAX_COUNTED_BYTES: u64 = 1 << 20;

/// Git treats a file as binary when a NUL shows up in its first 8000 bytes.
const BINARY_SNIFF: usize = 8000;

/// Untracked files past this many get no line count.
const MAX_COUNTED_FILES: usize = 2000;

/// Untracked files past this many are not looked at.
const MAX_UNTRACKED_FILES: usize = 10_000;

/// Bytes read from untracked files per call; later files get no line count.
const MAX_TOTAL_BYTES: u64 = 32 << 20;

/// The commit `revision` points at.
pub fn commit_of(root: &Path, revision: &str) -> Result<String, Error> {
    let target = format!("{revision}^{{commit}}");

    run(root, ["rev-parse", "--verify", "--end-of-options", &target])
}

/// The commit where `HEAD` and `revision` split.
pub fn merge_base(worktree: &Path, revision: &str) -> Result<String, Error> {
    run(
        worktree,
        ["merge-base", "--end-of-options", "HEAD", revision],
    )
}

/// What the working tree of `worktree` changed since `base`: committed and uncommitted
/// work, plus untracked files. A binary or huge untracked file counts as a file with no
/// lines.
pub fn diff_stat(worktree: &Path, base: &str) -> Result<DiffStat, Error> {
    // `diff-index` never writes the index, so it cannot block the agent's own git.
    let (arguments, output) = execute(
        worktree,
        [
            "diff-index",
            "--numstat",
            "-z",
            "--no-renames",
            "--end-of-options",
            base,
            "--",
        ],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    let mut files: HashSet<String> = HashSet::new();
    let (mut additions, mut deletions) = (0_u32, 0_u32);

    for entry in output
        .raw
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf8_lossy(entry);
        let mut parts = entry.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        additions = additions.saturating_add(added.parse().unwrap_or(0));
        deletions = deletions.saturating_add(removed.parse().unwrap_or(0));
        files.insert(path.to_owned());
    }

    let (arguments, output) = execute(
        worktree,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    let mut budget = MAX_TOTAL_BYTES;
    for (index, path) in output
        .raw
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .take(MAX_UNTRACKED_FILES)
        .enumerate()
    {
        let path = String::from_utf8_lossy(path).into_owned();
        if index < MAX_COUNTED_FILES {
            additions = additions.saturating_add(count_lines(&worktree.join(&path), &mut budget));
        }
        files.insert(path);
    }

    Ok(DiffStat {
        files: u32::try_from(files.len()).unwrap_or(u32::MAX),
        additions,
        deletions,
    })
}

/// Lines in a text file. A symlink is one line, like git sees it. Anything that is not a
/// regular file, or is binary, huge, unreadable or past the byte `budget`, has none.
fn count_lines(path: &Path, budget: &mut u64) -> u32 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 1;
    }
    if !meta.is_file() || meta.len() > MAX_COUNTED_BYTES || *budget == 0 {
        return 0;
    }

    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };

    // The file may grow after the check above.
    let mut bytes = Vec::new();
    if file
        .take(MAX_COUNTED_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return 0;
    }

    *budget = budget.saturating_sub(bytes.len() as u64);
    if bytes.len() as u64 > MAX_COUNTED_BYTES {
        return 0;
    }
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        return 0;
    }

    let newlines = bytes.split(|byte| *byte == b'\n').count() - 1;
    let unterminated = bytes.last().is_some_and(|byte| *byte != b'\n');

    u32::try_from(newlines + usize::from(unterminated)).unwrap_or(u32::MAX)
}

/// The top folder of the repository that contains `path`.
pub fn repository_root(path: &Path) -> Result<PathBuf, Error> {
    let root = run(path, ["rev-parse", "--show-toplevel"])?;

    Ok(PathBuf::from(root))
}

/// The configured branch, else the branch `origin/HEAD` points at, else the current
/// branch, else `main`.
pub fn default_base_branch(root: &Path, configured: Option<&str>) -> String {
    if let Some(branch) = configured.filter(|branch| !branch.is_empty()) {
        return branch.to_owned();
    }

    let remote = run(
        root,
        ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|reference| reference.strip_prefix("origin/").map(str::to_owned))
    .filter(|branch| !branch.is_empty());
    if let Some(branch) = remote {
        return branch;
    }

    run(root, ["branch", "--show-current"])
        .ok()
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| FALLBACK_BASE_BRANCH.to_owned())
}

pub fn branch_exists(root: &Path, branch: &str) -> Result<bool, Error> {
    let reference = format!("refs/heads/{branch}");
    let (arguments, output) = execute(root, ["show-ref", "--verify", "--quiet", &reference])?;

    match (output.success, output.code) {
        (true, _) => Ok(true),
        (false, Some(1)) => Ok(false),
        (false, _) => Err(failure(&arguments, &output)),
    }
}

/// Refuses a `base` that git would read as an option, or that is not a commit.
pub fn verify_start_point(root: &Path, base: &str) -> Result<(), Error> {
    if base.starts_with('-') {
        return Err(Error::Invalid(format!(
            "the start point {base:?} must not start with \"-\""
        )));
    }

    let revision = format!("{base}^{{commit}}");
    let (_, output) = execute(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &revision,
        ],
    )?;

    if !output.success {
        return Err(Error::Invalid(format!(
            "{base:?} is not a branch or a commit in this project"
        )));
    }

    Ok(())
}

/// Makes `branch` from `from` and checks it out in a new worktree at `path`.
pub fn worktree_add(root: &Path, branch: &str, path: &Path, from: &str) -> Result<(), Error> {
    let arguments = [
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("-b"),
        OsStr::new(branch),
        OsStr::new("--end-of-options"),
        path.as_os_str(),
        OsStr::new(from),
    ];
    run(root, arguments)?;

    Ok(())
}

pub fn worktree_remove(root: &Path, path: &Path) -> Result<(), Error> {
    let arguments = [
        OsStr::new("worktree"),
        OsStr::new("remove"),
        OsStr::new("--force"),
        OsStr::new("--end-of-options"),
        path.as_os_str(),
    ];
    run(root, arguments)?;

    Ok(())
}

/// The git folders a commit in a linked `worktree` writes: the shared objects, refs and
/// reflogs, and the worktree's own folder. Not the shared folder itself: hooks and the
/// config in it would run outside the sandbox. Empty for a main worktree.
pub fn writable_git_dirs(worktree: &Path) -> Result<Vec<PathBuf>, Error> {
    let shared = run(
        worktree,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let own = run(
        worktree,
        ["rev-parse", "--path-format=absolute", "--git-dir"],
    )?;
    if own == shared {
        return Ok(Vec::new());
    }

    let shared = PathBuf::from(shared);
    let mut folders: Vec<PathBuf> = ["objects", "refs", "logs"]
        .iter()
        .map(|folder| shared.join(folder))
        .filter(|folder| folder.is_dir())
        .collect();
    folders.push(PathBuf::from(own));

    Ok(folders)
}

/// How a merge ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// False when git stopped on conflicts and left the merge for the caller.
    pub merged: bool,
    pub conflicts: Vec<PathBuf>,
    /// What git printed.
    pub output: String,
}

/// Merges `branch` into the branch checked out in `worktree`, always with a merge commit.
/// A conflict is an outcome; any other git failure is an error.
pub fn merge_branch(worktree: &Path, branch: &str) -> Result<MergeOutcome, Error> {
    let (arguments, output) = execute(
        worktree,
        ["merge", "--no-ff", "--no-edit", "--end-of-options", branch],
    )?;
    let printed = [output.stdout.as_str(), output.stderr.as_str()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");

    if output.success {
        return Ok(MergeOutcome {
            merged: true,
            conflicts: Vec::new(),
            output: printed,
        });
    }

    let conflicts: Vec<PathBuf> = run(worktree, ["diff", "--name-only", "--diff-filter=U"])?
        .lines()
        .map(PathBuf::from)
        .collect();
    if conflicts.is_empty() {
        let command = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");

        return Err(Error::Invalid(format!("git {command} failed: {printed}")));
    }

    Ok(MergeOutcome {
        merged: false,
        conflicts,
        output: printed,
    })
}

pub fn branch_delete(root: &Path, branch: &str) -> Result<(), Error> {
    run(root, ["branch", "-D", "--end-of-options", branch])?;

    Ok(())
}

/// Adds `line` to `.git/info/exclude` unless it is already there. The file is shared
/// by linked worktrees, so it lives in the common git folder.
pub fn add_exclude_line(root: &Path, line: &str) -> Result<(), Error> {
    let common = PathBuf::from(run(root, ["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };

    let info = common.join("info");
    let path = info.join("exclude");
    let io_error = |action: &str| {
        let action = format!("{action} {}", path.display());
        move |source| Error::Io { action, source }
    };

    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(io_error("read")(error)),
    };
    if existing.lines().any(|existing_line| existing_line == line) {
        return Ok(());
    }

    std::fs::create_dir_all(&info).map_err(io_error("create the folder of"))?;

    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut file = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io_error("open"))?;
    writeln!(file, "{separator}{line}").map_err(io_error("write"))?;

    Ok(())
}
