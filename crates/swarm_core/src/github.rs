//! The GitHub CLI: finding it, running it and reading what it prints (PLAN.md, Pull
//! requests). User text only ever travels as an argument or on stdin, never through a
//! shell.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use swarm_protocol::{
    GithubCheck, GithubCheckState, GithubStatus, MergeMethod, PrLink, PrState, PullRequest,
    ReviewState, Reviewer,
};

use crate::store::unix_millis;
use crate::{Error, adapters, git, process};

/// How long one `gh` call may run.
pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// Lines of `gh` output an error carries.
const ERROR_LINES: usize = 40;

/// GitHub's longest pull request title, in characters.
pub const TITLE_CHARS: usize = 256;

/// GitHub's longest pull request body, in characters.
pub const BODY_CHARS: usize = 65_536;

const HOST: &str = "github.com";

/// The JSON fields `PrStatus` reads.
const VIEW_FIELDS: &str = "number,url,title,state,isDraft,mergeable,mergeStateStatus,statusCheckRollup,reviewRequests,latestReviews";

/// Downloads the latest official `gh` for macOS and installs its binary in the folder
/// given as `$1`. It checks the download against the checksums of the release and stops
/// at the first problem. Everything it writes stays inside that folder.
const INSTALL_SCRIPT: &str = r#"set -eu
bin=$1
case $bin in
  /*) ;;
  *) echo "The install folder must be an absolute path: $bin" >&2; exit 1 ;;
esac
case $(uname -m) in
  arm64) arch=arm64 ;;
  x86_64) arch=amd64 ;;
  *) echo "GitHub CLI has no macOS release for $(uname -m)" >&2; exit 1 ;;
esac
fetch() { curl --proto '=https' --proto-redir '=https' -fsSL "$@"; }
mkdir -p "$bin"
work=$(mktemp -d "$bin/.install.XXXXXX")
trap 'rm -rf "$work"' EXIT
cd "$work"
echo "Finding the latest GitHub CLI release..."
latest=$(fetch -o /dev/null -w '%{url_effective}' https://github.com/cli/cli/releases/latest)
tag=${latest##*/}
version=${tag#v}
case $version in
  ''|*[!0-9.]*) echo "Unexpected release tag: $tag" >&2; exit 1 ;;
esac
asset=gh_${version}_macOS_${arch}.zip
base=https://github.com/cli/cli/releases/download/$tag
echo "Downloading $asset..."
fetch -o "$asset" "$base/$asset"
fetch -o checksums.txt "$base/gh_${version}_checksums.txt"
expected=$(awk -v name="$asset" '$2 == name { print $1 }' checksums.txt)
if [ -z "$expected" ]; then
  echo "The release has no checksum for $asset" >&2
  exit 1
fi
actual=$(shasum -a 256 "$asset" | awk '{ print $1 }')
if [ "$actual" != "$expected" ]; then
  echo "Checksum mismatch for $asset" >&2
  exit 1
fi
unzip -q "$asset" -d unpacked
binary=$(find unpacked -type f -path '*/bin/gh' | head -n 1)
if [ -z "$binary" ]; then
  echo "$asset has no gh binary" >&2
  exit 1
fi
chmod 755 "$binary"
mv -f "$binary" "$bin/gh"
echo "Installed gh $version in $bin"
"#;

/// Where `gh` lives and where Swarm installs its own tools.
#[derive(Debug, Clone)]
pub struct Github {
    /// `<data>/bin`: the last place `gh` is looked for, and where the installer puts it.
    bin_dir: PathBuf,
    /// A `gh` to use as it is, without any search.
    injected: Option<PathBuf>,
    /// Searched in order for `gh` and `brew`.
    folders: Vec<PathBuf>,
}

/// A pull request that is to be made.
#[derive(Debug, Clone, Copy)]
pub struct NewPullRequest<'a> {
    pub base: &'a str,
    pub head: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub draft: bool,
}

/// The task worktree that `gh` runs in and the repository of `origin`.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub worktree: &'a Path,
    /// `owner/name`. Set as `GH_REPO`, so a fork with more remotes is never ambiguous.
    pub repository: &'a str,
}

/// What a successful `gh` call printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhOutput {
    pub stdout: String,
    pub stderr: String,
}

