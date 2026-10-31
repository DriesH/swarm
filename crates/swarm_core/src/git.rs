//! Runs the `git` binary. User text only ever travels as an argument, never through a shell.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use swarm_protocol::{DiffStat, FILES_LISTED, FileStat};

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
    execute_with(directory, arguments, &[])
}

fn execute_with<I, S>(
    directory: &Path,
    arguments: I,
    environment: &[(&str, &str)],
) -> Result<(Vec<OsString>, Output), Error>
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
        .envs(environment.iter().copied())
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
    diff_files(worktree, base).map(|(stat, _)| stat)
}

/// The totals of [`diff_stat`] and the changed files, most changed lines first. The list
/// stops at [`FILES_LISTED`] files; the totals count all of them.
pub fn diff_files(worktree: &Path, base: &str) -> Result<(DiffStat, Vec<FileStat>), Error> {
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

    // Keyed by the raw bytes, so names that are not UTF-8 never collide.
    let mut files: HashMap<Vec<u8>, (u32, u32)> = HashMap::new();
    let (mut additions, mut deletions) = (0_u32, 0_u32);

    add_numstat(&output.raw, &mut files, &mut additions, &mut deletions);

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
        let mut lines = 0;
        if index < MAX_COUNTED_FILES {
            lines = count_lines(&worktree.join(OsStr::from_bytes(path)), &mut budget);
        }

        additions = additions.saturating_add(lines);

        // A path deleted from the index but still on disk is in both lists.
        let entry = files.entry(path.to_vec()).or_default();
        entry.0 = entry.0.saturating_add(lines);
    }

    let stat = DiffStat {
        files: u32::try_from(files.len()).unwrap_or(u32::MAX),
        additions,
        deletions,
    };

    Ok((stat, listed_files(files)))
}

/// A numstat count; binary files print `-`.
fn parse_count(count: &[u8]) -> u32 {
    std::str::from_utf8(count)
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(0)
}

/// Most changed lines first, ties by path so the order is stable. A name that is not
/// UTF-8 is listed with replacement characters, because the wire format needs UTF-8.
fn listed_files(files: HashMap<Vec<u8>, (u32, u32)>) -> Vec<FileStat> {
    let mut sorted: Vec<(Vec<u8>, (u32, u32))> = files.into_iter().collect();

    sorted.sort_by(|(one_path, one), (other_path, other)| {
        let changed = |(added, removed): &(u32, u32)| u64::from(*added) + u64::from(*removed);

        changed(other)
            .cmp(&changed(one))
            .then_with(|| one_path.cmp(other_path))
    });
    sorted.truncate(FILES_LISTED);

    sorted
        .into_iter()
        .map(|(path, (additions, deletions))| FileStat {
            path: PathBuf::from(String::from_utf8_lossy(&path).into_owned()),
            additions,
            deletions,
        })
        .collect()
}

/// Adds the entries of `numstat -z` output to the totals and to each file. A file can
/// appear once per commit, so its counts add up.
fn add_numstat(
    raw: &[u8],
    files: &mut HashMap<Vec<u8>, (u32, u32)>,
    additions: &mut u32,
    deletions: &mut u32,
) {
    for entry in raw
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let mut parts = entry.splitn(3, |byte| *byte == b'\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        let (added, removed) = (parse_count(added), parse_count(removed));

        *additions = additions.saturating_add(added);
        *deletions = deletions.saturating_add(removed);

        let file = files.entry(path.to_vec()).or_default();
        file.0 = file.0.saturating_add(added);
        file.1 = file.1.saturating_add(removed);
    }
}

