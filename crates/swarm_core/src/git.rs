//! Runs the `git` binary. User text only ever travels as an argument, never through a shell.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use swarm_protocol::{
    DIFF_FILES, DIFF_LINE_BYTES, DIFF_LINES, DIFF_LINES_PER_FILE, DiffHunk, DiffLine, DiffLineKind,
    DiffStat, FILES_LISTED, FileChange, FileDiff, FileStat,
};

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

/// A stable 64-bit hash (FNV-1a). `DefaultHasher` may change between Rust releases, and
/// fingerprints must survive a daemon restart.
#[derive(Debug, Clone, Copy)]
pub struct Fingerprint(u64);

impl Default for Fingerprint {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fingerprint {
    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    pub fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    /// Writes the length first, so `("ab", "c")` and `("a", "bc")` differ.
    pub fn write_text(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        self.write(bytes);
    }

    pub fn finish(self) -> u64 {
        self.0
    }
}

/// How many files and lines a task diff may still carry. Share one value between the
/// worktrees of a diff, in the order they are listed.
#[derive(Debug, Clone, Copy)]
pub struct DiffBudget {
    files: usize,
    lines: usize,
}

impl Default for DiffBudget {
    fn default() -> Self {
        Self {
            files: DIFF_FILES,
            lines: DIFF_LINES,
        }
    }
}

/// One changed file of a worktree, before it is attributed to an agent.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    /// The path as git printed it; `diff.path` is the lossy form.
    pub raw_path: Vec<u8>,
    /// Hash of the whole change, also of lines the caps left out.
    pub digest: u64,
    /// `agent` is `None`. A file past the file budget has no hunks.
    pub diff: FileDiff,
}

/// Git needs a NUL byte in the first 8 KiB to call a file binary.
const DIFF_BINARY_SNIFF: usize = 8192;

/// Marks the start of a commit in the attribution log; unlikely inside a path.
const COMMIT_MARK: &[u8] = b"\x01\x02swarm-commit\x02\x01";

/// What a diff of a worktree compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    /// The working tree against `base`: committed, uncommitted and untracked work.
    WorkingTree,
    /// `HEAD` against `base`: commits only.
    Committed,
}

/// What `worktree` changed since `base`, as reviewable files: tracked changes from
/// git's patch, and for [`DiffScope::WorkingTree`] untracked files as added files.
/// Sorted by path. Spends `budget` on the hunk lines; files and lines past it are still
/// counted and hashed. Retries once when the worktree changes while git runs.
pub fn worktree_diff(
    worktree: &Path,
    base: &str,
    scope: DiffScope,
    budget: &mut DiffBudget,
) -> Result<Vec<ChangedFile>, Error> {
    for _ in 0..2 {
        let mut attempt = *budget;

        if let Some(files) = read_worktree_diff(worktree, base, scope, &mut attempt)? {
            *budget = attempt;

            return Ok(files);
        }
    }

    Err(Error::Invalid(
        "the files changed while the diff was read; try again".to_owned(),
    ))
}

/// One path of the diff, before it is read.
enum Item {
    Tracked {
        path: Vec<u8>,
        /// The patch blocks of the path, as ranges of the patch. Two when a file
        /// became a symlink or the other way round.
        blocks: Vec<std::ops::Range<usize>>,
        /// `None` for a binary file.
        added: Option<u32>,
        deleted: u32,
    },
    Untracked {
        path: Vec<u8>,
    },
}

impl Item {
    fn path(&self) -> &[u8] {
        match self {
            Item::Tracked { path, .. } | Item::Untracked { path } => path,
        }
    }
}

