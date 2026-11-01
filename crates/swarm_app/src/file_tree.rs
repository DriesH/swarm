//! The file tree of Review and the Changes panel: plain data, no gpui entities.
//!
//! Files from different worktrees merge by their relative path. Folders come first, then
//! files, each sorted by name. A folder whose only child is one folder shows as one row.

use std::collections::HashSet;
use std::path::{Component, Path};

use gpui::SharedString;
use swarm_protocol::FileDiff;
use swarm_ui::file_icons::FileIconName;

use crate::review_model::{AgentLabel, FileKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Folder {
        open: bool,
    },
    /// An index into [`swarm_protocol::TaskDiff::files`].
    File {
        file: usize,
    },
}

/// One visible row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub kind: EntryKind,
    pub level: usize,
    /// The folder names joined by `/` for a folder row, the file name for a file row.
    pub name: SharedString,
    /// The path of the folder, down to the deepest folder of a compacted row. For a
    /// file, the path of its folder. This is what the closed set holds.
    pub folder: SharedString,
    /// Set on a file whose relative path shows more than once: the agent it belongs to.
    pub agent: Option<SharedString>,
    /// The folder row this row sits under.
    pub parent: Option<usize>,
    /// The 1-based place among the rows with the same parent, for a screen reader.
    pub position_in_set: usize,
    /// How many rows have the same parent.
    pub size_of_set: usize,
}

impl Entry {
    /// The icon of the row: a folder icon from the last folder of its name, open or closed,
    /// or a file icon from the file name.
    pub(crate) fn file_icon(&self) -> FileIconName {
        match self.kind {
            EntryKind::Folder { open } => FileIconName::for_folder(&self.name, open),
            EntryKind::File { .. } => FileIconName::for_file(&self.name),
        }
    }
}

/// What the cursor of a tree stands on. It names a row by its content, so it survives a
/// new diff that moves the rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Folder(String),
    File(FileKey),
}

#[derive(Default)]
struct Dir {
    dirs: Vec<(String, Dir)>,
    files: Vec<usize>,
}

impl Dir {
    fn child(&mut self, name: &str) -> &mut Self {
        let position = self.dirs.iter().position(|(other, _)| other == name);
        let position = position.unwrap_or_else(|| {
            self.dirs.push((name.to_owned(), Self::default()));

            self.dirs.len() - 1
        });

        &mut self.dirs[position].1
    }
}

struct Build<'a> {
    files: &'a [FileDiff],
    closed: &'a HashSet<String>,
    /// The relative paths that more than one shown file has.
    repeated: HashSet<&'a Path>,
    agents: &'a [AgentLabel],
    entries: Vec<Entry>,
}

/// The visible rows for the `shown` files of a diff, without the rows under a folder in
/// `closed`.
pub(crate) fn build(
    files: &[FileDiff],
    shown: &[usize],
    agents: &[AgentLabel],
    closed: &HashSet<String>,
) -> Vec<Entry> {
    let mut root = Dir::default();
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut repeated: HashSet<&Path> = HashSet::new();

    for index in shown {
        let Some(file) = files.get(*index) else {
            continue;
        };
        let mut dir = &mut root;

        for name in folder_names(&file.path) {
            dir = dir.child(&name);
        }
        dir.files.push(*index);

        if !seen.insert(file.path.as_path()) {
            repeated.insert(file.path.as_path());
        }
    }

    let mut build = Build {
        files,
        closed,
        repeated,
        agents,
        entries: Vec::new(),
    };

    build.dir(&root, "", 0, None);

    build.entries
}

