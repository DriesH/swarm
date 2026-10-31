//! Review: the data behind the diff view. Plain data, no gpui entities.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use gpui::SharedString;
use swarm_protocol::{
    AgentId as DaemonAgentId, DiffHunk, DiffLine, DiffLineKind, DiffStat, FileChange, FileDiff,
    ReviewSource, TaskDiff, TaskId,
};

use crate::model::AgentKind;

/// The worktree and the path inside it: what tells two changed files apart.
pub(crate) type FileKey = (PathBuf, PathBuf);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DiffStyle {
    #[default]
    Unified,
    Split,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Grouping {
    #[default]
    ByAgent,
    ByFolder,
}

/// An agent of the open task, as the file lists name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentLabel {
    pub id: DaemonAgentId,
    pub name: SharedString,
    pub kind: AgentKind,
}

pub(crate) const OTHER_CHANGES: &str = "Other changes";
pub(crate) const ROOT_FOLDER: &str = "Repository root";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupKind {
    Agent(AgentKind),
    Folder,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileGroup {
    pub title: SharedString,
    pub kind: GroupKind,
    /// Indexes into [`TaskDiff::files`], in diff order.
    pub files: Vec<usize>,
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

/// Groups the files by the agent that touched them, or by their folder. Files no known
/// agent touched go in "Other changes", last.
pub(crate) fn group_files(
    files: &[FileDiff],
    visible: &[usize],
    grouping: Grouping,
    agents: &[AgentLabel],
) -> Vec<FileGroup> {
    match grouping {
        Grouping::ByAgent => group_by_agent(files, visible, agents),
        Grouping::ByFolder => group_by_folder(files, visible),
    }
}

fn group_by_agent(files: &[FileDiff], visible: &[usize], agents: &[AgentLabel]) -> Vec<FileGroup> {
    let mut groups: Vec<FileGroup> = Vec::new();
    let mut other = Vec::new();

    for agent in agents {
        let owned: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|index| files.get(*index).is_some_and(|f| f.agent == Some(agent.id)))
            .collect();

        if !owned.is_empty() {
            groups.push(FileGroup {
                title: agent.name.clone(),
                kind: GroupKind::Agent(agent.kind),
                files: owned,
            });
        }
    }

    for index in visible {
        let known = files
            .get(*index)
            .and_then(|file| file.agent)
            .is_some_and(|id| agents.iter().any(|agent| agent.id == id));

        if !known {
            other.push(*index);
        }
    }

    if !other.is_empty() {
        groups.push(FileGroup {
            title: OTHER_CHANGES.into(),
            kind: GroupKind::Other,
            files: other,
        });
    }

    groups
}

fn group_by_folder(files: &[FileDiff], visible: &[usize]) -> Vec<FileGroup> {
    let mut folders: BTreeMap<String, Vec<usize>> = BTreeMap::new();

    for index in visible {
        let Some(file) = files.get(*index) else {
            continue;
        };

        folders
            .entry(folder_of(&file.path))
            .or_default()
            .push(*index);
    }

    folders
        .into_iter()
        .map(|(folder, files)| FileGroup {
            title: if folder.is_empty() {
                ROOT_FOLDER.into()
            } else {
                folder.into()
            },
            kind: GroupKind::Folder,
            files,
        })
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
    /// Changes when any hunk of the file changes, see [`file_hash`].
    pub hash: u64,
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
        hash: file_hash(file),
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

/// A hash of the file's hunks. A viewed file stops being viewed when it changes.
pub(crate) fn file_hash(file: &FileDiff) -> u64 {
    let mut hasher = DefaultHasher::new();

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
            | Self::Note { file, .. } => file,
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

/// Flattens the shown files into rows. A collapsed file keeps only its header.
pub(crate) fn build_rows(
    diff: &TaskDiff,
    infos: &[FileInfo],
    shown: &[usize],
    collapsed: impl Fn(usize) -> bool,
    style: DiffStyle,
) -> BodyRows {
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

        if collapsed(*index) {
            continue;
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

            match style {
                DiffStyle::Unified => unified_rows(&mut body.rows, at, hunk_at, hunk),
                DiffStyle::Split => split_rows(&mut body.rows, at, hunk_at, hunk),
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

    let mut files: Vec<FileDiff> = SAMPLE_FILES
        .iter()
        .map(|(agent, path, change, additions, deletions)| {
            let worktree = match (source, own.get(*agent)) {
                (ReviewSource::Live, Some(worktree)) => worktree.clone(),
                _ => task_worktree.clone(),
            };

            generated_file(
                worktree,
                path,
                agents.get(*agent).copied(),
                *change,
                *additions,
                *deletions,
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
    let section = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".rs"))
        .map(|name| format!("impl {} ", name.replace(['-', '_'], "")))
        .unwrap_or_default();

    let hunks = if change == FileChange::Added {
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
    };

    FileDiff {
        worktree,
        path: PathBuf::from(path),
        agent,
        change,
        mode_change: None,
        additions,
        deletions,
        binary: false,
        hunks,
        truncated: false,
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
        }
    }

    fn diff(files: Vec<FileDiff>) -> TaskDiff {
        TaskDiff {
            task: TaskId(1),
            source: ReviewSource::Merged,
            fingerprint: 7,
            stat: DiffStat::default(),
            files,
        }
    }

    fn label(id: u64, name: &str, kind: AgentKind) -> AgentLabel {
        AgentLabel {
            id: DaemonAgentId(id),
            name: name.to_owned().into(),
            kind,
        }
    }

    fn rows_of(diff: &TaskDiff, style: DiffStyle) -> Vec<Row> {
        let infos = file_infos(diff);
        let shown: Vec<usize> = (0..diff.files.len()).collect();

        build_rows(diff, &infos, &shown, |_| false, style).rows
    }

    #[test]
    fn files_group_by_agent_with_other_changes_last() {
        let diff = diff(vec![
            file("a.rs", None, Vec::new()),
            file("b.rs", Some(2), Vec::new()),
            file("c.rs", Some(1), Vec::new()),
            file("d.rs", Some(99), Vec::new()),
        ]);
        let agents = [
            label(1, "term-render", AgentKind::ClaudeCode),
            label(2, "pty-io", AgentKind::Codex),
        ];
        let visible = visible_files(&diff, None);

        let groups = group_files(&diff.files, &visible, Grouping::ByAgent, &agents);

        let summary: Vec<_> = groups
            .iter()
            .map(|group| (group.title.to_string(), group.files.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                ("term-render".to_owned(), vec![2]),
                ("pty-io".to_owned(), vec![1]),
                (OTHER_CHANGES.to_owned(), vec![0, 3]),
            ]
        );
        assert_eq!(groups[1].kind, GroupKind::Agent(AgentKind::Codex));
    }

    #[test]
    fn files_group_by_their_folder() {
        let diff = diff(vec![
            file("src/b.rs", None, Vec::new()),
            file("Cargo.toml", None, Vec::new()),
            file("src/a.rs", None, Vec::new()),
        ]);
        let visible = visible_files(&diff, None);

        let groups = group_files(&diff.files, &visible, Grouping::ByFolder, &[]);

        let summary: Vec<_> = groups
            .iter()
            .map(|group| (group.title.to_string(), group.files.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (ROOT_FOLDER.to_owned(), vec![1]),
                ("src".to_owned(), vec![0, 2]),
            ]
        );
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
            |index| index == 0,
            DiffStyle::Unified,
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
    fn a_mode_change_changes_the_hash() {
        let plain = file("a.rs", None, vec![line(DiffLineKind::Added, "x")]);
        let mut moded = plain.clone();
        moded.mode_change = Some((0o100_644, 0o100_755));

        assert_ne!(file_hash(&plain), file_hash(&moded));
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
    fn the_hash_changes_with_the_hunks() {
        let one = file("a.rs", None, vec![line(DiffLineKind::Added, "x")]);
        let same = one.clone();
        let other = file("a.rs", None, vec![line(DiffLineKind::Added, "y")]);

        assert_eq!(file_hash(&one), file_hash(&same));
        assert_ne!(file_hash(&one), file_hash(&other));
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
            (9, 321, 41)
        );
        assert_eq!(worktree_count(&live), 3);
        assert_eq!(worktree_count(&merged), 1);

        for file in &merged.files {
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
}
