//! Review: the data behind the diff view. Plain data, no gpui entities.

use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::SharedString;
use swarm_protocol::{
    AgentId as DaemonAgentId, CommentAnchor, CommentEvent, CommentId, CommentState, DiffHunk,
    DiffLine, DiffLineKind, DiffSide, DiffStat, FileChange, FileDiff, Fold, NewComment,
    ReviewComment, ReviewSource, TaskDiff, TaskId,
};

use crate::model::AgentKind;

/// The worktree and the path inside it: what tells two changed files apart.
pub(crate) type FileKey = (PathBuf, PathBuf);

/// Where the lines of a folded file stand, once the user asked for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileLoad {
    /// The daemon is sending the lines.
    Loading,
    /// The last try failed, with a short reason.
    Failed(SharedString),
    /// The lines are in the diff.
    Loaded,
}

/// The loads of one diff, by file.
pub(crate) type FileLoads = HashMap<FileKey, FileLoad>;

/// True while a file shows as one header row: it came folded and its lines are not loaded.
pub(crate) fn is_folded(file: &FileDiff, loads: &FileLoads) -> bool {
    file.fold.is_some()
        && !matches!(
            loads.get(&(file.worktree.clone(), file.path.clone())),
            Some(FileLoad::Loaded)
        )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DiffStyle {
    #[default]
    Unified,
    Split,
}

/// An agent of the open task, as the file lists name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentLabel {
    pub id: DaemonAgentId,
    pub name: SharedString,
    pub kind: AgentKind,
}

/// The files to show: all of them, or those of one agent.
pub(crate) fn visible_files(diff: &TaskDiff, agent: Option<DaemonAgentId>) -> Vec<usize> {
    diff.files
        .iter()
        .enumerate()
        .filter(|(_, file)| agent.is_none() || file.agent == agent)
        .map(|(index, _)| index)
        .collect()
}

fn folder_of(path: &Path) -> String {
    path.parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the view needs to know about one changed file, worked out once per diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub key: FileKey,
    /// The folder with a trailing slash, empty in the root.
    pub folder: SharedString,
    pub name: SharedString,
    pub path: SharedString,
    /// Changes when the change of the file does, also while its lines are not loaded.
    pub digest: u64,
    /// Changed lines the daemon left out because of its caps.
    pub omitted: u32,
}

pub(crate) fn file_infos(diff: &TaskDiff) -> Vec<FileInfo> {
    diff.files.iter().map(file_info).collect()
}

fn file_info(file: &FileDiff) -> FileInfo {
    let name = file
        .path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let folder = folder_of(&file.path);
    let path = file.path.to_string_lossy().into_owned();

    let shown_added = shown_lines(file, DiffLineKind::Added);
    let shown_deleted = shown_lines(file, DiffLineKind::Deleted);
    let omitted =
        file.additions.saturating_sub(shown_added) + file.deletions.saturating_sub(shown_deleted);

    FileInfo {
        key: (file.worktree.clone(), file.path.clone()),
        folder: if folder.is_empty() {
            SharedString::default()
        } else {
            format!("{folder}/").into()
        },
        name: name.into(),
        path: path.into(),
        digest: file.digest,
        omitted,
    }
}

fn shown_lines(file: &FileDiff, kind: DiffLineKind) -> u32 {
    let count = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == kind)
        .count();

    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many worktrees the files come from.
pub(crate) fn worktree_count(diff: &TaskDiff) -> usize {
    let mut worktrees: Vec<&Path> = diff
        .files
        .iter()
        .map(|file| file.worktree.as_path())
        .collect();

    worktrees.sort_unstable();
    worktrees.dedup();

    worktrees.len()
}

/// A folded file of a task and source: the worktree and the path inside it.
pub(crate) type LoadKey = (TaskId, ReviewSource, PathBuf, PathBuf);

#[derive(Clone, Debug)]
struct Lines {
    /// The digest of the file these lines belong to.
    digest: u64,
    hunks: Vec<DiffHunk>,
    truncated: bool,
}

#[derive(Clone, Debug, Default)]
struct Loaded {
    lines: Option<Lines>,
    /// The digest the file had when the last request for its lines went out.
    asked: u64,
    loading: bool,
    error: Option<SharedString>,
}

/// What the app does next, once the daemon answered a request for the lines of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterLoad {
    Nothing,
    /// The diff moved on while the request ran. The file is marked as loading for its new
    /// digest, and the caller asks again.
    LoadAgain,
    /// The answer does not fit the stored diff. The load is marked as failed, and the
    /// caller asks the daemon for the whole diff again.
    FreshDiff,
}

/// The lines of folded files the user asked for. A new diff of the daemon comes without
/// them, so they are kept here and put back while the digest of the file still matches.
/// Lines of another digest are never shown.
#[derive(Clone, Debug, Default)]
pub(crate) struct LoadedFiles {
    files: HashMap<LoadKey, Loaded>,
}

fn file_of<'a>(diff: &'a TaskDiff, key: &LoadKey) -> Option<&'a FileDiff> {
    diff.files
        .iter()
        .find(|file| file.worktree == key.2 && file.path == key.3)
}

impl LoadedFiles {
    /// Marks the file as loading, for the `digest` it has now. False when it is loading
    /// already.
    pub(crate) fn begin(&mut self, key: &LoadKey, digest: u64) -> bool {
        let loaded = self.files.entry(key.clone()).or_default();

        if loaded.loading {
            return false;
        }

        loaded.loading = true;
        loaded.error = None;
        loaded.asked = digest;

        true
    }

    /// Settles a load with the daemon's answer, and merges the lines into `diff`. The lines
    /// go in only when the stored file is still folded and has the digest of the answer.
    /// A load of a task that closed in the meantime changes nothing.
    pub(crate) fn finish(
        &mut self,
        key: &LoadKey,
        answer: Result<Option<FileDiff>, SharedString>,
        diff: Option<&mut Rc<TaskDiff>>,
    ) -> AfterLoad {
        let Some(asked) = self.files.get(key).map(|loaded| loaded.asked) else {
            return AfterLoad::Nothing;
        };

        let current = diff
            .as_deref()
            .and_then(|diff| file_of(diff, key))
            .filter(|file| file.fold.is_some())
            .map(|file| file.digest);
        let (Some(digest), Some(diff)) = (current, diff) else {
            self.files.remove(key);

            return AfterLoad::Nothing;
        };

        if digest != asked {
            if let Some(loaded) = self.files.get_mut(key) {
                loaded.asked = digest;
            }

            return AfterLoad::LoadAgain;
        }

        match answer {
            Ok(Some(file)) if file.digest == digest => {
                self.files.insert(
                    key.clone(),
                    Loaded {
                        lines: Some(Lines {
                            digest,
                            hunks: file.hunks,
                            truncated: file.truncated,
                        }),
                        asked,
                        loading: false,
                        error: None,
                    },
                );
                self.merge(key, Rc::make_mut(diff));

                AfterLoad::Nothing
            }
            Ok(Some(_)) => {
                self.fail(key, "Changed while loading".into());

                AfterLoad::FreshDiff
            }
            Ok(None) => {
                self.fail(key, "No longer in the diff".into());

                AfterLoad::FreshDiff
            }
            Err(message) => {
                self.fail(key, message);

                AfterLoad::Nothing
            }
        }
    }

    /// Ends a load that failed. Lines kept earlier stay, but they are shown only for the
    /// digest they belong to.
    fn fail(&mut self, key: &LoadKey, message: SharedString) {
        if let Some(loaded) = self.files.get_mut(key) {
            loaded.loading = false;
            loaded.error = Some(message);
        }
    }

    pub(crate) fn forget_task(&mut self, task: TaskId) {
        self.files.retain(|key, _| key.0 != task);
    }

    /// The state of every folded file of `diff` the user asked for.
    pub(crate) fn loads(&self, diff: &TaskDiff) -> FileLoads {
        diff.files
            .iter()
            .filter(|file| file.fold.is_some())
            .filter_map(|file| {
                let key = (
                    diff.task,
                    diff.source,
                    file.worktree.clone(),
                    file.path.clone(),
                );
                let loaded = self.files.get(&key)?;
                let current = loaded
                    .lines
                    .as_ref()
                    .is_some_and(|lines| lines.digest == file.digest);

                let load = if current {
                    FileLoad::Loaded
                } else if loaded.loading {
                    FileLoad::Loading
                } else {
                    FileLoad::Failed(loaded.error.clone()?)
                };

                Some(((key.2, key.3), load))
            })
            .collect()
    }

    /// Puts the kept lines into the folded files of a diff that just came in, and forgets
    /// files the diff no longer folds. A file with another digest than its lines stays
    /// without lines, see [`LoadedFiles::begin_stale`].
    pub(crate) fn restore(&mut self, diff: &mut TaskDiff) {
        let (task, source) = (diff.task, diff.source);
        let folded: HashSet<(&Path, &Path)> = diff
            .files
            .iter()
            .filter(|file| file.fold.is_some())
            .map(|file| (file.worktree.as_path(), file.path.as_path()))
            .collect();

        self.files.retain(|key, loaded| {
            key.0 != task
                || key.1 != source
                || loaded.loading
                || folded.contains(&(key.2.as_path(), key.3.as_path()))
        });

        for file in diff.files.iter_mut().filter(|file| file.fold.is_some()) {
            let key = (task, source, file.worktree.clone(), file.path.clone());

            if let Some(lines) = self
                .files
                .get(&key)
                .and_then(|loaded| loaded.lines.as_ref())
                .filter(|lines| lines.digest == file.digest)
            {
                file.hunks.clone_from(&lines.hunks);
                file.truncated = lines.truncated;
            }
        }
    }

    /// Puts the kept lines of one file into the diff. The file keeps its fold. Lines of
    /// another digest than the file has stay out.
    fn merge(&self, key: &LoadKey, diff: &mut TaskDiff) {
        let Some(lines) = self.files.get(key).and_then(|loaded| loaded.lines.as_ref()) else {
            return;
        };

        let file = diff.files.iter_mut().find(|file| {
            file.worktree == key.2
                && file.path == key.3
                && file.fold.is_some()
                && file.digest == lines.digest
        });

        if let Some(file) = file {
            file.hunks.clone_from(&lines.hunks);
            file.truncated = lines.truncated;
        }
    }