impl Build<'_> {
    fn dir(&mut self, dir: &Dir, folder: &str, level: usize, parent: Option<usize>) {
        let mut dirs: Vec<&(String, Dir)> = dir.dirs.iter().collect();
        dirs.sort_by(|(a, _), (b, _)| compare_names(a, b));

        let all_files = self.files;
        let mut files: Vec<(usize, &FileDiff)> = dir
            .files
            .iter()
            .filter_map(|index| all_files.get(*index).map(|file| (*index, file)))
            .collect();
        files.sort_by(|(a_index, a), (b_index, b)| {
            compare_names(&file_name(a), &file_name(b)).then(a_index.cmp(b_index))
        });

        let size_of_set = dirs.len() + files.len();
        let mut position_in_set = 0;

        for (name, child) in dirs {
            position_in_set += 1;

            let mut label = name.clone();
            let mut path = join(folder, name);
            let mut node = child;

            while node.files.is_empty() && node.dirs.len() == 1 {
                let (next, deeper) = &node.dirs[0];

                label = format!("{label}/{next}");
                path = join(&path, next);
                node = deeper;
            }

            let open = !self.closed.contains(&path);
            let index = self.entries.len();

            self.entries.push(Entry {
                kind: EntryKind::Folder { open },
                level,
                name: label.into(),
                folder: path.clone().into(),
                agent: None,
                parent,
                position_in_set,
                size_of_set,
            });

            if open {
                self.dir(node, &path, level + 1, Some(index));
            }
        }

        for (index, file) in files {
            position_in_set += 1;

            let agent = self
                .repeated
                .contains(file.path.as_path())
                .then(|| self.agent_name(file));

            self.entries.push(Entry {
                kind: EntryKind::File { file: index },
                level,
                name: file_name(file).into(),
                folder: folder.to_owned().into(),
                agent,
                parent,
                position_in_set,
                size_of_set,
            });
        }
    }

    fn agent_name(&self, file: &FileDiff) -> SharedString {
        let agent = file
            .agent
            .and_then(|id| self.agents.iter().find(|agent| agent.id == id));

        match agent {
            Some(agent) => agent.name.clone(),
            None => file
                .worktree
                .file_name()
                .map_or_else(SharedString::default, |name| {
                    name.to_string_lossy().into_owned().into()
                }),
        }
    }
}

fn folder_names(path: &Path) -> Vec<String> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };

    parent
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn file_name(file: &FileDiff) -> String {
    file.path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// Case-insensitive, so `README.md` sits among the other files and not before `a.rs`.
fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// What a row stands for.
pub(crate) fn mark_of(entry: &Entry, files: &[FileDiff]) -> Option<Mark> {
    match entry.kind {
        EntryKind::Folder { .. } => Some(Mark::Folder(entry.folder.to_string())),
        EntryKind::File { file } => files
            .get(file)
            .map(|file| Mark::File((file.worktree.clone(), file.path.clone()))),
    }
}

/// The row that shows `mark`. A file under a closed folder shows as that folder. A folder
/// that is no row of its own, because a new diff merged it into another row or left it
/// out, shows as the row it merged into, else as its nearest folder above.
pub(crate) fn row_of(entries: &[Entry], files: &[FileDiff], mark: &Mark) -> Option<usize> {
    match mark {
        Mark::File(key) => entries
            .iter()
            .position(|entry| match entry.kind {
                EntryKind::File { file } => files
                    .get(file)
                    .is_some_and(|data| data.worktree == key.0 && data.path == key.1),
                EntryKind::Folder { .. } => false,
            })
            .or_else(|| closest_folder(entries, &key.1, true)),
        Mark::Folder(path) => {
            let folder = |entry: &Entry| matches!(entry.kind, EntryKind::Folder { .. });

            entries
                .iter()
                .position(|entry| folder(entry) && entry.folder == path.as_str())
                .or_else(|| {
                    entries.iter().position(|entry| {
                        folder(entry) && Path::new(&*entry.folder).starts_with(path)
                    })
                })
                .or_else(|| closest_folder(entries, Path::new(path), false))
        }
    }
}

/// The deepest folder row whose folder holds `path`. With `closed_only`, only a closed
/// folder counts, since that is the row that hides what is below it.
fn closest_folder(entries: &[Entry], path: &Path, closed_only: bool) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| match entry.kind {
            EntryKind::Folder { open } => {
                (!closed_only || !open) && path.starts_with(&*entry.folder)
            }
            EntryKind::File { .. } => false,
        })
        .max_by_key(|(_, entry)| entry.level)
        .map(|(index, _)| index)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    /// Put the cursor on this row.
    Select(usize),
    Open(usize),
    Close(usize),
    Stay,
}