/// `None` when the patch and the numstat disagree.
fn read_worktree_diff(
    worktree: &Path,
    base: &str,
    scope: DiffScope,
    budget: &mut DiffBudget,
) -> Result<Option<Vec<ChangedFile>>, Error> {
    let numstat = diff_command(worktree, base, scope, &["--numstat", "-z"])?;
    let patch = diff_command(
        worktree,
        base,
        scope,
        &["-p", "-U3", "--full-index", "--inter-hunk-context=0"],
    )?;

    let Some(mut items) = tracked_items(&numstat, &patch) else {
        return Ok(None);
    };

    if scope == DiffScope::WorkingTree {
        let untracked = untracked_paths(worktree)?;
        items.retain(|item| match item {
            Item::Tracked { path, .. } => !untracked.contains(path),
            Item::Untracked { .. } => true,
        });
        items.extend(untracked.into_iter().map(|path| Item::Untracked { path }));
    }
    items.sort_by(|one, other| one.path().cmp(other.path()));

    let mut untracked_budget = MAX_TOTAL_BYTES;
    let mut untracked_seen = 0;
    let mut files = Vec::with_capacity(items.len());

    for item in &items {
        let mut collector = Collector::new(budget);

        let file = match item {
            Item::Tracked {
                path,
                blocks,
                added,
                deleted,
            } => {
                let Some(file) = tracked_file(
                    worktree,
                    path,
                    blocks.iter().map(|block| &patch[block.clone()]),
                    (*added, *deleted),
                    &mut collector,
                ) else {
                    return Ok(None);
                };

                file
            }
            Item::Untracked { path } => {
                let readable = untracked_seen < MAX_UNTRACKED_FILES;
                untracked_seen += 1;

                let Some(file) = untracked_file(
                    worktree,
                    path,
                    readable,
                    &mut untracked_budget,
                    &mut collector,
                ) else {
                    continue;
                };

                file
            }
        };

        files.push(collector.finish(worktree, scope, file, budget));
    }

    Ok(Some(files))
}

fn diff_command(
    worktree: &Path,
    base: &str,
    scope: DiffScope,
    options: &[&str],
) -> Result<Vec<u8>, Error> {
    // `diff-index` never writes the index, so it cannot block the agent's own git.
    let command = match scope {
        DiffScope::WorkingTree => "diff-index",
        DiffScope::Committed => "diff",
    };
    let mut arguments: Vec<&str> = vec![
        command,
        "--no-renames",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ];
    arguments.extend_from_slice(options);
    arguments.extend(["--end-of-options", base]);
    if scope == DiffScope::Committed {
        arguments.push("HEAD");
    }
    arguments.push("--");

    let (arguments, output) = execute(worktree, arguments)?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    Ok(output.raw)
}

fn untracked_paths(worktree: &Path) -> Result<HashSet<Vec<u8>>, Error> {
    let (arguments, output) = execute(
        worktree,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    Ok(output
        .raw
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// Pairs the NUL-separated numstat with the patch blocks, which come in the same order.
/// Git quotes unusual paths in patch headers, so the numstat gives the exact path.
fn tracked_items(counts: &[u8], diff: &[u8]) -> Option<Vec<Item>> {
    let mut entries = Vec::new();
    for entry in counts
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let mut parts = entry.splitn(3, |byte| *byte == b'\t');
        let (Some(gained), Some(lost), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return None;
        };

        let added = (gained != b"-").then(|| parse_count(gained));
        entries.push((path.to_vec(), added, parse_count(lost)));
    }

    // (header line, range) of each block; blocks of one path share their header.
    let mut blocks: Vec<(&[u8], std::ops::Range<usize>)> = Vec::new();
    let mut offset = 0;
    for line in diff.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"diff --git ") {
            let header = line.strip_suffix(b"\n").unwrap_or(line);

            blocks.push((header, offset..offset + line.len()));
        } else if let Some((_, range)) = blocks.last_mut() {
            range.end = offset + line.len();
        }
        offset += line.len();
    }

    let mut groups: Vec<(&[u8], Vec<std::ops::Range<usize>>)> = Vec::new();
    for (header, range) in blocks {
        match groups.last_mut() {
            Some((last, ranges)) if *last == header => ranges.push(range),
            _ => groups.push((header, vec![range])),
        }
    }

    if groups.len() != entries.len() {
        return None;
    }

    let mut items = Vec::with_capacity(entries.len());
    for ((path, added, deleted), (header, ranges)) in entries.into_iter().zip(groups) {
        if !header_names(header, &path) {
            return None;
        }

        items.push(Item::Tracked {
            path,
            blocks: ranges,
            added,
            deleted,
        });
    }

    Some(items)
}