/// What the commits since `base` that carry `Swarm-Agent: <agent>` changed. Work that
/// is not committed yet, and commits of other agents, do not count.
pub fn trailer_stat(
    worktree: &Path,
    base: &str,
    agent: &str,
) -> Result<(DiffStat, Vec<FileStat>), Error> {
    let pattern = format!("--grep=^Swarm-Agent: {}$", regex_escape(agent));
    let range = format!("{base}..HEAD");

    let (arguments, output) = execute(
        worktree,
        [
            "log",
            "--numstat",
            "-z",
            "--format=",
            "--no-renames",
            &pattern,
            "--end-of-options",
            &range,
        ],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    let mut files: HashMap<Vec<u8>, (u32, u32)> = HashMap::new();
    let (mut additions, mut deletions) = (0_u32, 0_u32);
    add_numstat(&output.raw, &mut files, &mut additions, &mut deletions);

    let stat = DiffStat {
        files: u32::try_from(files.len()).unwrap_or(u32::MAX),
        additions,
        deletions,
    };

    Ok((stat, listed_files(files)))
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::new();

    for character in text.chars() {
        if "\\.[]*^$".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }

    escaped
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

/// One commit at a time per worktree, so two agents never race on `index.lock`.
fn commit_lock(worktree: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    Arc::clone(locks.entry(worktree.to_path_buf()).or_default())
}

/// Checks that every path is a plain relative path inside `worktree` and not in `.git`.
fn check_commit_paths(worktree: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    if paths.is_empty() {
        return Err(Error::Invalid(
            "name the files to commit; committing everything is not allowed".to_owned(),
        ));
    }

    let real_worktree = worktree.canonicalize().map_err(|source| Error::Io {
        action: format!("read the worktree {}", worktree.display()),
        source,
    })?;

    for path in paths {
        let shown = path.display();

        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(Error::Invalid(format!(
                "{shown:?} must be a path relative to your worktree"
            )));
        }

        for component in path.components() {
            match component {
                Component::Normal(name) if name.eq_ignore_ascii_case(".git") => {
                    return Err(Error::Invalid(format!("{shown:?} is inside .git")));
                }
                Component::Normal(_) | Component::CurDir => {}
                _ => {
                    return Err(Error::Invalid(format!(
                        "{shown:?} must stay inside your worktree (no \"..\")"
                    )));
                }
            }
        }

        let parent = worktree.join(path.parent().unwrap_or(Path::new("")));
        if let Ok(real_parent) = parent.canonicalize()
            && !real_parent.starts_with(&real_worktree)
        {
            return Err(Error::Invalid(format!(
                "{shown:?} leads outside your worktree"
            )));
        }
    }

    Ok(())
}

/// Commits `paths` and nothing else in `worktree`. The author is `<agent> (Swarm)`, and
/// a `Swarm-Agent` trailer names the agent. Returns the new commit.
pub fn commit_paths(
    worktree: &Path,
    agent: &str,
    message: &str,
    paths: &[PathBuf],
) -> Result<String, Error> {
    if message.trim().is_empty() {
        return Err(Error::Invalid("the commit message is empty".to_owned()));
    }
    check_commit_paths(worktree, paths)?;

    let key = worktree.canonicalize().map_err(|source| Error::Io {
        action: format!("read the worktree {}", worktree.display()),
        source,
    })?;
    let lock = commit_lock(&key);
    let _held = lock.lock().unwrap_or_else(PoisonError::into_inner);

    let author = format!("{agent} (Swarm)");
    let email = run(worktree, ["config", "user.email"])
        .ok()
        .filter(|email| !email.is_empty());
    let mut environment = vec![
        ("GIT_LITERAL_PATHSPECS", "1"),
        ("GIT_AUTHOR_NAME", author.as_str()),
    ];
    if let Some(email) = &email {
        environment.push(("GIT_AUTHOR_EMAIL", email.as_str()));
    }

    let mut add: Vec<&OsStr> = vec![OsStr::new("add"), OsStr::new("--end-of-options")];
    add.extend(paths.iter().map(|path| path.as_os_str()));
    git_step(worktree, &add, &environment)?;

    let trailer = format!("Swarm-Agent: {agent}");
    let mut commit: Vec<&OsStr> = vec![
        OsStr::new("commit"),
        OsStr::new("-m"),
        OsStr::new(message),
        OsStr::new("--trailer"),
        OsStr::new(&trailer),
        OsStr::new("--"),
    ];
    commit.extend(paths.iter().map(|path| path.as_os_str()));
    git_step(worktree, &commit, &environment)?;

    run(worktree, ["rev-parse", "--verify", "HEAD"])
}

/// Runs one git step; a failure becomes the text git printed, for the agent to act on.
fn git_step(
    worktree: &Path,
    arguments: &[&OsStr],
    environment: &[(&str, &str)],
) -> Result<(), Error> {
    let (arguments, output) = execute_with(worktree, arguments, environment)?;
    if output.success {
        return Ok(());
    }

    let printed = [output.stderr.as_str(), output.stdout.as_str()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let command = arguments
        .first()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());

    Err(Error::Invalid(format!("git {command} failed: {printed}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_are_not_utf8_stay_separate_entries() {
        let mut files = HashMap::new();
        files.insert(vec![b'a', 0xff], (1, 0));
        files.insert(vec![b'a', 0xfe], (2, 0));
        files.insert(b"a.txt".to_vec(), (3, 0));

        let listed = listed_files(files);

        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].path, PathBuf::from("a.txt"));
        assert_eq!(listed[1].path, PathBuf::from("a\u{fffd}"));
    }
}