impl Github {
    /// Looks for tools in `PATH`, the usual install folders and then `bin_dir`. `gh`
    /// skips the search when given (tests and `--gh`).
    pub fn new(bin_dir: PathBuf, gh: Option<PathBuf>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let folders =
            adapters::search_folders(std::env::var_os("PATH").as_deref(), home.as_deref());

        Self::with_folders(bin_dir, gh, folders)
    }

    /// Like [`Github::new`], with the folders to search given.
    pub fn with_folders(bin_dir: PathBuf, gh: Option<PathBuf>, folders: Vec<PathBuf>) -> Self {
        Self {
            bin_dir,
            injected: gh,
            folders,
        }
    }

    /// Swarm's own folder for tools it installs.
    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    /// The `gh` to run, if there is one.
    pub fn find(&self) -> Option<PathBuf> {
        if let Some(gh) = &self.injected {
            return adapters::is_executable(gh).then(|| gh.clone());
        }

        self.find_program("gh")
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.folders
            .iter()
            .chain([&self.bin_dir])
            .map(|folder| folder.join(name))
            .find(|candidate| adapters::is_executable(candidate))
    }

    /// Homebrew, when it is installed.
    fn brew(&self) -> Option<PathBuf> {
        self.folders
            .iter()
            .map(|folder| folder.join("brew"))
            .find(|candidate| adapters::is_executable(candidate))
    }