    /// Marks the folded files of a diff as loading when the user asked for them and their
    /// digest is not the one of the last request. Returns the files to ask for.
    pub(crate) fn begin_stale(&mut self, diff: &TaskDiff) -> Vec<LoadKey> {
        let mut started = Vec::new();

        for file in diff.files.iter().filter(|file| file.fold.is_some()) {
            let key = (
                diff.task,
                diff.source,
                file.worktree.clone(),
                file.path.clone(),
            );
            let stale = self
                .files
                .get(&key)
                .is_some_and(|loaded| !loaded.loading && loaded.asked != file.digest);

            if stale && self.begin(&key, file.digest) {
                started.push(key);
            }
        }

        started
    }
}

/// A line of a hunk with its number on one side, 0 when it has none there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Side {
    pub line: u32,
    pub number: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    Binary,
    Empty,
    /// Changed lines left out of the file.
    Truncated(u32),
    /// The whole file was left out.
    TooLarge,
}

/// One row of the flattened body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Header {
        file: u32,
    },
    Hunk {
        file: u32,
        hunk: u32,
    },
    /// A unified line. `old` and `new` are 0 when the line has no number on that side.
    Line {
        file: u32,
        hunk: u32,
        line: u32,
        old: u32,
        new: u32,
    },
    /// A split row: old on the left, new on the right.
    Pair {
        file: u32,
        hunk: u32,
        left: Option<Side>,
        right: Option<Side>,
    },
    Note {
        file: u32,
        note: Note,
    },
    /// A comment thread: an index into [`Marks::threads`].
    Thread {
        file: u32,
        thread: u32,
    },
    /// The box for a new comment, under the line of [`Marks::composer`].
    Composer {
        file: u32,
    },
    /// The space under the last card.
    End,
}

impl Row {
    pub(crate) fn file(self) -> Option<usize> {
        let file = match self {
            Self::Header { file }
            | Self::Hunk { file, .. }
            | Self::Line { file, .. }
            | Self::Pair { file, .. }
            | Self::Note { file, .. }
            | Self::Thread { file, .. }
            | Self::Composer { file } => file,
            Self::End => return None,
        };

        Some(file as usize)
    }

    /// True for the first row of a card.
    pub(crate) fn starts_card(self) -> bool {
        matches!(self, Self::Header { .. })
    }

    /// True when a card cannot go on past this row.
    pub(crate) fn ends_card(self) -> bool {
        matches!(self, Self::Header { .. } | Self::End)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyRows {
    pub rows: Vec<Row>,
    /// The row of each file's header, `None` for a file that is not shown.
    pub headers: Vec<Option<usize>>,
}

impl BodyRows {
    /// The rows of a file: its header up to the next card.
    pub(crate) fn range_of(&self, file: usize) -> Option<std::ops::Range<usize>> {
        let start = (*self.headers.get(file)?)?;
        let len = self
            .rows
            .get(start + 1..)?
            .iter()
            .position(|row| row.ends_card())
            .unwrap_or(self.rows.len() - start - 1);

        Some(start..start + 1 + len)
    }
}

/// Flattens the shown files into rows. A collapsed file and a folded file keep only their
/// header.
pub(crate) fn build_rows(
    diff: &TaskDiff,
    infos: &[FileInfo],
    shown: &[usize],
    (collapsed, folded): (impl Fn(usize) -> bool, impl Fn(usize) -> bool),
    style: DiffStyle,
    marks: &Marks,
) -> BodyRows {
    let lookup = Lookup::new(marks);
    let mut body = BodyRows {
        rows: Vec::new(),
        headers: vec![None; diff.files.len()],
    };

    for index in shown {
        let Some(file) = diff.files.get(*index) else {
            continue;
        };
        let at = row_index(*index);

        if let Some(slot) = body.headers.get_mut(*index) {
            *slot = Some(body.rows.len());
        }
        body.rows.push(Row::Header { file: at });

        if collapsed(*index) || folded(*index) {
            continue;
        }

        for thread in lookup.tops(*index) {
            body.rows.push(Row::Thread {
                file: at,
                thread: *thread,
            });
        }

        if marks.composer == Some(ComposerAt::Top { file: *index }) {
            body.rows.push(Row::Composer { file: at });
        }

        if file.binary {
            body.rows.push(Row::Note {
                file: at,
                note: Note::Binary,
            });
            continue;
        }

        if file.hunks.is_empty() && file.truncated {
            body.rows.push(Row::Note {
                file: at,
                note: Note::TooLarge,
            });
            continue;
        }

        if file.hunks.is_empty() {
            body.rows.push(Row::Note {
                file: at,
                note: Note::Empty,
            });
            continue;
        }

        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let hunk_at = row_index(hunk_index);
            body.rows.push(Row::Hunk {
                file: at,
                hunk: hunk_at,
            });

            let start = body.rows.len();

            match style {
                DiffStyle::Unified => unified_rows(&mut body.rows, at, hunk_at, hunk),
                DiffStyle::Split => split_rows(&mut body.rows, at, hunk_at, hunk),
            }

            if !lookup.is_empty() {
                lookup.add_after_lines(&mut body.rows, start);
            }
        }

        if file.truncated {
            let omitted = infos.get(*index).map_or(0, |info| info.omitted);

            body.rows.push(Row::Note {
                file: at,
                note: Note::Truncated(omitted),
            });
        }
    }

    body.rows.push(Row::End);

    body
}

fn row_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

fn unified_rows(rows: &mut Vec<Row>, file: u32, hunk: u32, data: &DiffHunk) {
    let (mut old, mut new) = (data.old_start, data.new_start);

    for (index, line) in data.lines.iter().enumerate() {
        let (old_number, new_number) = match line.kind {
            DiffLineKind::Context => (old, new),
            DiffLineKind::Deleted => (old, 0),
            DiffLineKind::Added => (0, new),
            DiffLineKind::NoNewline => (0, 0),
        };

        if matches!(line.kind, DiffLineKind::Context | DiffLineKind::Deleted) {
            old += 1;
        }
        if matches!(line.kind, DiffLineKind::Context | DiffLineKind::Added) {
            new += 1;
        }

        rows.push(Row::Line {
            file,
            hunk,
            line: row_index(index),
            old: old_number,
            new: new_number,
        });
    }
}

/// Pairs the deletions of a change block with its additions, in order.
fn split_rows(rows: &mut Vec<Row>, file: u32, hunk: u32, data: &DiffHunk) {
    let (mut old, mut new) = (data.old_start, data.new_start);
    let mut index = 0;

    while index < data.lines.len() {
        let Some(line) = data.lines.get(index) else {
            break;
        };

        if line.kind == DiffLineKind::NoNewline {
            let side = Side {
                line: row_index(index),
                number: 0,
            };

            rows.push(Row::Pair {
                file,
                hunk,
                left: Some(side),
                right: Some(side),
            });
            index += 1;
            continue;
        }

        if line.kind == DiffLineKind::Context {
            rows.push(Row::Pair {
                file,
                hunk,
                left: Some(Side {
                    line: row_index(index),
                    number: old,
                }),
                right: Some(Side {
                    line: row_index(index),
                    number: new,
                }),
            });
            old += 1;
            new += 1;
            index += 1;
            continue;
        }

        let mut deleted = Vec::new();
        let mut added = Vec::new();
        let mut marks: (Option<usize>, Option<usize>) = (None, None);
        let mut last = DiffLineKind::Context;

        while let Some(line) = data.lines.get(index) {
            match line.kind {
                DiffLineKind::Context => break,
                DiffLineKind::Deleted => deleted.push(index),
                DiffLineKind::Added => added.push(index),
                DiffLineKind::NoNewline if last == DiffLineKind::Deleted => marks.0 = Some(index),
                DiffLineKind::NoNewline => marks.1 = Some(index),
            }
            if line.kind != DiffLineKind::NoNewline {
                last = line.kind;
            }
            index += 1;
        }

        for position in 0..deleted.len().max(added.len()) {
            let side = |lines: &[usize], start: u32| {
                lines.get(position).map(|line| Side {
                    line: row_index(*line),
                    number: start + row_index(position),
                })
            };

            rows.push(Row::Pair {
                file,
                hunk,
                left: side(&deleted, old),
                right: side(&added, new),
            });
        }

        if marks.0.is_some() || marks.1.is_some() {
            let mark = |line: Option<usize>| {
                line.map(|line| Side {
                    line: row_index(line),
                    number: 0,
                })
            };

            rows.push(Row::Pair {
                file,
                hunk,
                left: mark(marks.0),
                right: mark(marks.1),
            });
        }

        old += row_index(deleted.len());
        new += row_index(added.len());
    }
}

/// A line of a file diff that a comment can sit on: indexes into the diff, with the
/// line's index inside its hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LinePos {
    pub file: u32,
    pub hunk: u32,
    pub line: u32,
    pub side: DiffSide,
}

/// The side a line of this kind can be commented on. `NoNewline` has none.
pub(crate) fn side_of(kind: DiffLineKind) -> Option<DiffSide> {
    match kind {
        DiffLineKind::Deleted => Some(DiffSide::Old),
        DiffLineKind::Added | DiffLineKind::Context => Some(DiffSide::New),
        DiffLineKind::NoNewline => None,
    }
}

fn on_side(kind: DiffLineKind, side: DiffSide) -> bool {
    match side {
        DiffSide::Old => matches!(kind, DiffLineKind::Context | DiffLineKind::Deleted),
        DiffSide::New => matches!(kind, DiffLineKind::Context | DiffLineKind::Added),
    }
}

/// The numbers of a hunk's lines as `(old, new)`, 0 where a line has none.
pub(crate) fn hunk_numbers(hunk: &DiffHunk) -> Vec<(u32, u32)> {
    let (mut old, mut new) = (hunk.old_start, hunk.new_start);

    hunk.lines
        .iter()
        .map(|line| {
            let numbers = match line.kind {
                DiffLineKind::Context => (old, new),
                DiffLineKind::Deleted => (old, 0),
                DiffLineKind::Added => (0, new),
                DiffLineKind::NoNewline => (0, 0),
            };

            if matches!(line.kind, DiffLineKind::Context | DiffLineKind::Deleted) {
                old += 1;
            }
            if matches!(line.kind, DiffLineKind::Context | DiffLineKind::Added) {
                new += 1;
            }

            numbers
        })
        .collect()
}