/// False when an unquoted header names another path. A quoted header cannot be checked
/// without unescaping it, and its order and count already matched.
fn header_names(header: &[u8], path: &[u8]) -> bool {
    let Some(rest) = header.strip_prefix(b"diff --git ") else {
        return false;
    };
    if rest.starts_with(b"\"") {
        return true;
    }

    let mut expected = b"a/".to_vec();
    expected.extend_from_slice(path);
    expected.extend_from_slice(b" b/");
    expected.extend_from_slice(path);

    rest == expected.as_slice()
}

/// Collects the lines of one file under the caps while hashing all of them.
struct Collector {
    digest: Fingerprint,
    hunks: Vec<DiffHunk>,
    pending: Option<DiffHunk>,
    kept: usize,
    limit: usize,
    truncated: bool,
    additions: u32,
    deletions: u32,
    last_kept: bool,
    /// The file was not read; its size and mtime stand in for its content.
    left_out: bool,
}

impl Collector {
    fn new(budget: &DiffBudget) -> Self {
        let limit = if budget.files == 0 {
            0
        } else {
            DIFF_LINES_PER_FILE.min(budget.lines)
        };

        Self {
            digest: Fingerprint::default(),
            hunks: Vec::new(),
            pending: None,
            kept: 0,
            limit,
            truncated: false,
            additions: 0,
            deletions: 0,
            last_kept: false,
            left_out: false,
        }
    }

    /// A header line of the file block (modes, blob hashes), hashed but never shown.
    fn header(&mut self, line: &[u8]) {
        self.digest.write_text(line);
    }

    /// The file is too large or over a cap, so it is shown without lines.
    fn leave_out(&mut self) {
        self.truncated = true;
        self.left_out = true;
    }

    fn start_hunk(&mut self, hunk: DiffHunk) {
        for number in [
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines,
        ] {
            self.digest.write_u64(u64::from(number));
        }
        self.digest.write_text(hunk.section.as_bytes());

        self.pending = Some(hunk);
    }

    fn push(&mut self, kind: DiffLineKind, text: &[u8]) {
        self.digest.write(&[match kind {
            DiffLineKind::Context => 0,
            DiffLineKind::Added => 1,
            DiffLineKind::Deleted => 2,
            DiffLineKind::NoNewline => 3,
        }]);
        self.digest.write_text(text);

        if kind == DiffLineKind::NoNewline {
            // The marker follows the line it belongs to, whatever the caps are.
            if self.last_kept
                && let Some(hunk) = self.hunks.last_mut()
            {
                hunk.lines.push(DiffLine {
                    kind,
                    text: String::new(),
                });
            }

            return;
        }

        match kind {
            DiffLineKind::Added => self.additions = self.additions.saturating_add(1),
            DiffLineKind::Deleted => self.deletions = self.deletions.saturating_add(1),
            DiffLineKind::Context | DiffLineKind::NoNewline => {}
        }

        if self.kept >= self.limit {
            self.truncated = true;
            self.last_kept = false;

            return;
        }
        self.kept += 1;
        self.last_kept = true;

        if let Some(hunk) = self.pending.take() {
            self.hunks.push(hunk);
        }
        let Some(hunk) = self.hunks.last_mut() else {
            return;
        };

        hunk.lines.push(DiffLine {
            kind,
            text: cut_line(text),
        });
    }

    fn finish(
        self,
        worktree: &Path,
        scope: DiffScope,
        mut file: FileDiff,
        budget: &mut DiffBudget,
    ) -> ChangedFile {
        let mut digest = self.digest;
        digest.write_u64(u64::from(file.binary));

        // A committed binary change shows in its blob hashes; the working tree has none.
        let by_stat = scope == DiffScope::WorkingTree && file.binary;
        if (by_stat || self.left_out)
            && let Ok(meta) = std::fs::symlink_metadata(worktree.join(&file.path))
        {
            digest.write_u64(meta.len());
            digest.write_u64(meta.mtime().cast_unsigned());
            digest.write_u64(meta.mtime_nsec().cast_unsigned());
        }

        budget.files = budget.files.saturating_sub(1);
        budget.lines = budget.lines.saturating_sub(self.kept);

        file.hunks = self.hunks;
        file.truncated |= self.truncated;
        if !file.binary {
            file.additions = self.additions;
            file.deletions = self.deletions;
        }

        ChangedFile {
            raw_path: file.path.as_os_str().as_bytes().to_vec(),
            digest: digest.finish(),
            diff: file,
        }
    }
}