    /// The command that installs `gh`: Homebrew when it exists, else the script that
    /// downloads the official release into Swarm's bin folder.
    pub fn install_command(&self) -> Vec<String> {
        if let Some(brew) = self.brew() {
            return vec![text(&brew), "install".to_owned(), "gh".to_owned()];
        }

        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            INSTALL_SCRIPT.to_owned(),
            "swarm-install-gh".to_owned(),
            text(&self.bin_dir),
        ]
    }

    /// Where `gh` is after [`Github::install_command`] ran.
    fn installed_path(&self) -> PathBuf {
        match self.brew() {
            Some(brew) => brew.with_file_name("gh"),
            None => self.bin_dir.join("gh"),
        }
    }

    /// The command that signs `gh` in to github.com in the browser. It uses the `gh`
    /// that will exist once the install ran, when there is none yet.
    pub fn sign_in_command(&self) -> Vec<String> {
        let gh = self.find().unwrap_or_else(|| self.installed_path());

        vec![
            text(&gh),
            "auth".to_owned(),
            "login".to_owned(),
            "--web".to_owned(),
            "--hostname".to_owned(),
            HOST.to_owned(),
        ]
    }

    /// `gh`, its sign-in and `origin` of the repository at `root`.
    pub fn status(&self, root: &Path) -> GithubStatus {
        let gh = self.find();
        let gh_version = gh.is_some().then(|| self.version(root)).flatten();
        let signed_in = gh.is_some()
            && self
                .exec(root, None, &["auth", "status", "--hostname", HOST], None)
                .is_ok();
        let repository = git::origin_url(root).and_then(|url| parse_repository(&url));

        GithubStatus {
            gh,
            gh_version,
            signed_in,
            repository,
            install: self.install_command(),
            sign_in: self.sign_in_command(),
        }
    }

    fn version(&self, root: &Path) -> Option<String> {
        let output = self.exec(root, None, &["--version"], None).ok()?;

        parse_version(&output.stdout)
    }

    /// The path of a `gh` that is installed and signed in to github.com.
    pub fn require_ready(&self, root: &Path) -> Result<PathBuf, Error> {
        let gh = self.find().ok_or_else(missing)?;

        if self
            .exec(root, None, &["auth", "status", "--hostname", HOST], None)
            .is_err()
        {
            return Err(Error::Invalid(
                "the GitHub CLI is not signed in to github.com".to_owned(),
            ));
        }

        Ok(gh)
    }

    /// Opens the pull request and returns its number and URL.
    pub fn create_pull_request(
        &self,
        target: &Target<'_>,
        new: &NewPullRequest<'_>,
    ) -> Result<PrLink, Error> {
        let mut arguments = vec![
            "pr",
            "create",
            "--base",
            new.base,
            "--head",
            new.head,
            "--title",
            new.title,
            "--body-file",
            "-",
        ];
        if new.draft {
            arguments.push("--draft");
        }

        let output = self.run(target, &arguments, Some(new.body))?;

        parse_link(&output.stdout).ok_or_else(|| {
            Error::Invalid(format!(
                "gh made the pull request but printed no link: {}",
                process::last_lines(output.stdout.trim(), 3)
            ))
        })
    }

    /// The pull request as GitHub reports it now.
    pub fn pull_request(&self, target: &Target<'_>, number: u64) -> Result<PullRequest, Error> {
        let number = number.to_string();
        let output = self.run(
            target,
            &["pr", "view", &number, "--json", VIEW_FIELDS],
            None,
        )?;

        parse_pull_request(&output.stdout, unix_millis())
    }

    pub fn mark_ready(&self, target: &Target<'_>, number: u64) -> Result<(), Error> {
        self.run(target, &["pr", "ready", &number.to_string()], None)?;

        Ok(())
    }

    /// Asks the logins for a review. They must be valid GitHub logins.
    pub fn add_reviewers(
        &self,
        target: &Target<'_>,
        number: u64,
        logins: &[String],
    ) -> Result<(), Error> {
        let logins = check_logins(logins)?;
        let joined = logins.join(",");

        self.run(
            target,
            &["pr", "edit", &number.to_string(), "--add-reviewer", &joined],
            None,
        )?;

        Ok(())
    }

    /// Merges the pull request and deletes its branches. `gh` merges first and deletes
    /// after, and it cannot remove the local branch while a worktree has it checked out.
    /// So when `gh` fails but GitHub shows the pull request merged, the merge counts as
    /// done and the remote branch is removed here.
    pub fn merge(
        &self,
        target: &Target<'_>,
        number: u64,
        method: MergeMethod,
        head: &str,
    ) -> Result<(), Error> {
        let number_text = number.to_string();
        let flag = match method {
            MergeMethod::Squash => "--squash",
            MergeMethod::Merge => "--merge",
            MergeMethod::Rebase => "--rebase",
        };

        let Err(error) = self.run(
            target,
            &["pr", "merge", &number_text, flag, "--delete-branch"],
            None,
        ) else {
            return Ok(());
        };

        match self.pull_request(target, number) {
            Ok(pull_request) if pull_request.state == PrState::Merged => {}
            _ => return Err(error),
        }

        let endpoint = format!("repos/{}/git/refs/heads/{head}", target.repository);
        if let Err(delete_error) = self.run(target, &["api", "--method", "DELETE", &endpoint], None)
        {
            log::debug!("the remote branch {head} is not deleted: {delete_error}");
        }

        Ok(())
    }

    fn run(
        &self,
        target: &Target<'_>,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<GhOutput, Error> {
        self.exec(target.worktree, Some(target.repository), arguments, input)
    }

    /// Runs `gh` in `folder` with prompts off and no terminal.
    fn exec(
        &self,
        folder: &Path,
        repository: Option<&str>,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<GhOutput, Error> {
        let gh = self.find().ok_or_else(missing)?;

        let mut command = Command::new(&gh);
        command
            .args(arguments)
            .current_dir(folder)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(repository) = repository {
            command.env("GH_REPO", repository);
        }

        let captured = process::capture(command, input.map(str::as_bytes), GH_TIMEOUT, false)
            .map_err(|source| Error::Io {
                action: "run gh".to_owned(),
                source,
            })?;

        let output = GhOutput {
            stdout: String::from_utf8_lossy(&captured.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&captured.stderr).into_owned(),
        };
        if captured.success() {
            return Ok(output);
        }

        let command = arguments
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let printed = [output.stderr.trim(), output.stdout.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let printed = if captured.status.is_none() {
            format!(
                "took longer than {} seconds\n{printed}",
                GH_TIMEOUT.as_secs()
            )
        } else {
            printed
        };

        Err(Error::Gh {
            command,
            output: process::last_lines(printed.trim(), ERROR_LINES),
        })
    }
}

fn missing() -> Error {
    Error::Invalid("the GitHub CLI (gh) is not installed".to_owned())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The version number from the first line of `gh --version`.
fn parse_version(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }

    let version = line
        .strip_prefix("gh version ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(line);

    Some(version.to_owned())
}

/// `owner/name` of a GitHub remote URL, in the https, ssh and scp forms, with or without
/// `.git`. `None` for another host.
pub fn parse_repository(url: &str) -> Option<String> {
    let url = url.trim();

    let (authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            if !matches!(
                scheme.as_str(),
                "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
            ) {
                return None;
            }

            rest.split_once('/')?
        }
        None => url.split_once(':')?,
    };

    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "github.com" | "www.github.com" | "ssh.github.com"
    ) {
        return None;
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;

    let valid = |part: &str| {
        !part.is_empty()
            && part.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
    };

    (valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

/// The pull request link `gh pr create` prints as its last line.
fn parse_link(stdout: &str) -> Option<PrLink> {
    let url = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("https://") && line.contains("/pull/"))?;
    let number = url.rsplit('/').next()?.parse().ok()?;

    Some(PrLink {
        number,
        url: url.to_owned(),
    })
}

/// Logins that GitHub accepts: letters, digits and hyphens, at most 39, not starting with
/// a hyphen. One leading `@` is allowed and dropped.
fn check_logins(logins: &[String]) -> Result<Vec<String>, Error> {
    if logins.is_empty() {
        return Err(Error::Invalid("name at least one reviewer".to_owned()));
    }

    logins
        .iter()
        .map(|login| {
            let login = login.trim();
            let login = login.strip_prefix('@').unwrap_or(login);

            let valid = !login.is_empty()
                && login.len() <= 39
                && !login.starts_with('-')
                && login
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-');
            if valid {
                Ok(login.to_owned())
            } else {
                Err(Error::Invalid(format!("{login:?} is not a GitHub login")))
            }
        })
        .collect()
}

/// A title GitHub accepts: one line, 1 to [`TITLE_CHARS`] characters.
pub fn check_title(title: &str) -> Result<String, Error> {
    let title = title.trim();

    if title.is_empty() {
        return Err(Error::Invalid("the title is empty".to_owned()));
    }
    if title.contains(['\n', '\r']) {
        return Err(Error::Invalid("the title must be one line".to_owned()));
    }
    if title.chars().count() > TITLE_CHARS {
        return Err(Error::Invalid(format!(
            "the title is longer than {TITLE_CHARS} characters"
        )));
    }

    Ok(title.to_owned())
}

/// A body GitHub accepts: at most [`BODY_CHARS`] characters.
pub fn check_body(body: &str) -> Result<(), Error> {
    if body.chars().count() > BODY_CHARS {
        return Err(Error::Invalid(format!(
            "the description is longer than {BODY_CHARS} characters"
        )));
    }

    Ok(())
}

/// A JSON string that may be `null`.
fn text_or_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// A JSON list that may be `null`.
fn list_or_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct ViewJson {
    number: u64,
    #[serde(deserialize_with = "text_or_empty")]
    url: String,
    #[serde(deserialize_with = "text_or_empty")]
    title: String,
    #[serde(deserialize_with = "text_or_empty")]
    state: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(default, deserialize_with = "text_or_empty")]
    mergeable: String,
    #[serde(
        rename = "mergeStateStatus",
        default,
        deserialize_with = "text_or_empty"
    )]
    merge_state_status: String,
    #[serde(
        rename = "statusCheckRollup",
        default,
        deserialize_with = "list_or_empty"
    )]
    rollup: Vec<RollupJson>,
    #[serde(rename = "reviewRequests", default, deserialize_with = "list_or_empty")]
    requests: Vec<RequestJson>,
    #[serde(rename = "latestReviews", default, deserialize_with = "list_or_empty")]
    reviews: Vec<ReviewJson>,
}