/// The comment anchor of a line: its number and text on `side`, and the texts of the
/// lines just before and after on that side, `None` at the hunk's edges.
pub(crate) fn anchor_for(
    file: &FileDiff,
    hunk: usize,
    line: usize,
    side: DiffSide,
) -> Option<CommentAnchor> {
    let data = file.hunks.get(hunk)?;
    let current = data.lines.get(line)?;

    if !on_side(current.kind, side) {
        return None;
    }

    let numbers = hunk_numbers(data);
    let (old, new) = *numbers.get(line)?;
    let number = match side {
        DiffSide::Old => old,
        DiffSide::New => new,
    };

    let same_side: Vec<&DiffLine> = data
        .lines
        .iter()
        .filter(|other| on_side(other.kind, side))
        .collect();
    let position = same_side
        .iter()
        .position(|other| std::ptr::eq(*other, current))?;

    Some(CommentAnchor {
        worktree: file.worktree.clone(),
        path: file.path.clone(),
        side,
        line: number,
        text: current.text.clone(),
        before: position
            .checked_sub(1)
            .and_then(|before| same_side.get(before))
            .map(|line| line.text.clone()),
        after: same_side.get(position + 1).map(|line| line.text.clone()),
    })
}

/// The hunk and line index of the line numbered `number` on `side`.
pub(crate) fn find_line(file: &FileDiff, side: DiffSide, number: u32) -> Option<(u32, u32)> {
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        for (line_index, (old, new)) in hunk_numbers(hunk).into_iter().enumerate() {
            let here = match side {
                DiffSide::Old => old,
                DiffSide::New => new,
            };
            let kind_fits = hunk
                .lines
                .get(line_index)
                .is_some_and(|line| on_side(line.kind, side));

            if here == number && kind_fits {
                return Some((row_index(hunk_index), row_index(line_index)));
            }
        }
    }

    None
}

/// The number of the line an anchor sits on now, among the lines of its side that the
/// diff shows. Same rule as the daemon: equal text, one point for an equal line before
/// and one after, and the nearest to the old number on a tie.
pub(crate) fn refind_line(file: &FileDiff, anchor: &CommentAnchor) -> Option<u32> {
    // Equal neighbors, then nearest to the old number, then the earlier line.
    type Rank = (u8, Reverse<u32>, Reverse<u32>);

    let mut best: Option<(Rank, u32)> = None;

    for hunk in &file.hunks {
        let numbers = hunk_numbers(hunk);
        let same_side: Vec<(u32, &str)> = hunk
            .lines
            .iter()
            .zip(numbers)
            .filter(|(line, _)| on_side(line.kind, anchor.side))
            .map(|(line, (old, new))| {
                let number = match anchor.side {
                    DiffSide::Old => old,
                    DiffSide::New => new,
                };

                (number, line.text.as_str())
            })
            .collect();

        for (position, (number, text)) in same_side.iter().enumerate() {
            if *text != anchor.text {
                continue;
            }

            let before = position > 0
                && anchor.before.as_deref() == same_side.get(position - 1).map(|other| other.1);
            let after = anchor.after.is_some()
                && anchor.after.as_deref() == same_side.get(position + 1).map(|other| other.1);
            let rank = (
                u8::from(before) + u8::from(after),
                Reverse(number.abs_diff(anchor.line)),
                Reverse(*number),
            );

            if best.is_none_or(|(other, _)| rank > other) {
                best = Some((rank, *number));
            }
        }
    }

    best.map(|(_, number)| number)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Under a line, as hunk and line index.
    Line { hunk: u32, line: u32 },
    /// At the top of the file card: outdated, or on a line no hunk holds.
    Top,
    /// The file is folded and its lines are not loaded, so the thread has no place yet.
    Folded,
}

/// A top-level comment placed on a file of the diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Thread {
    /// Index into the comments slice that was placed.
    pub root: usize,
    pub file: usize,
    pub side: DiffSide,
    pub place: Place,
}

/// Places the top-level comments made on the diff's source on its files. Comments of
/// other sources, and of files the diff does not hold, are left out. A file that is still
/// folded has no lines to place them on, so its threads wait as [`Place::Folded`]. The
/// result is ordered by file, then top before lines, then position, then age.
pub(crate) fn place_threads(
    diff: &TaskDiff,
    comments: &[ReviewComment],
    loads: &FileLoads,
) -> Vec<Thread> {
    let files: HashMap<FileKey, usize> = diff
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| ((file.worktree.clone(), file.path.clone()), index))
        .collect();
    let mut threads = Vec::new();

    for (root, comment) in comments.iter().enumerate() {
        if comment.parent.is_some() || comment.source != diff.source {
            continue;
        }

        let key = (comment.anchor.worktree.clone(), comment.anchor.path.clone());
        let Some(file) = files.get(&key).copied() else {
            continue;
        };
        let folded = diff
            .files
            .get(file)
            .is_some_and(|data| is_folded(data, loads));
        let found = if comment.outdated || folded {
            None
        } else {
            diff.files
                .get(file)
                .and_then(|data| find_line(data, comment.anchor.side, comment.anchor.line))
        };
        let place = if folded {
            Place::Folded
        } else {
            found.map_or(Place::Top, |(hunk, line)| Place::Line { hunk, line })
        };

        threads.push(Thread {
            root,
            file,
            side: comment.anchor.side,
            place,
        });
    }

    threads.sort_by_key(|thread| {
        let order = match thread.place {
            Place::Top | Place::Folded => (0, 0, 0),
            Place::Line { hunk, line } => (1, hunk, line),
        };

        (thread.file, order, thread.root)
    });

    threads
}

/// Where the comment box sits in a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerAt {
    /// Under a line.
    Line(LinePos),
    /// At the top of the file card: its line is gone.
    Top { file: usize },
}

/// What the rows need besides the diff: the placed threads and the open composer.
#[derive(Clone, Debug, Default)]
pub(crate) struct Marks {
    pub threads: Rc<[Thread]>,
    pub composer: Option<ComposerAt>,
}

/// Finds the threads of a line quickly while rows are built.
struct Lookup<'a> {
    marks: &'a Marks,
    tops: HashMap<usize, Vec<u32>>,
    lines: HashMap<(usize, u32, u32), Vec<u32>>,
}

impl<'a> Lookup<'a> {
    fn new(marks: &'a Marks) -> Self {
        let mut tops: HashMap<usize, Vec<u32>> = HashMap::new();
        let mut lines: HashMap<(usize, u32, u32), Vec<u32>> = HashMap::new();

        for (index, thread) in marks.threads.iter().enumerate() {
            let index = row_index(index);

            match thread.place {
                Place::Folded => {}
                Place::Top => tops.entry(thread.file).or_default().push(index),
                Place::Line { hunk, line } => lines
                    .entry((thread.file, hunk, line))
                    .or_default()
                    .push(index),
            }
        }

        Self { marks, tops, lines }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.marks.composer.is_none()
    }

    fn tops(&self, file: usize) -> &[u32] {
        self.tops.get(&file).map_or(&[], Vec::as_slice)
    }

    /// Adds the thread and composer rows under the lines among `rows[start..]`.
    fn add_after_lines(&self, rows: &mut Vec<Row>, start: usize) {
        let generated = rows.split_off(start);

        for row in generated {
            rows.push(row);

            match row {
                Row::Line {
                    file, hunk, line, ..
                } => self.push_under(rows, file, hunk, line, None),
                Row::Pair {
                    file,
                    hunk,
                    left,
                    right,
                } => {
                    if let Some(side) = left {
                        self.push_under(rows, file, hunk, side.line, Some(DiffSide::Old));
                    }
                    if let Some(side) = right {
                        self.push_under(rows, file, hunk, side.line, Some(DiffSide::New));
                    }
                }
                _ => {}
            }
        }
    }

    fn push_under(
        &self,
        rows: &mut Vec<Row>,
        file: u32,
        hunk: u32,
        line: u32,
        side: Option<DiffSide>,
    ) {
        let fits = |other: DiffSide| side.is_none_or(|side| side == other);

        if let Some(threads) = self.lines.get(&(file as usize, hunk, line)) {
            for thread in threads {
                let placed = self.marks.threads.get(*thread as usize);

                if placed.is_some_and(|placed| fits(placed.side)) {
                    rows.push(Row::Thread {
                        file,
                        thread: *thread,
                    });
                }
            }
        }

        let composer = match self.marks.composer {
            Some(ComposerAt::Line(at)) => {
                at.file == file && at.hunk == hunk && at.line == line && fits(at.side)
            }
            _ => false,
        };

        if composer {
            rows.push(Row::Composer { file });
        }
    }
}

/// How many comments of each state a task has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CommentCounts {
    pub pending: u32,
    pub sent: u32,
    pub resolved: u32,
}

pub(crate) fn count_comments(comments: &[ReviewComment]) -> CommentCounts {
    let mut counts = CommentCounts::default();

    for comment in comments {
        match comment.state {
            CommentState::Pending => counts.pending += 1,
            CommentState::Sent => counts.sent += 1,
            CommentState::Resolved => counts.resolved += 1,
        }
    }

    counts
}

impl CommentCounts {
    /// "2 pending comments", or "3 sent · 1 pending" when more than one state shows.
    pub(crate) fn status_text(self) -> Option<String> {
        let parts: Vec<(u32, &str)> = [
            (self.sent, "sent"),
            (self.pending, "pending"),
            (self.resolved, "resolved"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .collect();

        match parts.as_slice() {
            [] => None,
            [(count, word)] => Some(format!(
                "{count} {word} {}",
                if *count == 1 { "comment" } else { "comments" }
            )),
            many => Some(
                many.iter()
                    .map(|(count, word)| format!("{count} {word}"))
                    .collect::<Vec<_>>()
                    .join(" \u{b7} "),
            ),
        }
    }
}

/// Applies a daemon event to a task's comments, kept oldest first.
pub(crate) fn apply_comment_event(comments: &mut Vec<ReviewComment>, event: &CommentEvent) {
    match event {
        CommentEvent::Changed(comment) => {
            if let Some(slot) = comments.iter_mut().find(|other| other.id == comment.id) {
                *slot = (**comment).clone();
            } else {
                comments.push((**comment).clone());
                comments.sort_by_key(|other| other.id);
            }
        }
        CommentEvent::Deleted { id, .. } => {
            comments.retain(|other| other.id != *id && other.parent != Some(*id));
        }
    }
}

/// The comment the sample (no daemon) makes of a new one: Live goes at once, Merged
/// waits, and a reply follows its parent.
pub(crate) fn fake_add(
    list: &mut Vec<ReviewComment>,
    new: NewComment,
    owner: Option<DaemonAgentId>,
    now: u64,
) {
    let id = CommentId(list.iter().map(|comment| comment.id.0).max().unwrap_or(0) + 1);
    let waits = match new.parent {
        Some(parent) => list
            .iter()
            .find(|comment| comment.id == parent)
            .is_some_and(|comment| comment.state == CommentState::Pending),
        None => new.source == ReviewSource::Merged,
    };

    list.push(ReviewComment {
        id,
        task: new.task,
        parent: new.parent,
        source: new.source,
        anchor: new.anchor,
        body: new.body,
        state: if waits {
            CommentState::Pending
        } else {
            CommentState::Sent
        },
        to: if waits { None } else { owner },
        outdated: false,
        created_at: now,
        sent_at: (!waits).then_some(now),
        delivered_at: (!waits).then_some(now),
    });
}

/// Marks a thread resolved, or sent again.
pub(crate) fn fake_resolve(list: &mut [ReviewComment], id: CommentId, resolved: bool) {
    if let Some(comment) = list.iter_mut().find(|comment| comment.id == id) {
        comment.state = if resolved {
            CommentState::Resolved
        } else {
            CommentState::Sent
        };
    }
}

/// Deletes a pending comment and its replies.
pub(crate) fn fake_delete(list: &mut Vec<ReviewComment>, id: CommentId) {
    let pending = list
        .iter()
        .any(|comment| comment.id == id && comment.state == CommentState::Pending);

    if pending {
        list.retain(|comment| comment.id != id && comment.parent != Some(id));
    }
}

/// Sends every pending comment. Returns false when none waited.
pub(crate) fn fake_send(
    list: &mut [ReviewComment],
    owner: impl Fn(&ReviewComment) -> Option<DaemonAgentId>,
    now: u64,
) -> bool {
    let mut sent = false;

    for comment in list.iter_mut() {
        if comment.state == CommentState::Pending {
            comment.to = owner(comment);
            comment.state = CommentState::Sent;
            comment.sent_at = Some(now);
            comment.delivered_at = Some(now);
            sent = true;
        }
    }

    sent
}

/// How long ago, as "40s", "6m", "2h" or "3d". Under 5 seconds is "now".
pub(crate) fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then) / 1_000;