/// Cuts at [`DIFF_LINE_BYTES`] on a character boundary. Invalid UTF-8 turns into
/// replacement characters first.
fn cut_line(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    if text.len() <= DIFF_LINE_BYTES {
        return text.into_owned();
    }

    let mut end = DIFF_LINE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    text[..end].to_owned()
}

fn blank_file(worktree: &Path, raw_path: &[u8], change: FileChange, binary: bool) -> FileDiff {
    FileDiff {
        worktree: worktree.to_owned(),
        path: PathBuf::from(String::from_utf8_lossy(raw_path).into_owned()),
        agent: None,
        change,
        mode_change: None,
        additions: 0,
        deletions: 0,
        binary,
        hunks: Vec::new(),
        truncated: false,
    }
}

/// Reads the patch blocks of one tracked path. `None` when the line counts differ from
/// the numstat, which means the worktree changed between the two git calls.
fn tracked_file<'a>(
    worktree: &Path,
    path: &[u8],
    blocks: impl Iterator<Item = &'a [u8]>,
    (added, deleted): (Option<u32>, u32),
    collector: &mut Collector,
) -> Option<FileDiff> {
    let (mut created, mut removed) = (false, false);
    let (mut old_mode, mut new_mode) = (None, None);

    for block in blocks {
        let mut in_hunks = false;

        for line in block.split_inclusive(|byte| *byte == b'\n').skip(1) {
            let line = line.strip_suffix(b"\n").unwrap_or(line);

            if line.starts_with(b"@@ ") {
                in_hunks = true;
                if let Some(hunk) = parse_hunk_header(line) {
                    collector.start_hunk(hunk);
                }

                continue;
            }

            if !in_hunks {
                created |= line.starts_with(b"new file mode ");
                removed |= line.starts_with(b"deleted file mode ");
                old_mode = old_mode.or_else(|| parse_mode(line, b"old mode "));
                new_mode = new_mode.or_else(|| parse_mode(line, b"new mode "));
                collector.header(line);

                continue;
            }

            match line.first() {
                Some(b'\\') => collector.push(DiffLineKind::NoNewline, b""),
                Some(b'+') => collector.push(DiffLineKind::Added, &line[1..]),
                Some(b'-') => collector.push(DiffLineKind::Deleted, &line[1..]),
                Some(b' ') => collector.push(DiffLineKind::Context, &line[1..]),
                None => collector.push(DiffLineKind::Context, line),
                Some(_) => {}
            }
        }
    }

    let change = match (created, removed) {
        (true, false) => FileChange::Added,
        (false, true) => FileChange::Deleted,
        _ => FileChange::Modified,
    };

    let binary = added.is_none();
    if !binary && (Some(collector.additions) != added || collector.deletions != deleted) {
        return None;
    }

    let mut file = blank_file(worktree, path, change, binary);
    file.mode_change = old_mode.zip(new_mode);

    Some(file)
}

/// The octal mode in a `old mode 100644` style header line.
fn parse_mode(line: &[u8], prefix: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(line.strip_prefix(prefix)?).ok()?;

    u32::from_str_radix(text.trim(), 8).ok()
}

/// `@@ -1,2 +3,4 @@ section`
fn parse_hunk_header(line: &[u8]) -> Option<DiffHunk> {
    let line = String::from_utf8_lossy(line);
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;

    let range = |text: &str, sign: char| -> Option<(u32, u32)> {
        let text = text.strip_prefix(sign)?;
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let ((old_start, old_lines), (new_start, new_lines)) = (range(old, '-')?, range(new, '+')?);

    Some(DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section: cut_line(section.strip_prefix(' ').unwrap_or(section).as_bytes()),
        lines: Vec::new(),
    })
}