/// A check run or a commit status.
#[derive(Deserialize)]
struct RollupJson {
    #[serde(rename = "__typename", default, deserialize_with = "text_or_empty")]
    kind: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    name: String,
    #[serde(rename = "workflowName", default, deserialize_with = "text_or_empty")]
    workflow: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    status: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    conclusion: String,
    #[serde(rename = "startedAt", default, deserialize_with = "text_or_empty")]
    started_at: String,
    #[serde(rename = "completedAt", default, deserialize_with = "text_or_empty")]
    completed_at: String,
    #[serde(rename = "detailsUrl", default, deserialize_with = "text_or_empty")]
    details_url: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    context: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    state: String,
    #[serde(rename = "targetUrl", default, deserialize_with = "text_or_empty")]
    target_url: String,
}

/// A user, a bot or a team; only the first two have a login.
#[derive(Deserialize)]
struct RequestJson {
    #[serde(default, deserialize_with = "text_or_empty")]
    login: String,
}

#[derive(Deserialize)]
struct ReviewJson {
    #[serde(default)]
    author: Option<RequestJson>,
    #[serde(default, deserialize_with = "text_or_empty")]
    state: String,
}

/// Reads the JSON of `gh pr view --json` ([`VIEW_FIELDS`]).
pub fn parse_pull_request(json: &str, read_at: u64) -> Result<PullRequest, Error> {
    let view: ViewJson = serde_json::from_str(json)
        .map_err(|error| Error::Invalid(format!("gh printed JSON that cannot be read: {error}")))?;

    let state = match view.state.as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    let mergeable = state == PrState::Open
        && !view.is_draft
        && view.mergeable == "MERGEABLE"
        && matches!(
            view.merge_state_status.as_str(),
            "CLEAN" | "UNSTABLE" | "HAS_HOOKS"
        );

    Ok(PullRequest {
        number: view.number,
        url: view.url,
        title: view.title,
        state,
        draft: view.is_draft,
        checks: view.rollup.into_iter().map(github_check).collect(),
        reviewers: reviewers(view.requests, view.reviews),
        mergeable,
        read_at,
    })
}