    if seconds < 5 {
        "now".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// The first added or deleted line of a file, as a position.
pub(crate) fn first_changed_line(file: &FileDiff, index: usize) -> Option<LinePos> {
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let side = match line.kind {
                DiffLineKind::Added => DiffSide::New,
                DiffLineKind::Deleted => DiffSide::Old,
                DiffLineKind::Context | DiffLineKind::NoNewline => continue,
            };

            return Some(LinePos {
                file: row_index(index),
                hunk: row_index(hunk_index),
                line: row_index(line_index),
                side,
            });
        }
    }

    None
}

fn find_file<'a>(diff: &'a TaskDiff, name: &str) -> Option<(usize, &'a FileDiff)> {
    diff.files
        .iter()
        .enumerate()
        .find(|(_, file)| file.path.ends_with(name))
}

/// Sample comments for the sample task: a pending one and a deleted-side one on Merged;
/// a sent thread with a reply, a resolved thread and an outdated comment on Live.
pub(crate) fn sample_comments(
    live: &TaskDiff,
    merged: &TaskDiff,
    agents: [DaemonAgentId; 3],
    now: u64,
) -> Vec<ReviewComment> {
    let task = merged.task;
    let find = find_file;
    let first = |diff: &TaskDiff, name: &str| {
        let (index, file) = find(diff, name)?;
        let at = first_changed_line(file, index)?;

        anchor_for(file, at.hunk as usize, at.line as usize, at.side)
    };
    let made = |id: u64,
                parent: Option<u64>,
                source: ReviewSource,
                anchor: CommentAnchor,
                body: &str,
                state: CommentState,
                age: u64| {
        let sent = state != CommentState::Pending;

        ReviewComment {
            id: CommentId(id),
            task,
            parent: parent.map(CommentId),
            source,
            anchor,
            body: body.to_owned(),
            state,
            to: sent.then_some(agents[0]),
            outdated: false,
            created_at: now.saturating_sub(age),
            sent_at: sent.then(|| now.saturating_sub(age).saturating_add(1_000)),
            delivered_at: sent.then(|| now.saturating_sub(age).saturating_add(3_000)),
        }
    };

    let mut comments = Vec::new();

    if let Some(anchor) = first(merged, "element.rs") {
        comments.push(made(
            1,
            None,
            ReviewSource::Merged,
            anchor,
            "Cache shaped lines per row and only reshape dirty rows. A full reshape every frame will hurt at `200\u{d7}60`.",
            CommentState::Pending,
            300_000,
        ));
    }

    if let Some(anchor) = first(merged, "grid.rs") {
        comments.push(made(
            2,
            None,
            ReviewSource::Merged,
            anchor,
            "Why not a ring buffer here?",
            CommentState::Pending,
            120_000,
        ));
    }

    if let Some(anchor) = first(live, "element.rs") {
        comments.push(made(
            3,
            None,
            ReviewSource::Live,
            anchor.clone(),
            "Cache shaped lines per row and only reshape dirty rows. A full reshape every frame will hurt at `200\u{d7}60`.",
            CommentState::Sent,
            41_000,
        ));
        comments.push(made(
            4,
            Some(3),
            ReviewSource::Live,
            anchor,
            "Also check `dirty.contains` after a resize.",
            CommentState::Sent,
            20_000,
        ));
    }

    if let Some(anchor) = first(live, "grid.rs") {
        comments.push(made(
            5,
            None,
            ReviewSource::Live,
            anchor,
            "Rename `cell_0` to something that says what it holds.",
            CommentState::Resolved,
            900_000,
        ));
    }

    if let Some((_, file)) = find(live, "pty.rs") {
        let anchor = CommentAnchor {
            worktree: file.worktree.clone(),
            path: file.path.clone(),
            side: DiffSide::New,
            line: 60,
            text: "        let stale = old_buffer.take();".to_owned(),
            before: None,
            after: None,
        };
        let mut outdated = made(
            6,
            None,
            ReviewSource::Live,
            anchor,
            "Drop the `take` here, the buffer is reused.",
            CommentState::Sent,
            600_000,
        );

        outdated.outdated = true;
        comments.push(outdated);
    }

    comments
}

/// `1204` as `1,204`.
pub(crate) fn thousands(number: u32) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);

    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }

    out
}

pub(crate) fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

const SAMPLE_FILES: [(usize, &str, FileChange, u32, u32); 9] = [
    (
        0,
        "crates/swarm-term/src/element.rs",
        FileChange::Modified,
        48,
        12,
    ),
    (
        0,
        "crates/swarm-term/src/grid.rs",
        FileChange::Modified,
        22,
        9,
    ),
    (0, "crates/swarm-term/src/font.rs", FileChange::Added, 18, 0),
    (0, "benches/grid.rs", FileChange::Modified, 6, 0),
    (
        1,
        "crates/swarm-term/src/pty.rs",
        FileChange::Modified,
        39,
        8,
    ),
    (
        1,
        "crates/swarm-term/Cargo.toml",
        FileChange::Modified,
        2,
        0,
    ),
    (
        2,
        "crates/swarm-ui/src/tokens.rs",
        FileChange::Added,
        142,
        0,
    ),
    (
        2,
        "crates/swarm-ui/src/theme.rs",
        FileChange::Modified,
        38,
        12,
    ),
    (2, "crates/swarm-ui/src/lib.rs", FileChange::Modified, 6, 0),
];

/// Files of the sample task that come folded: pty-io's lock file and a large file of
/// term-render.
const SAMPLE_FOLDED: [(usize, &str, FileChange, u32, u32, Fold); 2] = [
    (
        1,
        "Cargo.lock",
        FileChange::Modified,
        1_690,
        312,
        Fold::Lock,
    ),
    (
        0,
        "crates/swarm-term/src/glyphs.rs",
        FileChange::Modified,
        612,
        41,
        Fold::Large,
    ),
];

/// The sample task's diff. `agents` are term-render, pty-io and ui-tokens. Live keeps the
/// files of the first two in their own worktrees; Merged holds all in the task worktree.
pub(crate) fn sample_diff(
    task: TaskId,
    source: ReviewSource,
    agents: [DaemonAgentId; 3],
) -> TaskDiff {
    let task_worktree = PathBuf::from("/work/swarm/terminal-renderer");
    let own = [
        PathBuf::from("/work/swarm/terminal-renderer--term-render"),
        PathBuf::from("/work/swarm/terminal-renderer--pty-io"),
    ];

    let folded = SAMPLE_FOLDED
        .iter()
        .map(|(agent, path, change, additions, deletions, _)| {
            (*agent, *path, *change, *additions, *deletions)
        });
    let mut files: Vec<FileDiff> = SAMPLE_FILES
        .iter()
        .copied()
        .chain(folded)
        .map(|(agent, path, change, additions, deletions)| {
            let worktree = match (source, own.get(agent)) {
                (ReviewSource::Live, Some(worktree)) => worktree.clone(),
                _ => task_worktree.clone(),
            };

            generated_file(
                worktree,
                path,
                agents.get(agent).copied(),
                change,
                additions,
                deletions,
            )
        })
        .collect();
    for file in &mut files {
        let shows_edge_cases =
            file.path.ends_with("benches/grid.rs") || file.path.ends_with("swarm-ui/src/lib.rs");

        if shows_edge_cases {
            file.mode_change = Some((0o100_644, 0o100_755));
        }

        if shows_edge_cases && let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::NoNewline,
                text: String::new(),
            });
        }

        file.digest = sample_digest(file);

        let fold = SAMPLE_FOLDED
            .iter()
            .find(|(_, path, ..)| file.path.ends_with(path))
            .map(|(.., fold)| *fold);

        if fold.is_some() {
            file.hunks.clear();
            file.fold = fold;
        }
    }
    files.sort_by(|a, b| (&a.worktree, &a.path).cmp(&(&b.worktree, &b.path)));

    let stat = DiffStat {
        files: row_index(files.len()),
        additions: files.iter().map(|file| file.additions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
    };

    TaskDiff {
        task,
        source,
        fingerprint: match source {
            ReviewSource::Live => 0x11,
            ReviewSource::Merged => 0x22,
        },
        head: "sample".to_owned(),
        stat,
        files,
    }
}

const CHANGES_PER_HUNK: u32 = 14;
const CONTEXT: u32 = 3;

/// A file with `additions` and `deletions` spread over hunks of made-up Rust.
pub(crate) fn generated_file(
    worktree: PathBuf,
    path: &str,
    agent: Option<DaemonAgentId>,
    change: FileChange,
    additions: u32,
    deletions: u32,
) -> FileDiff {
    FileDiff {
        worktree,
        path: PathBuf::from(path),
        agent,
        change,
        mode_change: None,
        additions,
        deletions,
        binary: false,
        hunks: sample_hunks(path, change, additions, deletions),
        truncated: false,
        digest: 0,
        fold: None,
    }
}