/// An untracked path as an added file. `None` when it vanished while reading.
fn untracked_file(
    worktree: &Path,
    raw_path: &[u8],
    readable: bool,
    budget: &mut u64,
    collector: &mut Collector,
) -> Option<FileDiff> {
    let path = worktree.join(OsStr::from_bytes(raw_path));
    let meta = std::fs::symlink_metadata(&path).ok()?;

    let text_file = |binary| blank_file(worktree, raw_path, FileChange::Added, binary);

    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&path).ok()?;

        collector.start_hunk(added_hunk(1));
        collector.push(DiffLineKind::Added, target.as_os_str().as_bytes());

        return Some(text_file(false));
    }

    // A FIFO or a folder (a nested repository) is never opened.
    if !meta.is_file() {
        return Some(text_file(true));
    }

    if !readable || meta.len() > MAX_COUNTED_BYTES || *budget == 0 {
        collector.leave_out();

        return Some(text_file(false));
    }

    let mut bytes = Vec::new();
    let read = std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_COUNTED_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() {
        collector.leave_out();

        return Some(text_file(false));
    }

    *budget = budget.saturating_sub(bytes.len() as u64);
    if bytes[..bytes.len().min(DIFF_BINARY_SNIFF)].contains(&0) {
        return Some(text_file(true));
    }
    if bytes.len() as u64 > MAX_COUNTED_BYTES {
        collector.leave_out();

        return Some(text_file(false));
    }

    let missing_newline = bytes.last().is_some_and(|last| *last != b'\n');
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    if !lines.is_empty() {
        collector.start_hunk(added_hunk(u32::try_from(lines.len()).unwrap_or(u32::MAX)));
    }
    for line in lines {
        collector.push(DiffLineKind::Added, line);
    }
    if missing_newline {
        collector.push(DiffLineKind::NoNewline, b"");
    }

    Some(text_file(false))
}

fn added_hunk(lines: u32) -> DiffHunk {
    DiffHunk {
        old_start: 0,
        old_lines: 0,
        new_start: 1,
        new_lines: lines,
        section: String::new(),
        lines: Vec::new(),
    }
}

/// The agent behind the newest commit in `base..HEAD` that carries a `Swarm-Agent`
/// trailer and touched the path.
pub fn attribution(worktree: &Path, base: &str) -> Result<HashMap<Vec<u8>, String>, Error> {
    let format = "--format=%x01%x02swarm-commit%x02%x01%(trailers:key=Swarm-Agent,valueonly,unfold,separator=%x03)";
    let range = format!("{base}..HEAD");

    let (arguments, output) = execute(
        worktree,
        [
            "log",
            "-z",
            "--name-only",
            "--no-renames",
            format,
            "--end-of-options",
            &range,
        ],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    let mut agents = HashMap::new();
    let mut current: Option<String> = None;

    for token in output.raw.split(|byte| *byte == 0) {
        if let Some(trailer) = token.strip_prefix(COMMIT_MARK) {
            let trailer = String::from_utf8_lossy(trailer);
            let name = trailer.split('\u{3}').next().unwrap_or("").trim();

            current = (!name.is_empty()).then(|| name.to_owned());

            continue;
        }

        let path = token.strip_prefix(b"\n").unwrap_or(token);
        if let (Some(agent), false) = (&current, path.is_empty()) {
            // The log lists the newest commit first.
            agents.entry(path.to_vec()).or_insert_with(|| agent.clone());
        }
    }

    Ok(agents)
}

/// True when `ancestor` is `descendant` or reachable from it.
pub fn is_ancestor(worktree: &Path, ancestor: &str, descendant: &str) -> Result<bool, Error> {
    let (arguments, output) = execute(
        worktree,
        [
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ],
    )?;

    match (output.success, output.code) {
        (true, _) => Ok(true),
        (false, Some(1)) => Ok(false),
        (false, _) => Err(failure(&arguments, &output)),
    }
}

/// Files with changes that are not committed, untracked ones included. Ignored files
/// are not counted: closing a task deletes them without asking.
pub fn uncommitted_files(worktree: &Path) -> Result<u32, Error> {
    let count = uncommitted_paths(worktree)?.len();

    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The paths behind [`uncommitted_files`], as git printed them.
pub fn uncommitted_paths(worktree: &Path) -> Result<HashSet<Vec<u8>>, Error> {
    let (arguments, output) = execute(
        worktree,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
        ],
    )?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    // Each entry is `XY path`.
    Ok(output
        .raw
        .split(|byte| *byte == 0)
        .filter(|entry| entry.len() > 3)
        .map(|entry| entry[3..].to_vec())
        .collect())
}

/// Forgets worktrees whose folder is gone.
pub fn worktree_prune(root: &Path) -> Result<(), Error> {
    run(root, ["worktree", "prune"])?;

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