fn github_check(entry: RollupJson) -> GithubCheck {
    let is_status =
        entry.kind == "StatusContext" || (entry.kind.is_empty() && !entry.context.is_empty());

    if is_status {
        let state = match entry.state.as_str() {
            "SUCCESS" => GithubCheckState::Passed,
            "PENDING" | "EXPECTED" => GithubCheckState::Running,
            _ => GithubCheckState::Failed,
        };

        return GithubCheck {
            name: entry.context,
            state,
            duration_secs: None,
            url: non_empty(entry.target_url),
        };
    }

    let state = match entry.status.as_str() {
        "COMPLETED" => match entry.conclusion.as_str() {
            "SUCCESS" => GithubCheckState::Passed,
            "SKIPPED" | "NEUTRAL" => GithubCheckState::Skipped,
            "CANCELLED" => GithubCheckState::Cancelled,
            _ => GithubCheckState::Failed,
        },
        "IN_PROGRESS" => GithubCheckState::Running,
        _ => GithubCheckState::Queued,
    };
    let name = if entry.workflow.is_empty() {
        entry.name
    } else {
        format!("{} / {}", entry.workflow, entry.name)
    };

    GithubCheck {
        name,
        state,
        duration_secs: duration_secs(&entry.started_at, &entry.completed_at),
        url: non_empty(entry.details_url),
    }
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// Seconds from start to end. A check that is not done has the year 1 as its end.
fn duration_secs(started: &str, completed: &str) -> Option<u32> {
    let (started, completed) = (unix_seconds(started)?, unix_seconds(completed)?);

    u32::try_from(completed.checked_sub(started)?).ok()
}

/// `2024-05-01T10:00:00Z` as seconds since the Unix epoch.
fn unix_seconds(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if bytes.len() < 19
        || separators
            .iter()
            .any(|(index, expected)| bytes[*index] != *expected)
    {
        return None;
    }

    let number = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days since 1970-01-01 in the proleptic Gregorian calendar.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// The people asked for a review, then the ones whose latest review stands. Someone who
/// is asked again shows as requested.
fn reviewers(requests: Vec<RequestJson>, reviews: Vec<ReviewJson>) -> Vec<Reviewer> {
    let mut reviewers: Vec<Reviewer> = Vec::new();
    let known = |reviewers: &[Reviewer], login: &str| {
        reviewers
            .iter()
            .any(|reviewer| reviewer.login.eq_ignore_ascii_case(login))
    };

    for request in requests {
        if !request.login.is_empty() && !known(&reviewers, &request.login) {
            reviewers.push(Reviewer {
                login: request.login,
                state: ReviewState::Requested,
            });
        }
    }

    for review in reviews {
        let Some(login) = review.author.map(|author| author.login) else {
            continue;
        };
        let state = match review.state.as_str() {
            "APPROVED" => ReviewState::Approved,
            "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
            "COMMENTED" => ReviewState::Commented,
            _ => continue,
        };

        if !login.is_empty() && !known(&reviewers, &login) {
            reviewers.push(Reviewer { login, state });
        }
    }

    reviewers
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn https_remotes_give_the_repository() {
        for url in [
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets/",
            "https://user@github.com/acme/widgets.git",
            "https://user:token@github.com/acme/widgets.git",
            "http://github.com/acme/widgets",
            "HTTPS://GitHub.com/acme/widgets",
        ] {
            assert_eq!(
                parse_repository(url).as_deref(),
                Some("acme/widgets"),
                "{url}"
            );
        }
    }

    #[test]
    fn ssh_remotes_give_the_repository() {
        for url in [
            "git@github.com:acme/widgets.git",
            "git@github.com:acme/widgets",
            "github.com:acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
            "ssh://git@github.com:22/acme/widgets",
            "ssh://git@ssh.github.com:443/acme/widgets.git",
            "git://github.com/acme/widgets.git",
        ] {
            assert_eq!(
                parse_repository(url).as_deref(),
                Some("acme/widgets"),
                "{url}"
            );
        }
    }

    #[test]
    fn a_name_with_dots_and_dashes_is_kept() {
        assert_eq!(
            parse_repository("git@github.com:my-org/my.repo-name.git").as_deref(),
            Some("my-org/my.repo-name")
        );
    }

    #[test]
    fn other_hosts_and_odd_urls_give_nothing() {
        for url in [
            "https://gitlab.com/acme/widgets.git",
            "git@gitlab.com:acme/widgets.git",
            "https://github.com.evil.example/acme/widgets",
            "https://example.com/github.com/acme/widgets",
            "https://github.com/acme",
            "https://github.com/acme/widgets/extra",
            "https://github.com/",
            "/local/path/to/origin.git",
            "file:///tmp/origin.git",
            "../origin.git",
            "",
        ] {
            assert_eq!(parse_repository(url), None, "{url}");
        }
    }

    #[test]
    fn the_version_is_the_number_of_the_first_line() {
        let output =
            "gh version 2.62.0 (2024-11-14)\nhttps://github.com/cli/cli/releases/tag/v2.62.0\n";

        assert_eq!(parse_version(output).as_deref(), Some("2.62.0"));
        assert_eq!(
            parse_version("something else\n").as_deref(),
            Some("something else")
        );
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_link_is_the_last_url_with_a_pull_number() {
        let stdout = "Creating pull request for swarm/x into main\n\nhttps://github.com/acme/widgets/pull/42\n";

        assert_eq!(
            parse_link(stdout),
            Some(PrLink {
                number: 42,
                url: "https://github.com/acme/widgets/pull/42".to_owned()
            })
        );
        assert_eq!(parse_link("no link here"), None);
        assert_eq!(parse_link("https://github.com/acme/widgets/pull/x"), None);
    }

    #[test]
    fn logins_must_look_like_github_logins() {
        let ok = check_logins(&["alice".to_owned(), "@bob-2".to_owned()]).expect("logins");
        assert_eq!(ok, ["alice", "bob-2"]);

        for bad in [
            "",
            "-flag",
            "--add-reviewer",
            "a b",
            "a,b",
            "a/b",
            "ali;ce",
            "x".repeat(40).as_str(),
        ] {
            assert!(check_logins(&[bad.to_owned()]).is_err(), "{bad:?}");
        }
        assert!(check_logins(&[]).is_err());
    }

    #[test]
    fn a_title_is_one_line_of_at_most_256_characters() {
        assert_eq!(
            check_title("  Add login  ").as_deref().ok(),
            Some("Add login")
        );
        assert!(check_title("   ").is_err());
        assert!(check_title("two\nlines").is_err());
        assert!(check_title(&"x".repeat(257)).is_err());
        assert!(check_title(&"x".repeat(256)).is_ok());
        assert!(check_body(&"x".repeat(BODY_CHARS)).is_ok());
        assert!(check_body(&"x".repeat(BODY_CHARS + 1)).is_err());
    }

    #[test]
    fn timestamps_become_seconds() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2024-05-01T10:00:00Z"), Some(1_714_557_600));
        assert_eq!(unix_seconds("2000-02-29T23:59:59Z"), Some(951_868_799));
        assert_eq!(unix_seconds("soon"), None);
        assert_eq!(unix_seconds("2024-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_check_that_is_not_done_has_no_duration() {
        assert_eq!(
            duration_secs("2024-05-01T10:00:00Z", "2024-05-01T10:02:05Z"),
            Some(125)
        );
        assert_eq!(
            duration_secs("2024-05-01T10:00:00Z", "0001-01-01T00:00:00Z"),
            None
        );
        assert_eq!(duration_secs("2024-05-01T10:00:00Z", ""), None);
    }

    #[test]
    fn the_install_command_uses_homebrew_when_it_exists() {
        let folder = std::env::temp_dir().join(format!("swarm-gh-brew-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");
        let brew = folder.join("brew");
        std::fs::write(&brew, "#!/bin/sh\n").expect("brew");
        std::fs::set_permissions(&brew, Permissions::from_mode(0o755)).expect("mode");

        let github = Github::with_folders(PathBuf::from("/data/bin"), None, vec![folder.clone()]);
        let install = github.install_command();
        let sign_in = github.sign_in_command();
        std::fs::remove_dir_all(&folder).expect("cleanup");

        assert_eq!(
            install,
            [text(&brew), "install".to_owned(), "gh".to_owned()]
        );
        assert_eq!(sign_in[0], text(&folder.join("gh")));
    }

    #[test]
    fn without_homebrew_the_install_is_the_checked_download_into_the_bin_folder() {
        let github = Github::with_folders(PathBuf::from("/data/bin"), None, Vec::new());

        let install = github.install_command();

        assert_eq!(install[..2], ["/bin/sh", "-c"]);
        assert_eq!(install[3], "swarm-install-gh");
        assert_eq!(install[4], "/data/bin");
        assert_eq!(install.len(), 5);
        assert!(install[2].contains("shasum -a 256"));
        assert!(install[2].contains("Checksum mismatch"));
        assert!(install[2].contains("https://github.com/cli/cli/releases/latest"));
        assert!(!install[2].contains("sudo"));
        assert_eq!(
            github.sign_in_command(),
            [
                "/data/bin/gh",
                "auth",
                "login",
                "--web",
                "--hostname",
                "github.com"
            ]
        );
    }

    const VIEW: &str = r#"{
        "number": 42,
        "url": "https://github.com/acme/widgets/pull/42",
        "title": "Add login",
        "state": "OPEN",
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [
            {"__typename":"CheckRun","name":"build","workflowName":"CI","status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2024-05-01T10:00:00Z","completedAt":"2024-05-01T10:02:05Z","detailsUrl":"https://github.com/acme/widgets/actions/runs/1"},
            {"__typename":"CheckRun","name":"lint","workflowName":"","status":"IN_PROGRESS","conclusion":"","startedAt":"2024-05-01T10:00:00Z","completedAt":"0001-01-01T00:00:00Z","detailsUrl":""},
            {"__typename":"CheckRun","name":"slow","workflowName":"CI","status":"QUEUED","conclusion":null,"startedAt":"2024-05-01T10:00:00Z","completedAt":null,"detailsUrl":null},
            {"__typename":"CheckRun","name":"docs","workflowName":"CI","status":"COMPLETED","conclusion":"SKIPPED","startedAt":"2024-05-01T10:00:00Z","completedAt":"2024-05-01T10:00:00Z","detailsUrl":"u"},
            {"__typename":"CheckRun","name":"e2e","workflowName":"CI","status":"COMPLETED","conclusion":"CANCELLED","startedAt":"2024-05-01T10:00:00Z","completedAt":"2024-05-01T10:00:09Z","detailsUrl":"u"},
            {"__typename":"CheckRun","name":"unit","workflowName":"CI","status":"COMPLETED","conclusion":"FAILURE","startedAt":"2024-05-01T10:00:00Z","completedAt":"2024-05-01T10:01:00Z","detailsUrl":"u"},
            {"__typename":"StatusContext","context":"ci/circleci","state":"SUCCESS","targetUrl":"https://circleci.example/1","startedAt":"2024-05-01T10:00:00Z"},
            {"__typename":"StatusContext","context":"deploy","state":"PENDING","targetUrl":"","startedAt":"2024-05-01T10:00:00Z"},
            {"__typename":"StatusContext","context":"legacy","state":"ERROR","targetUrl":"","startedAt":"2024-05-01T10:00:00Z"}
        ],
        "reviewRequests": [
            {"__typename":"User","login":"alice"},
            {"__typename":"Team","name":"Core","slug":"core"},
            {"__typename":"User","login":"Dave"}
        ],
        "latestReviews": [
            {"author":{"login":"bob"},"state":"APPROVED"},
            {"author":{"login":"carol"},"state":"CHANGES_REQUESTED"},
            {"author":{"login":"erin"},"state":"COMMENTED"},
            {"author":{"login":"frank"},"state":"DISMISSED"},
            {"author":{"login":"dave"},"state":"APPROVED"}
        ]
    }"#;

    #[test]
    fn check_runs_and_statuses_map_to_github_check_states() {
        let pull_request = parse_pull_request(VIEW, 7).expect("parse");

        let checks: Vec<(&str, GithubCheckState, Option<u32>, Option<&str>)> = pull_request
            .checks
            .iter()
            .map(|check| {
                (
                    check.name.as_str(),
                    check.state,
                    check.duration_secs,
                    check.url.as_deref(),
                )
            })
            .collect();

        assert_eq!(
            checks,
            [
                (
                    "CI / build",
                    GithubCheckState::Passed,
                    Some(125),
                    Some("https://github.com/acme/widgets/actions/runs/1")
                ),
                ("lint", GithubCheckState::Running, None, None),
                ("CI / slow", GithubCheckState::Queued, None, None),
                ("CI / docs", GithubCheckState::Skipped, Some(0), Some("u")),
                ("CI / e2e", GithubCheckState::Cancelled, Some(9), Some("u")),
                ("CI / unit", GithubCheckState::Failed, Some(60), Some("u")),
                (
                    "ci/circleci",
                    GithubCheckState::Passed,
                    None,
                    Some("https://circleci.example/1")
                ),
                ("deploy", GithubCheckState::Running, None, None),
                ("legacy", GithubCheckState::Failed, None, None),
            ]
        );
    }

    #[test]
    fn reviewers_come_from_requests_and_latest_reviews() {
        let pull_request = parse_pull_request(VIEW, 7).expect("parse");

        let reviewers: Vec<(&str, ReviewState)> = pull_request
            .reviewers
            .iter()
            .map(|reviewer| (reviewer.login.as_str(), reviewer.state))
            .collect();

        assert_eq!(
            reviewers,
            [
                ("alice", ReviewState::Requested),
                ("Dave", ReviewState::Requested),
                ("bob", ReviewState::Approved),
                ("carol", ReviewState::ChangesRequested),
                ("erin", ReviewState::Commented),
            ]
        );
    }

    #[test]
    fn a_clean_open_pull_request_is_mergeable() {
        let pull_request = parse_pull_request(VIEW, 7).expect("parse");

        assert_eq!(pull_request.number, 42);
        assert_eq!(pull_request.title, "Add login");
        assert_eq!(pull_request.state, PrState::Open);
        assert!(pull_request.mergeable);
        assert!(!pull_request.draft);
        assert_eq!(pull_request.read_at, 7);
    }

    fn view_with(state: &str, draft: bool, mergeable: &str, merge_state: &str) -> String {
        format!(
            r#"{{"number":1,"url":"u","title":"t","state":"{state}","isDraft":{draft},
                "mergeable":"{mergeable}","mergeStateStatus":"{merge_state}",
                "statusCheckRollup":null,"reviewRequests":[],"latestReviews":[]}}"#
        )
    }

    #[test]
    fn mergeable_needs_mergeable_a_good_merge_state_and_no_draft() {
        let mergeable = |state, draft, mergeable, merge_state| {
            parse_pull_request(&view_with(state, draft, mergeable, merge_state), 0)
                .expect("parse")
                .mergeable
        };

        assert!(mergeable("OPEN", false, "MERGEABLE", "CLEAN"));
        assert!(mergeable("OPEN", false, "MERGEABLE", "UNSTABLE"));
        assert!(mergeable("OPEN", false, "MERGEABLE", "HAS_HOOKS"));
        assert!(!mergeable("OPEN", false, "MERGEABLE", "BLOCKED"));
        assert!(!mergeable("OPEN", false, "MERGEABLE", "BEHIND"));
        assert!(!mergeable("OPEN", false, "MERGEABLE", "DIRTY"));
        assert!(!mergeable("OPEN", false, "CONFLICTING", "CLEAN"));
        assert!(!mergeable("OPEN", false, "UNKNOWN", "CLEAN"));
        assert!(!mergeable("OPEN", true, "MERGEABLE", "CLEAN"));
        assert!(!mergeable("MERGED", false, "MERGEABLE", "CLEAN"));
    }

    #[test]
    fn merged_and_closed_pull_requests_keep_their_state() {
        let merged = parse_pull_request(&view_with("MERGED", false, "UNKNOWN", "UNKNOWN"), 0)
            .expect("parse");
        let closed = parse_pull_request(&view_with("CLOSED", false, "UNKNOWN", "UNKNOWN"), 0)
            .expect("parse");

        assert_eq!(merged.state, PrState::Merged);
        assert_eq!(closed.state, PrState::Closed);
        assert!(merged.checks.is_empty());
        assert!(!merged.mergeable);
    }

    #[test]
    fn broken_json_is_an_error() {
        assert!(parse_pull_request("not json", 0).is_err());
        assert!(parse_pull_request("{}", 0).is_err());
    }
}