/// The file as the sample daemon sends it when asked for its lines.
pub(crate) fn sample_lines(file: &FileDiff) -> FileDiff {
    FileDiff {
        hunks: sample_hunks(
            &file.path.to_string_lossy(),
            file.change,
            file.additions,
            file.deletions,
        ),
        truncated: false,
        ..file.clone()
    }
}

/// A hash of the sample file's change, so each sample file has its own stable digest.
fn sample_digest(file: &FileDiff) -> u64 {
    let mut hasher = DefaultHasher::new();

    file.path.hash(&mut hasher);
    (file.change as u8).hash(&mut hasher);
    file.binary.hash(&mut hasher);
    file.truncated.hash(&mut hasher);
    file.mode_change.hash(&mut hasher);
    file.additions.hash(&mut hasher);
    file.deletions.hash(&mut hasher);

    for hunk in &file.hunks {
        hunk.old_start.hash(&mut hasher);
        hunk.old_lines.hash(&mut hasher);
        hunk.new_start.hash(&mut hasher);
        hunk.new_lines.hash(&mut hasher);
        hunk.section.hash(&mut hasher);

        for line in &hunk.lines {
            (line.kind as u8).hash(&mut hasher);
            line.text.hash(&mut hasher);
        }
    }

    hasher.finish()
}

fn sample_hunks(path: &str, change: FileChange, additions: u32, deletions: u32) -> Vec<DiffHunk> {
    let section = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".rs"))
        .map(|name| format!("impl {} ", name.replace(['-', '_'], "")))
        .unwrap_or_default();

    if change == FileChange::Added {
        vec![DiffHunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: additions,
            section: String::new(),
            lines: (0..additions)
                .map(|n| DiffLine {
                    kind: DiffLineKind::Added,
                    text: sample_line(n, 1),
                })
                .collect(),
        }]
    } else {
        modified_hunks(&section, additions, deletions)
    }
}

fn modified_hunks(section: &str, additions: u32, deletions: u32) -> Vec<DiffHunk> {
    let count = (additions + deletions).div_ceil(CHANGES_PER_HUNK).max(1);
    let mut hunks = Vec::new();
    let (mut left_add, mut left_del) = (additions, deletions);
    let (mut old_start, mut new_start) = (40_u32, 40_u32);

    for hunk in 0..count {
        let remaining = count - hunk;
        let (hunk_add, hunk_del) = (left_add.div_ceil(remaining), left_del.div_ceil(remaining));
        left_add -= hunk_add;
        left_del -= hunk_del;

        let mut lines = Vec::new();
        for n in 0..CONTEXT {
            lines.push(context_line(hunk * 100 + n));
        }
        for n in 0..hunk_del {
            lines.push(DiffLine {
                kind: DiffLineKind::Deleted,
                text: sample_line(hunk * 100 + n, 0),
            });
        }
        for n in 0..hunk_add {
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: sample_line(hunk * 100 + n, 1),
            });
        }
        for n in 0..CONTEXT {
            lines.push(context_line(hunk * 100 + 50 + n));
        }

        hunks.push(DiffHunk {
            old_start,
            old_lines: CONTEXT * 2 + hunk_del,
            new_start,
            new_lines: CONTEXT * 2 + hunk_add,
            section: format!("{section}for TerminalElement"),
            lines,
        });

        old_start += CONTEXT * 2 + hunk_del + 30;
        new_start += CONTEXT * 2 + hunk_add + 30;
    }

    hunks
}

fn context_line(n: u32) -> DiffLine {
    DiffLine {
        kind: DiffLineKind::Context,
        text: format!("        let cell_{n} = grid.cell(row, column + {});", n % 7),
    }
}