/// What an arrow key does when the cursor is on `current`, the WAI-ARIA tree pattern:
/// up and down move, right opens a folder or steps into it, left closes it or steps out.
pub(crate) fn step(entries: &[Entry], current: Option<usize>, key: Key) -> Move {
    let last = entries.len().saturating_sub(1);

    if entries.is_empty() {
        return Move::Stay;
    }

    let Some(current) = current.filter(|current| *current <= last) else {
        return Move::Select(0);
    };

    match key {
        Key::Up => Move::Select(current.saturating_sub(1)),
        Key::Down => Move::Select((current + 1).min(last)),
        Key::Right => match entries[current].kind {
            EntryKind::Folder { open: false } => Move::Open(current),
            EntryKind::Folder { open: true }
                if entries
                    .get(current + 1)
                    .is_some_and(|next| next.parent == Some(current)) =>
            {
                Move::Select(current + 1)
            }
            _ => Move::Stay,
        },
        Key::Left => match entries[current] {
            Entry {
                kind: EntryKind::Folder { open: true },
                ..
            } => Move::Close(current),
            Entry {
                parent: Some(parent),
                ..
            } => Move::Select(parent),
            _ => Move::Stay,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use swarm_protocol::{AgentId, FileChange};

    use super::*;
    use crate::model::AgentKind;

    fn file(worktree: &str, path: &str, agent: Option<u64>) -> FileDiff {
        FileDiff {
            worktree: PathBuf::from(worktree),
            path: PathBuf::from(path),
            agent: agent.map(AgentId),
            change: FileChange::Modified,
            mode_change: None,
            additions: 1,
            deletions: 0,
            binary: false,
            hunks: Vec::new(),
            truncated: false,
            digest: 0,
            fold: None,
        }
    }

    fn agent(id: u64, name: &str) -> AgentLabel {
        AgentLabel {
            id: AgentId(id),
            name: name.to_owned().into(),
            kind: AgentKind::ClaudeCode,
        }
    }

    fn tree(files: &[FileDiff], closed: &[&str]) -> Vec<Entry> {
        let shown: Vec<usize> = (0..files.len()).collect();
        let closed = closed.iter().map(|path| (*path).to_owned()).collect();

        build(files, &shown, &[agent(1, "term-render")], &closed)
    }

    /// `level name` of each row, folders with a trailing slash.
    fn outline(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| {
                let slash = if matches!(entry.kind, EntryKind::Folder { .. }) {
                    "/"
                } else {
                    ""
                };

                format!("{} {}{slash}", entry.level, entry.name)
            })
            .collect()
    }

    #[test]
    fn each_row_knows_its_place_among_the_rows_of_its_folder() {
        let files = [
            file("/w", "z.rs", None),
            file("/w", "src/b.rs", None),
            file("/w", "docs/a.md", None),
            file("/w", "src/c.rs", None),
            file("/w", "a.rs", None),
        ];

        let entries = tree(&files, &[]);

        let places: Vec<_> = entries
            .iter()
            .map(|entry| (&*entry.name, entry.position_in_set, entry.size_of_set))
            .collect();
        assert_eq!(
            places,
            [
                ("docs", 1, 4),
                ("a.md", 1, 1),
                ("src", 2, 4),
                ("b.rs", 1, 2),
                ("c.rs", 2, 2),
                ("a.rs", 3, 4),
                ("z.rs", 4, 4),
            ]
        );
    }

    #[test]
    fn a_closed_folder_still_counts_in_its_set() {
        let files = [file("/w", "src/b.rs", None), file("/w", "a.rs", None)];

        let entries = tree(&files, &["src"]);

        let places: Vec<_> = entries
            .iter()
            .map(|entry| (&*entry.name, entry.position_in_set, entry.size_of_set))
            .collect();
        assert_eq!(places, [("src", 1, 2), ("a.rs", 2, 2)]);
    }

    #[test]
    fn folders_come_first_then_files_each_sorted_by_name() {
        let files = [
            file("/w", "z.rs", None),
            file("/w", "src/b.rs", None),
            file("/w", "README.md", None),
            file("/w", "benches/grid.rs", None),
            file("/w", "src/a.rs", None),
            file("/w", "a.rs", None),
        ];

        let entries = tree(&files, &[]);

        assert_eq!(
            outline(&entries),
            [
                "0 benches/",
                "1 grid.rs",
                "0 src/",
                "1 a.rs",
                "1 b.rs",
                "0 a.rs",
                "0 README.md",
                "0 z.rs",
            ]
        );
    }

    #[test]
    fn a_folder_with_one_folder_and_no_files_is_one_row() {
        let files = [
            file("/w", "crates/swarm_term/src/grid.rs", None),
            file("/w", "crates/swarm_term/src/pty.rs", None),
            file("/w", "docs/PLAN.md", None),
        ];

        let entries = tree(&files, &[]);

        assert_eq!(
            outline(&entries),
            [
                "0 crates/swarm_term/src/",
                "1 grid.rs",
                "1 pty.rs",
                "0 docs/",
                "1 PLAN.md",
            ]
        );
    }

    #[test]
    fn a_folder_with_a_file_or_two_folders_does_not_compact() {
        let files = [
            file("/w", "crates/lib.rs", None),
            file("/w", "crates/a/x.rs", None),
            file("/w", "tools/b/y.rs", None),
            file("/w", "tools/c/z.rs", None),
        ];

        let entries = tree(&files, &[]);

        assert_eq!(
            outline(&entries),
            [
                "0 crates/",
                "1 a/",
                "2 x.rs",
                "1 lib.rs",
                "0 tools/",
                "1 b/",
                "2 y.rs",
                "1 c/",
                "2 z.rs",
            ]
        );
    }

    #[test]
    fn folders_start_open_and_a_closed_one_hides_its_rows() {
        let files = [
            file("/w", "crates/term/src/grid.rs", None),
            file("/w", "docs/PLAN.md", None),
        ];

        let open = tree(&files, &[]);
        let closed = tree(&files, &["crates/term/src"]);

        assert_eq!(open.len(), 4);
        assert_eq!(
            outline(&closed),
            ["0 crates/term/src/", "0 docs/", "1 PLAN.md"]
        );
        assert_eq!(closed[0].kind, EntryKind::Folder { open: false });
        assert_eq!(closed[0].folder, "crates/term/src");
    }

    #[test]
    fn closed_folders_stay_closed_when_the_files_change() {
        let before = [file("/w", "src/a.rs", None), file("/w", "docs/x.md", None)];
        let after = [
            file("/w", "src/a.rs", None),
            file("/w", "src/b.rs", None),
            file("/w", "docs/x.md", None),
            file("/w", "docs/y.md", None),
        ];

        let first = tree(&before, &["src"]);
        let second = tree(&after, &["src"]);

        assert_eq!(outline(&first), ["0 docs/", "1 x.md", "0 src/"]);
        assert_eq!(outline(&second), ["0 docs/", "1 x.md", "1 y.md", "0 src/"]);
    }

    #[test]
    fn a_row_knows_its_level_and_its_parent() {
        let files = [file("/w", "a/b/x.rs", None), file("/w", "a/c.rs", None)];

        let entries = tree(&files, &[]);

        assert_eq!(outline(&entries), ["0 a/", "1 b/", "2 x.rs", "1 c.rs"]);
        let parents: Vec<_> = entries.iter().map(|entry| entry.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(1), Some(0)]);
    }

    #[test]
    fn the_same_path_in_two_worktrees_shows_once_per_file_with_its_agent() {
        let files = [
            file("/w", "crates/a.rs", None),
            file("/w", "crates/only.rs", None),
            file("/w--one", "crates/a.rs", Some(1)),
            file("/w--two", "crates/a.rs", Some(9)),
        ];

        let entries = tree(&files, &[]);

        assert_eq!(
            outline(&entries),
            ["0 crates/", "1 a.rs", "1 a.rs", "1 a.rs", "1 only.rs"]
        );
        let agents: Vec<_> = entries
            .iter()
            .map(|entry| entry.agent.as_ref().map(ToString::to_string))
            .collect();
        assert_eq!(
            agents,
            [
                None,
                Some("w".to_owned()),
                Some("term-render".to_owned()),
                Some("w--two".to_owned()),
                None,
            ]
        );
    }

    #[test]
    fn a_hidden_file_does_not_make_a_path_repeat() {
        let files = [file("/w", "a.rs", None), file("/w--one", "a.rs", Some(1))];

        let entries = build(&files, &[1], &[agent(1, "term-render")], &HashSet::new());

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent, None);
    }

    fn marks(entries: &[Entry], files: &[FileDiff]) -> Vec<Option<Mark>> {
        entries.iter().map(|entry| mark_of(entry, files)).collect()
    }

    #[test]
    fn a_mark_finds_its_row_again_after_the_rows_move() {
        let before = [file("/w", "src/a.rs", None)];
        let after = [file("/w", "docs/x.md", None), file("/w", "src/a.rs", None)];

        let entries = tree(&before, &[]);
        let mark = marks(&entries, &before)[1].clone();
        let moved = tree(&after, &[]);

        assert_eq!(
            mark.as_ref().and_then(|mark| row_of(&moved, &after, mark)),
            Some(3)
        );
    }

    #[test]
    fn a_file_under_a_closed_folder_shows_as_that_folder() {
        let files = [file("/w", "src/deep/a.rs", None)];
        let entries = tree(&files, &["src/deep"]);
        let mark = Mark::File((PathBuf::from("/w"), PathBuf::from("src/deep/a.rs")));

        assert_eq!(row_of(&entries, &files, &mark), Some(0));
    }

    #[test]
    fn a_folder_that_merged_into_a_row_shows_as_that_row() {
        let files = [file("/w", "a/b/c.rs", None)];
        let entries = tree(&files, &[]);

        assert_eq!(
            row_of(&entries, &files, &Mark::Folder("a".to_owned())),
            Some(0)
        );
    }

    fn arrows(files: &[FileDiff], closed: &[&str]) -> (Vec<Entry>, impl Fn(usize, Key) -> Move) {
        let entries = tree(files, closed);
        let copy = entries.clone();

        (entries, move |current, key| step(&copy, Some(current), key))
    }

    #[test]
    fn up_and_down_move_one_row_and_stop_at_the_ends() {
        let files = [file("/w", "a/x.rs", None), file("/w", "b.rs", None)];
        let (entries, press) = arrows(&files, &[]);

        assert_eq!(entries.len(), 3);
        assert_eq!(press(0, Key::Down), Move::Select(1));
        assert_eq!(press(2, Key::Down), Move::Select(2));
        assert_eq!(press(2, Key::Up), Move::Select(1));
        assert_eq!(press(0, Key::Up), Move::Select(0));
        assert_eq!(step(&entries, None, Key::Down), Move::Select(0));
        assert_eq!(step(&[], None, Key::Down), Move::Stay);
    }

    #[test]
    fn right_opens_a_closed_folder_then_steps_to_its_first_row() {
        let files = [file("/w", "a/x.rs", None), file("/w", "a/y.rs", None)];
        let (_, closed) = arrows(&files, &["a"]);
        let (_, open) = arrows(&files, &[]);

        assert_eq!(closed(0, Key::Right), Move::Open(0));
        assert_eq!(open(0, Key::Right), Move::Select(1));
        assert_eq!(open(1, Key::Right), Move::Stay);
    }

    #[test]
    fn left_closes_an_open_folder_else_steps_to_the_parent() {
        let files = [file("/w", "a/b/x.rs", None), file("/w", "a/y.rs", None)];
        let (entries, press) = arrows(&files, &[]);

        assert_eq!(outline(&entries), ["0 a/", "1 b/", "2 x.rs", "1 y.rs"]);
        assert_eq!(press(2, Key::Left), Move::Select(1));
        assert_eq!(press(1, Key::Left), Move::Close(1));
        assert_eq!(press(3, Key::Left), Move::Select(0));
        assert_eq!(press(0, Key::Left), Move::Close(0));

        let (_, collapsed) = arrows(&files, &["a"]);
        assert_eq!(collapsed(0, Key::Left), Move::Stay);
    }

    #[test]
    fn folders_get_a_folder_icon_by_their_last_name_and_state() {
        let files = [
            file("/w", "crates/swarm_term/src/grid.rs", None),
            file("/w", "docs/PLAN.md", None),
            file("/w", "tests/a.rs", None),
        ];

        let open = tree(&files, &[]);
        let closed = tree(&files, &["crates/swarm_term/src", "docs"]);

        assert_eq!(outline(&open)[0], "0 crates/swarm_term/src/");
        assert_eq!(open[0].file_icon(), FileIconName::FolderSrcOpen);
        assert_eq!(open[2].file_icon(), FileIconName::FolderDocsOpen);
        assert_eq!(open[4].file_icon(), FileIconName::FolderTestOpen);
        assert_eq!(closed[0].file_icon(), FileIconName::FolderSrc);
        assert_eq!(closed[1].file_icon(), FileIconName::FolderDocs);
        assert_eq!(closed[2].file_icon(), FileIconName::FolderTestOpen);
    }

    #[test]
    fn files_get_a_file_icon_by_their_name() {
        let files = [
            file("/w", "Cargo.toml", None),
            file("/w", "README.md", None),
            file("/w", "src/main.rs", None),
            file("/w", "Cargo.lock", None),
        ];

        let entries = tree(&files, &[]);
        let icons: Vec<_> = entries
            .iter()
            .map(|entry| (&*entry.name, entry.file_icon()))
            .collect();

        assert_eq!(
            icons,
            [
                ("src", FileIconName::FolderSrcOpen),
                ("main.rs", FileIconName::File),
                ("Cargo.lock", FileIconName::File),
                ("Cargo.toml", FileIconName::Config),
                ("README.md", FileIconName::Readme),
            ]
        );
    }
}