fn sample_line(n: u32, side: u32) -> String {
    match (n % 4, side) {
        (0, 0) => format!("        paint_cell(cell_{n}, cx);"),
        (1, 0) => format!("        let text_{n} = cell_{n}.text().to_string();"),
        (2, 0) => format!("        window.paint_quad(fill(bounds_{n}, background));"),
        (_, 0) => format!("        self.cache.remove(&row_{n});"),
        (0, _) => format!("        shape_line(row_{n}, &font, cx);"),
        (1, _) => format!(
            "        let run_{n} = self.cache.entry(row_{n}).or_insert_with(|| shape(row_{n}));"
        ),
        (2, _) => format!("\tif dirty.contains(row_{n}) {{ reshape(row_{n}, &font, cx); }}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.to_owned(),
        }
    }

    fn file(path: &str, agent: Option<u64>, lines: Vec<DiffLine>) -> FileDiff {
        let count = |kind| u32::try_from(lines.iter().filter(|line| line.kind == kind).count());
        let (additions, deletions) = (
            count(DiffLineKind::Added).unwrap_or(0),
            count(DiffLineKind::Deleted).unwrap_or(0),
        );

        FileDiff {
            worktree: PathBuf::from("/wt"),
            path: PathBuf::from(path),
            agent: agent.map(DaemonAgentId),
            change: FileChange::Modified,
            mode_change: None,
            additions,
            deletions,
            binary: false,
            hunks: vec![DiffHunk {
                old_start: 10,
                old_lines: 0,
                new_start: 20,
                new_lines: 0,
                section: "fn main".to_owned(),
                lines,
            }],
            truncated: false,
            digest: 0,
            fold: None,
        }
    }

    fn diff(files: Vec<FileDiff>) -> TaskDiff {
        TaskDiff {
            task: TaskId(1),
            source: ReviewSource::Merged,
            fingerprint: 7,
            head: "abc".to_owned(),
            stat: DiffStat::default(),
            files,
        }
    }

    fn rows_of(diff: &TaskDiff, style: DiffStyle) -> Vec<Row> {
        let infos = file_infos(diff);
        let shown: Vec<usize> = (0..diff.files.len()).collect();

        build_rows(
            diff,
            &infos,
            &shown,
            (|_| false, |_| false),
            style,
            &Marks::default(),
        )
        .rows
    }

    #[test]
    fn the_agent_filter_keeps_only_that_agents_files() {
        let diff = diff(vec![
            file("a.rs", Some(1), Vec::new()),
            file("b.rs", Some(2), Vec::new()),
            file("c.rs", None, Vec::new()),
        ]);

        assert_eq!(visible_files(&diff, Some(DaemonAgentId(2))), [1]);
        assert_eq!(visible_files(&diff, None), [0, 1, 2]);
    }

    #[test]
    fn unified_rows_number_each_side() {
        let diff = diff(vec![file(
            "a.rs",
            None,
            vec![
                line(DiffLineKind::Context, "one"),
                line(DiffLineKind::Deleted, "two"),
                line(DiffLineKind::Added, "2"),
                line(DiffLineKind::Added, "2b"),
                line(DiffLineKind::Context, "three"),
            ],
        )]);

        let numbers: Vec<_> = rows_of(&diff, DiffStyle::Unified)
            .into_iter()
            .filter_map(|row| match row {
                Row::Line { old, new, .. } => Some((old, new)),
                _ => None,
            })
            .collect();

        assert_eq!(numbers, [(10, 20), (11, 0), (0, 21), (0, 22), (12, 23)]);
    }

    #[test]
    fn split_rows_pair_deletions_with_additions_of_one_block() {
        let diff = diff(vec![file(
            "a.rs",
            None,
            vec![
                line(DiffLineKind::Context, "one"),
                line(DiffLineKind::Deleted, "d1"),
                line(DiffLineKind::Deleted, "d2"),
                line(DiffLineKind::Deleted, "d3"),
                line(DiffLineKind::Added, "a1"),
                line(DiffLineKind::Context, "two"),
                line(DiffLineKind::Added, "a2"),
            ],
        )]);

        let pairs: Vec<_> = rows_of(&diff, DiffStyle::Split)
            .into_iter()
            .filter_map(|row| match row {
                Row::Pair { left, right, .. } => Some((
                    left.map(|side| (side.line, side.number)),
                    right.map(|side| (side.line, side.number)),
                )),
                _ => None,
            })
            .collect();

        assert_eq!(
            pairs,
            [
                (Some((0, 10)), Some((0, 20))),
                (Some((1, 11)), Some((4, 21))),
                (Some((2, 12)), None),
                (Some((3, 13)), None),
                (Some((5, 14)), Some((5, 22))),
                (None, Some((6, 23))),
            ]
        );
    }

    #[test]
    fn a_collapsed_file_keeps_only_its_header() {
        let diff = diff(vec![
            file("a.rs", None, vec![line(DiffLineKind::Added, "x")]),
            file("b.rs", None, vec![line(DiffLineKind::Added, "y")]),
        ]);
        let infos = file_infos(&diff);

        let body = build_rows(
            &diff,
            &infos,
            &[0, 1],
            (|index| index == 0, |_| false),
            DiffStyle::Unified,
            &Marks::default(),
        );

        assert_eq!(body.headers, [Some(0), Some(1)]);
        assert_eq!(body.range_of(0), Some(0..1));
        assert_eq!(body.range_of(1), Some(1..body.rows.len() - 1));
        assert_eq!(body.rows.last(), Some(&Row::End));
    }

    #[test]
    fn a_binary_file_shows_a_note_and_a_truncated_file_counts_what_is_missing() {
        let mut binary = file("logo.png", None, Vec::new());
        binary.binary = true;
        binary.hunks.clear();
        let mut long = file("big.rs", None, vec![line(DiffLineKind::Added, "x")]);
        long.additions = 1_205;
        long.truncated = true;
        let diff = diff(vec![binary, long]);

        let rows = rows_of(&diff, DiffStyle::Unified);

        assert!(rows.contains(&Row::Note {
            file: 0,
            note: Note::Binary
        }));
        assert!(rows.contains(&Row::Note {
            file: 1,
            note: Note::Truncated(1_204)
        }));
    }

    #[test]
    fn a_no_newline_line_has_no_number_and_does_not_shift_the_next_one() {
        let diff = diff(vec![file(
            "a.rs",
            None,
            vec![
                line(DiffLineKind::Deleted, "old"),
                line(DiffLineKind::NoNewline, ""),
                line(DiffLineKind::Added, "new"),
                line(DiffLineKind::NoNewline, ""),
            ],
        )]);

        let unified: Vec<_> = rows_of(&diff, DiffStyle::Unified)
            .into_iter()
            .filter_map(|row| match row {
                Row::Line { old, new, .. } => Some((old, new)),
                _ => None,
            })
            .collect();
        let split: Vec<_> = rows_of(&diff, DiffStyle::Split)
            .into_iter()
            .filter_map(|row| match row {
                Row::Pair { left, right, .. } => Some((
                    left.map(|side| (side.line, side.number)),
                    right.map(|side| (side.line, side.number)),
                )),
                _ => None,
            })
            .collect();

        assert_eq!(unified, [(10, 0), (0, 0), (0, 20), (0, 0)]);
        assert_eq!(
            split,
            [(Some((0, 10)), Some((2, 20))), (Some((1, 0)), Some((3, 0))),]
        );
    }

    #[test]
    fn a_no_newline_line_after_context_marks_both_sides() {
        let diff = diff(vec![file(
            "a.rs",
            None,
            vec![
                line(DiffLineKind::Context, "same"),
                line(DiffLineKind::NoNewline, ""),
            ],
        )]);

        let pairs: Vec<_> = rows_of(&diff, DiffStyle::Split)
            .into_iter()
            .filter_map(|row| match row {
                Row::Pair { left, right, .. } => Some((left?.number, right?.number)),
                _ => None,
            })
            .collect();

        assert_eq!(pairs, [(10, 20), (0, 0)]);
    }

    #[test]
    fn a_file_left_out_whole_shows_one_note() {
        let mut big = file("big.bin", None, Vec::new());
        big.hunks.clear();
        big.truncated = true;

        let rows = rows_of(&diff(vec![big]), DiffStyle::Unified);

        assert!(rows.contains(&Row::Note {
            file: 0,
            note: Note::TooLarge
        }));
        assert!(!rows.iter().any(|row| matches!(
            row,
            Row::Note {
                note: Note::Truncated(_),
                ..
            }
        )));
    }

    #[test]
    fn a_mode_change_changes_the_sample_digest() {
        let plain = file("a.rs", None, vec![line(DiffLineKind::Added, "x")]);
        let mut moded = plain.clone();
        moded.mode_change = Some((0o100_644, 0o100_755));

        assert_ne!(sample_digest(&plain), sample_digest(&moded));
    }

    #[test]
    fn an_empty_file_shows_a_note() {
        let mut empty = file("empty.txt", None, Vec::new());
        empty.hunks.clear();

        let rows = rows_of(&diff(vec![empty]), DiffStyle::Unified);

        assert!(rows.contains(&Row::Note {
            file: 0,
            note: Note::Empty
        }));
    }

    #[test]
    fn the_sample_digest_changes_with_the_hunks_and_the_path() {
        let one = file("a.rs", None, vec![line(DiffLineKind::Added, "x")]);
        let same = one.clone();
        let other = file("a.rs", None, vec![line(DiffLineKind::Added, "y")]);
        let moved = file("b.rs", None, vec![line(DiffLineKind::Added, "x")]);

        assert_eq!(sample_digest(&one), sample_digest(&same));
        assert_ne!(sample_digest(&one), sample_digest(&other));
        assert_ne!(sample_digest(&one), sample_digest(&moved));
    }

    #[test]
    fn thousands_get_a_comma() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1_204), "1,204");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn the_sample_diff_adds_up() {
        let agents = [DaemonAgentId(1), DaemonAgentId(2), DaemonAgentId(3)];

        let live = sample_diff(TaskId(0), ReviewSource::Live, agents);
        let merged = sample_diff(TaskId(0), ReviewSource::Merged, agents);

        assert_eq!(
            (live.stat.files, live.stat.additions, live.stat.deletions),
            (11, 2_623, 394)
        );
        assert_eq!(worktree_count(&live), 3);
        assert_eq!(worktree_count(&merged), 1);

        for file in &merged.files {
            let file = if file.fold.is_some() {
                assert!(file.hunks.is_empty());

                sample_lines(file)
            } else {
                file.clone()
            };
            let counted = |kind| {
                file.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .filter(|line| line.kind == kind)
                    .count()
            };

            assert_eq!(counted(DiffLineKind::Added), file.additions as usize);
            assert_eq!(counted(DiffLineKind::Deleted), file.deletions as usize);
        }
    }

    #[test]
    fn the_sample_diff_folds_a_lock_file_and_a_large_file_with_distinct_digests() {
        let agents = [DaemonAgentId(1), DaemonAgentId(2), DaemonAgentId(3)];

        for source in [ReviewSource::Live, ReviewSource::Merged] {
            let diff = sample_diff(TaskId(0), source, agents);
            let folds: Vec<_> = diff
                .files
                .iter()
                .filter_map(|file| file.fold.map(|fold| (file.path.clone(), fold)))
                .collect();
            let mut digests: Vec<u64> = diff.files.iter().map(|file| file.digest).collect();
            digests.sort_unstable();
            digests.dedup();

            assert_eq!(
                folds,
                [
                    (PathBuf::from("Cargo.lock"), Fold::Lock),
                    (
                        PathBuf::from("crates/swarm-term/src/glyphs.rs"),
                        Fold::Large
                    ),
                ]
            );
            assert_eq!(
                digests.len(),
                diff.files.len(),
                "a digest repeats in {source:?}"
            );
            assert!(diff.files.iter().all(|file| file.digest != 0));
            assert_eq!(
                diff,
                sample_diff(TaskId(0), source, agents),
                "the digests are not stable"
            );
        }
    }

    fn folded(path: &str, fold: Fold) -> FileDiff {
        let mut folded = file(path, None, Vec::new());

        folded.hunks.clear();
        folded.additions = 90;
        folded.deletions = 10;
        folded.fold = Some(fold);
        folded.digest = 5;

        folded
    }

    fn loaded(key: &FileDiff) -> FileLoads {
        FileLoads::from([((key.worktree.clone(), key.path.clone()), FileLoad::Loaded)])
    }

    #[test]
    fn a_folded_file_builds_exactly_one_row() {
        let diff = diff(vec![folded("Cargo.lock", Fold::Lock)]);
        let infos = file_infos(&diff);
        let comments = [comment(1, DiffSide::New, 21, CommentState::Sent)];
        let loads = FileLoads::new();
        let marks = Marks {
            threads: place_threads(&diff, &comments, &loads).into(),
            composer: Some(ComposerAt::Top { file: 0 }),
        };

        let body = build_rows(
            &diff,
            &infos,
            &[0],
            (|_| false, |index| is_folded(&diff.files[index], &loads)),
            DiffStyle::Unified,
            &marks,
        );

        assert_eq!(body.rows, [Row::Header { file: 0 }, Row::End]);
        assert_eq!(body.range_of(0), Some(0..1));
    }

    #[test]
    fn a_loaded_file_builds_its_rows_and_keeps_its_fold() {
        let mut file = folded("Cargo.lock", Fold::Lock);
        file.hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            section: String::new(),
            lines: vec![line(DiffLineKind::Added, "x")],
        }];
        let diff = diff(vec![file]);
        let infos = file_infos(&diff);
        let loads = loaded(&diff.files[0]);

        let body = build_rows(
            &diff,
            &infos,
            &[0],
            (|_| false, |index| is_folded(&diff.files[index], &loads)),
            DiffStyle::Unified,
            &Marks::default(),
        );

        assert!(body.rows.len() > 2);
        assert_eq!(diff.files[0].fold, Some(Fold::Lock));
        assert!(!is_folded(&diff.files[0], &loads));
        assert!(is_folded(&diff.files[0], &FileLoads::new()));
    }

    #[test]
    fn threads_on_a_folded_file_wait_until_its_lines_load() {
        let mut diff = diff(vec![folded("a.rs", Fold::Generated)]);
        let comments = [comment(1, DiffSide::New, 21, CommentState::Sent)];

        let waiting = place_threads(&diff, &comments, &FileLoads::new());
        let lines = commented_diff().files.remove(0);
        diff.files[0].hunks.clone_from(&lines.hunks);
        let placed = place_threads(&diff, &comments, &loaded(&diff.files[0]));

        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].place, Place::Folded);
        assert!(matches!(placed[0].place, Place::Line { .. }));
    }

    #[test]
    fn an_outdated_thread_on_a_folded_file_is_not_placed_at_the_top() {
        let diff = diff(vec![folded("a.rs", Fold::Generated)]);
        let mut outdated = comment(1, DiffSide::New, 21, CommentState::Sent);
        outdated.outdated = true;

        let threads = place_threads(&diff, &[outdated], &FileLoads::new());

        assert_eq!(threads[0].place, Place::Folded);
    }

    fn key(path: &str) -> LoadKey {
        (
            TaskId(1),
            ReviewSource::Merged,
            PathBuf::from("/wt"),
            PathBuf::from(path),
        )
    }

    fn lines_for(path: &str, digest: u64, text: &str) -> FileDiff {
        let mut file = file(path, None, vec![line(DiffLineKind::Added, text)]);

        file.digest = digest;
        file.fold = Some(Fold::Lock);

        file
    }

    fn folded_diff(path: &str, digest: u64) -> TaskDiff {
        let mut folded = folded(path, Fold::Lock);

        folded.worktree = PathBuf::from("/wt");
        folded.digest = digest;

        diff(vec![folded])
    }

    fn file_key() -> FileKey {
        (PathBuf::from("/wt"), PathBuf::from("Cargo.lock"))
    }

    fn stored(path: &str, digest: u64) -> Rc<TaskDiff> {
        Rc::new(folded_diff(path, digest))
    }

    fn texts(diff: &TaskDiff) -> Vec<&str> {
        diff.files[0]
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter().map(|line| line.text.as_str()))
            .collect()
    }

    /// A load of `Cargo.lock` that the daemon answered with `answer`.
    fn settle(
        loaded: &mut LoadedFiles,
        stored: &mut Rc<TaskDiff>,
        answer: Result<Option<FileDiff>, SharedString>,
    ) -> AfterLoad {
        loaded.finish(&key("Cargo.lock"), answer, Some(stored))
    }

    /// A new diff of the daemon arrives with another digest for the file.
    fn poll(loaded: &mut LoadedFiles, stored: &mut Rc<TaskDiff>, digest: u64) {
        let mut next = folded_diff("Cargo.lock", digest);

        loaded.restore(&mut next);
        *stored = Rc::new(next);
    }

    #[test]
    fn loaded_lines_come_back_into_a_new_diff_with_the_same_digest() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        assert!(loaded.begin(&key, 5));
        let next = settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 5, "kept"))),
        );

        let mut again = folded_diff("Cargo.lock", 5);
        loaded.restore(&mut again);

        assert_eq!(next, AfterLoad::Nothing);
        assert_eq!(texts(&kept), ["kept"]);
        assert_eq!(texts(&again), ["kept"]);
        assert_eq!(again.files[0].fold, Some(Fold::Lock));
        assert!(loaded.begin_stale(&again).is_empty());
        assert_eq!(
            loaded.loads(&again).get(&file_key()),
            Some(&FileLoad::Loaded)
        );
    }

    #[test]
    fn a_new_digest_never_shows_the_old_lines_and_asks_for_new_ones() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        loaded.begin(&key, 5);
        settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 5, "old"))),
        );

        poll(&mut loaded, &mut kept, 6);
        assert!(
            texts(&kept).is_empty(),
            "the old lines went into the new file"
        );
        assert_eq!(loaded.loads(&kept).get(&file_key()), None);

        assert_eq!(loaded.begin_stale(&kept), std::slice::from_ref(&key));
        assert!(
            loaded.begin_stale(&kept).is_empty(),
            "it is loading already"
        );
        assert!(!loaded.begin(&key, 6), "a second load of the same file");
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Loading)
        );
    }

    #[test]
    fn a_failed_reload_shows_its_error_and_never_the_old_lines() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        loaded.begin(&key, 5);
        settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 5, "old"))),
        );
        poll(&mut loaded, &mut kept, 6);
        loaded.begin(&key, 6);

        let next = settle(&mut loaded, &mut kept, Err("no daemon".into()));

        assert_eq!(next, AfterLoad::Nothing);
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Failed("no daemon".into()))
        );
        assert!(texts(&kept).is_empty());
        assert!(
            loaded.begin_stale(&kept).is_empty(),
            "no new try for the same digest"
        );
        poll(&mut loaded, &mut kept, 7);
        assert_eq!(
            loaded.begin_stale(&kept),
            [key],
            "a new digest is a new try"
        );
    }

    #[test]
    fn a_failed_load_stays_failed_until_the_next_try() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        loaded.begin(&key, 5);
        settle(&mut loaded, &mut kept, Err("no daemon".into()));
        let failed = loaded.loads(&kept);

        loaded.begin(&key, 5);
        let retrying = loaded.loads(&kept);

        assert_eq!(
            failed.get(&file_key()),
            Some(&FileLoad::Failed("no daemon".into()))
        );
        assert_eq!(retrying.get(&file_key()), Some(&FileLoad::Loading));
    }

    #[test]
    fn a_file_the_diff_no_longer_folds_is_forgotten() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        loaded.begin(&key, 5);
        settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 5, "kept"))),
        );

        let mut gone = diff(Vec::new());
        loaded.restore(&mut gone);

        assert!(loaded.loads(&folded_diff("Cargo.lock", 5)).is_empty());
    }

    #[test]
    fn an_answer_for_an_older_digest_is_dropped_and_the_file_loads_again() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 1);
        loaded.begin(&key, 1);
        poll(&mut loaded, &mut kept, 2);

        let late = settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 1, "old"))),
        );

        assert_eq!(late, AfterLoad::LoadAgain);
        assert!(
            texts(&kept).is_empty(),
            "the old answer landed in the new card"
        );
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Loading)
        );
        assert!(!loaded.begin(&key, 2), "the new load is running already");

        let fresh = settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 2, "new"))),
        );

        assert_eq!(fresh, AfterLoad::Nothing);
        assert_eq!(texts(&kept), ["new"]);
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Loaded)
        );
    }

    #[test]
    fn an_answer_for_a_file_that_is_not_folded_any_more_is_dropped() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut plain = folded_diff("Cargo.lock", 1);
        plain.files[0].fold = None;
        let mut kept = Rc::new(plain);
        loaded.begin(&key, 1);

        let next = settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 1, "old"))),
        );

        assert_eq!(next, AfterLoad::Nothing);
        assert!(texts(&kept).is_empty());
        assert!(loaded.loads(&stored("Cargo.lock", 1)).is_empty());
    }

    #[test]
    fn a_file_the_daemon_does_not_have_fails_the_load_and_leaves_the_diff_alone() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        let before = (*kept).clone();
        loaded.begin(&key, 5);

        let next = settle(&mut loaded, &mut kept, Ok(None));

        assert_eq!(next, AfterLoad::FreshDiff);
        assert_eq!(*kept, before, "the stored diff was edited");
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Failed("No longer in the diff".into()))
        );
    }

    #[test]
    fn an_answer_with_another_digest_than_the_asked_one_asks_for_a_fresh_diff() {
        let mut loaded = LoadedFiles::default();
        let key = key("Cargo.lock");
        let mut kept = stored("Cargo.lock", 5);
        loaded.begin(&key, 5);

        let next = settle(
            &mut loaded,
            &mut kept,
            Ok(Some(lines_for("Cargo.lock", 7, "newer"))),
        );

        assert_eq!(next, AfterLoad::FreshDiff);
        assert!(texts(&kept).is_empty());
        assert_eq!(
            loaded.loads(&kept).get(&file_key()),
            Some(&FileLoad::Failed("Changed while loading".into()))
        );
    }

    #[test]
    fn a_load_that_ends_after_its_task_closed_changes_nothing() {
        let answers: [Result<Option<FileDiff>, SharedString>; 3] = [
            Ok(Some(lines_for("Cargo.lock", 5, "late"))),
            Ok(None),
            Err("late".into()),
        ];

        for answer in answers {
            let mut loaded = LoadedFiles::default();
            let key = key("Cargo.lock");
            let mut kept = stored("Cargo.lock", 5);
            loaded.begin(&key, 5);
            loaded.forget_task(TaskId(1));

            let next = settle(&mut loaded, &mut kept, answer);

            assert_eq!(next, AfterLoad::Nothing);
            assert!(
                loaded.loads(&kept).is_empty(),
                "an entry came back for a closed task"
            );
            assert!(loaded.begin_stale(&kept).is_empty());
        }
    }

    fn comment(id: u64, side: DiffSide, line: u32, state: CommentState) -> ReviewComment {
        ReviewComment {
            id: CommentId(id),
            task: TaskId(1),
            parent: None,
            source: ReviewSource::Merged,
            anchor: CommentAnchor {
                worktree: PathBuf::from("/wt"),
                path: PathBuf::from("a.rs"),
                side,
                line,
                text: String::new(),
                before: None,
                after: None,
            },
            body: format!("comment {id}"),
            state,
            to: None,
            outdated: false,
            created_at: 1,
            sent_at: None,
            delivered_at: None,
        }
    }

    /// Lines: ctx a (10, 20), del b (11), add B (21), ctx c (12, 22).
    fn commented_diff() -> TaskDiff {
        diff(vec![file(
            "a.rs",
            None,
            vec![
                line(DiffLineKind::Context, "a"),
                line(DiffLineKind::Deleted, "b"),
                line(DiffLineKind::Added, "B"),
                line(DiffLineKind::Context, "c"),
            ],
        )])
    }

    fn rows_with(diff: &TaskDiff, comments: &[ReviewComment], style: DiffStyle) -> Vec<Row> {
        let infos = file_infos(diff);
        let marks = Marks {
            threads: place_threads(diff, comments, &FileLoads::new()).into(),
            composer: None,
        };

        build_rows(diff, &infos, &[0], (|_| false, |_| false), style, &marks).rows
    }

    fn thread_rows(rows: &[Row]) -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, Row::Thread { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn an_anchor_holds_the_line_and_its_neighbors_on_its_side() {
        let diff = commented_diff();
        let file = &diff.files[0];

        let added = anchor_for(file, 0, 2, DiffSide::New);
        let deleted = anchor_for(file, 0, 1, DiffSide::Old);

        let added = added.expect("the added line takes a new-side anchor");
        assert_eq!((added.line, added.text.as_str()), (21, "B"));
        assert_eq!(added.before.as_deref(), Some("a"));
        assert_eq!(added.after.as_deref(), Some("c"));
        assert_eq!(added.worktree, PathBuf::from("/wt"));
        assert_eq!(added.path, PathBuf::from("a.rs"));

        let deleted = deleted.expect("the deleted line takes an old-side anchor");
        assert_eq!((deleted.line, deleted.text.as_str()), (11, "b"));
        assert_eq!(deleted.before.as_deref(), Some("a"));
        assert_eq!(deleted.after.as_deref(), Some("c"));
    }

    #[test]
    fn an_anchor_has_no_neighbor_past_the_edge_of_its_hunk() {
        let diff = commented_diff();
        let file = &diff.files[0];

        let first = anchor_for(file, 0, 0, DiffSide::New).expect("context line");
        let last = anchor_for(file, 0, 3, DiffSide::Old).expect("context line, old side");

        assert_eq!(
            (first.line, first.before, first.after.as_deref()),
            (20, None, Some("B"))
        );
        assert_eq!(
            (last.line, last.before.as_deref(), last.after),
            (12, Some("b"), None)
        );
    }

    #[test]
    fn a_line_has_no_anchor_on_a_side_it_is_not_on() {
        let mut diff = commented_diff();
        diff.files[0].hunks[0]
            .lines
            .push(line(DiffLineKind::NoNewline, ""));
        let file = &diff.files[0];

        assert!(anchor_for(file, 0, 2, DiffSide::Old).is_none());
        assert!(anchor_for(file, 0, 1, DiffSide::New).is_none());
        assert!(anchor_for(file, 0, 4, DiffSide::New).is_none());
        assert!(anchor_for(file, 3, 0, DiffSide::New).is_none());
    }

    #[test]
    fn a_comment_sits_under_its_line_in_unified() {
        let diff = commented_diff();
        let comments = [comment(1, DiffSide::New, 21, CommentState::Pending)];

        let rows = rows_with(&diff, &comments, DiffStyle::Unified);

        // Header, hunk, a, b, B, thread, c, end.
        assert_eq!(thread_rows(&rows), [5]);
        assert!(matches!(rows[4], Row::Line { line: 2, .. }));
    }

    #[test]
    fn an_old_side_comment_sits_under_the_row_with_that_old_number() {
        let diff = commented_diff();
        let comments = [comment(1, DiffSide::Old, 11, CommentState::Pending)];

        let rows = rows_with(&diff, &comments, DiffStyle::Unified);

        assert_eq!(thread_rows(&rows), [4]);
        assert!(matches!(rows[3], Row::Line { line: 1, .. }));
    }

    #[test]
    fn a_comment_sits_under_its_pair_in_split() {
        let diff = commented_diff();
        let comments = [
            comment(1, DiffSide::New, 21, CommentState::Pending),
            comment(2, DiffSide::Old, 12, CommentState::Pending),
        ];

        let rows = rows_with(&diff, &comments, DiffStyle::Split);

        // Header, hunk, a|a, b|B, thread (new 21), c|c, thread (old 12), end.
        assert_eq!(thread_rows(&rows), [4, 6]);
        assert!(matches!(rows[3], Row::Pair { .. }));
        assert!(matches!(rows[5], Row::Pair { .. }));
    }

    #[test]
    fn an_outdated_comment_and_one_outside_every_hunk_go_to_the_top_of_the_file() {
        let diff = commented_diff();
        let mut outdated = comment(1, DiffSide::New, 21, CommentState::Sent);
        outdated.outdated = true;
        let outside = comment(2, DiffSide::New, 900, CommentState::Pending);

        let comments = [outdated, outside];

        let threads = place_threads(&diff, &comments, &FileLoads::new());
        let rows = rows_with(&diff, &comments, DiffStyle::Unified);

        assert!(threads.iter().all(|thread| thread.place == Place::Top));
        assert_eq!(thread_rows(&rows), [1, 2]);
        assert!(matches!(rows[0], Row::Header { .. }));
        assert!(matches!(rows[3], Row::Hunk { .. }));
    }

    #[test]
    fn comments_of_another_source_or_file_and_replies_are_not_placed() {
        let diff = commented_diff();
        let mut live = comment(1, DiffSide::New, 21, CommentState::Sent);
        live.source = ReviewSource::Live;
        let mut elsewhere = comment(2, DiffSide::New, 21, CommentState::Sent);
        elsewhere.anchor.path = PathBuf::from("other.rs");
        let mut reply = comment(3, DiffSide::New, 21, CommentState::Sent);
        reply.parent = Some(CommentId(9));

        let threads = place_threads(&diff, &[live, elsewhere, reply], &FileLoads::new());

        assert!(threads.is_empty());
    }

    #[test]
    fn the_comment_box_sits_under_the_threads_of_its_line() {
        let diff = commented_diff();
        let infos = file_infos(&diff);
        let comments = [comment(1, DiffSide::New, 21, CommentState::Pending)];
        let marks = Marks {
            threads: place_threads(&diff, &comments, &FileLoads::new()).into(),
            composer: Some(ComposerAt::Line(LinePos {
                file: 0,
                hunk: 0,
                line: 2,
                side: DiffSide::New,
            })),
        };

        let rows = build_rows(
            &diff,
            &infos,
            &[0],
            (|_| false, |_| false),
            DiffStyle::Unified,
            &marks,
        )
        .rows;

        assert!(matches!(rows[5], Row::Thread { .. }));
        assert!(matches!(rows[6], Row::Composer { .. }));
    }

    #[test]
    fn a_comment_box_without_a_line_sits_at_the_top_of_its_card() {
        let diff = commented_diff();
        let infos = file_infos(&diff);
        let mut outdated = comment(1, DiffSide::New, 21, CommentState::Pending);
        outdated.outdated = true;
        let marks = Marks {
            threads: place_threads(&diff, &[outdated], &FileLoads::new()).into(),
            composer: Some(ComposerAt::Top { file: 0 }),
        };

        let rows = build_rows(
            &diff,
            &infos,
            &[0],
            (|_| false, |_| false),
            DiffStyle::Unified,
            &marks,
        )
        .rows;

        assert!(matches!(rows[1], Row::Thread { .. }));
        assert!(matches!(rows[2], Row::Composer { .. }));
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Composer { .. }))
                .count(),
            1
        );
    }

    fn anchored(side: DiffSide, line: u32, text: &str, around: (&str, &str)) -> CommentAnchor {
        CommentAnchor {
            worktree: PathBuf::from("/wt"),
            path: PathBuf::from("a.rs"),
            side,
            line,
            text: text.to_owned(),
            before: Some(around.0)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            after: Some(around.1)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
        }
    }

    /// A file with one hunk whose lines start at number 1 on both sides.
    fn numbered(lines: Vec<DiffLine>) -> FileDiff {
        let mut data = file("a.rs", None, lines);

        data.hunks[0].old_start = 1;
        data.hunks[0].new_start = 1;

        data
    }

    fn context(texts: &[&str]) -> FileDiff {
        numbered(
            texts
                .iter()
                .map(|text| line(DiffLineKind::Context, text))
                .collect(),
        )
    }

    #[test]
    fn a_line_moved_by_new_lines_above_is_found_again() {
        let data = context(&["new", "new", "a", "b", "c"]);
        let anchor = anchored(DiffSide::New, 2, "b", ("a", "c"));

        assert_eq!(refind_line(&data, &anchor), Some(4));
    }

    #[test]
    fn equal_neighbors_beat_distance_and_ties_go_to_the_earlier_line() {
        let data = context(&["p", "x", "q", "a", "x", "c", "x"]);
        let with_neighbors = anchored(DiffSide::New, 2, "x", ("a", "c"));
        let nearest = anchored(DiffSide::New, 7, "x", ("", ""));

        let tie = context(&["x", "a", "b", "c", "x"]);
        let halfway = anchored(DiffSide::New, 3, "x", ("", ""));

        assert_eq!(refind_line(&data, &with_neighbors), Some(5));
        assert_eq!(refind_line(&data, &nearest), Some(7));
        assert_eq!(refind_line(&tie, &halfway), Some(1));
    }

    #[test]
    fn a_line_that_is_gone_or_on_the_other_side_is_not_found() {
        let data = numbered(vec![
            line(DiffLineKind::Context, "a"),
            line(DiffLineKind::Deleted, "old"),
            line(DiffLineKind::Added, "new"),
        ]);

        assert_eq!(
            refind_line(&data, &anchored(DiffSide::New, 1, "gone", ("", ""))),
            None
        );
        assert_eq!(
            refind_line(&data, &anchored(DiffSide::New, 2, "old", ("", ""))),
            None,
            "a deleted line is not on the new side"
        );
        assert_eq!(
            refind_line(&data, &anchored(DiffSide::Old, 2, "old", ("a", ""))),
            Some(2)
        );
    }

    #[test]
    fn comment_counts_read_as_a_status() {
        let pending = comment(1, DiffSide::New, 1, CommentState::Pending);
        let sent = comment(2, DiffSide::New, 1, CommentState::Sent);
        let resolved = comment(3, DiffSide::New, 1, CommentState::Resolved);
        let text = |list: &[ReviewComment]| count_comments(list).status_text();

        assert_eq!(text(&[]), None);
        assert_eq!(
            text(&[pending.clone(), pending.clone()]).as_deref(),
            Some("2 pending comments")
        );
        assert_eq!(
            text(std::slice::from_ref(&pending)).as_deref(),
            Some("1 pending comment")
        );
        assert_eq!(
            text(&[sent.clone(), sent.clone(), sent.clone(), pending]).as_deref(),
            Some("3 sent \u{b7} 1 pending")
        );
        assert_eq!(
            text(&[sent, resolved]).as_deref(),
            Some("1 sent \u{b7} 1 resolved")
        );
    }

    #[test]
    fn events_add_change_and_delete_comments() {
        let mut list = vec![comment(1, DiffSide::New, 1, CommentState::Pending)];
        let mut reply = comment(2, DiffSide::New, 1, CommentState::Pending);
        reply.parent = Some(CommentId(1));
        let mut sent = list[0].clone();
        sent.state = CommentState::Sent;

        apply_comment_event(&mut list, &CommentEvent::Changed(Box::new(reply)));
        apply_comment_event(&mut list, &CommentEvent::Changed(Box::new(sent)));
        let changed: Vec<_> = list.iter().map(|c| (c.id.0, c.state)).collect();
        apply_comment_event(
            &mut list,
            &CommentEvent::Deleted {
                task: TaskId(1),
                id: CommentId(1),
            },
        );

        assert_eq!(
            changed,
            [(1, CommentState::Sent), (2, CommentState::Pending)]
        );
        assert!(list.is_empty(), "a delete takes the replies with it");
    }

    #[test]
    fn the_sample_daemon_follows_the_rules_of_the_real_one() {
        let anchor = comment(1, DiffSide::New, 21, CommentState::Pending).anchor;
        let new = |parent, source| NewComment {
            task: TaskId(1),
            parent,
            source,
            anchor: anchor.clone(),
            body: "x".into(),
        };
        let mut list = Vec::new();

        fake_add(&mut list, new(None, ReviewSource::Merged), None, 5);
        fake_add(
            &mut list,
            new(Some(CommentId(1)), ReviewSource::Merged),
            None,
            6,
        );
        fake_add(
            &mut list,
            new(None, ReviewSource::Live),
            Some(DaemonAgentId(4)),
            7,
        );
        let added: Vec<_> = list.iter().map(|c| (c.state, c.to)).collect();
        fake_delete(&mut list, CommentId(3));
        let after_refused = list.len();
        let sent = fake_send(&mut list, |_| Some(DaemonAgentId(9)), 8);
        let again = fake_send(&mut list, |_| None, 9);
        fake_resolve(&mut list, CommentId(1), true);

        assert_eq!(
            added,
            [
                (CommentState::Pending, None),
                (CommentState::Pending, None),
                (CommentState::Sent, Some(DaemonAgentId(4))),
            ]
        );
        assert_eq!(after_refused, 3, "only a pending comment can be deleted");
        assert!(sent && !again);
        assert_eq!(list[0].state, CommentState::Resolved);
        assert_eq!(list[1].to, Some(DaemonAgentId(9)));
    }

    #[test]
    fn times_read_as_short_ago_words() {
        assert_eq!(ago(10_000, 12_000), "now");
        assert_eq!(ago(0, 40_000), "40s");
        assert_eq!(ago(0, 6 * 60_000), "6m");
        assert_eq!(ago(0, 2 * 3_600_000), "2h");
        assert_eq!(ago(0, 3 * 86_400_000), "3d");
    }

    #[test]
    fn the_sample_comments_anchor_to_real_lines() {
        let agents = [DaemonAgentId(1), DaemonAgentId(2), DaemonAgentId(3)];
        let live = sample_diff(TaskId(0), ReviewSource::Live, agents);
        let merged = sample_diff(TaskId(0), ReviewSource::Merged, agents);

        let comments = sample_comments(&live, &merged, agents, 1_000_000);

        for diff in [&live, &merged] {
            let placed = place_threads(diff, &comments, &FileLoads::new());
            let tops = placed.iter().filter(|t| t.place == Place::Top).count();

            assert!(!placed.is_empty());
            assert!(tops <= 1, "only the outdated sample is not on a line");
        }
        assert_eq!(
            comments
                .iter()
                .filter(|c| c.state == CommentState::Pending)
                .count(),
            2
        );
    }
}
