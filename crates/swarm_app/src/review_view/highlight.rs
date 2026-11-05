//! Syntax colors in the diff: the whole old and new text of a file is highlighted once on
//! a background thread, and the lines of the diff look their colors up by line number.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use gpui::{Context, HighlightStyle, Hsla, Task};
use swarm_protocol::{Client, DiffSide, FileDiff, FileTexts};
use swarm_syntax::{Language, Span, Token};
use swarm_ui::ThemeColors;

use super::ReviewView;
use crate::review_model::LoadKey;

/// The colors of one line, and a hash of the text they were made for. A line of the diff
/// whose text differs (the file moved on since the diff was read) stays plain.
struct LineMarks {
    hash: u64,
    spans: Vec<Span>,
}

/// The colors of every line of a file, on each side of the diff.
#[derive(Default)]
pub(super) struct FileHighlights {
    old: Vec<LineMarks>,
    new: Vec<LineMarks>,
}

fn hash_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);

    hasher.finish()
}

fn side_marks(language: Language, lines: Option<&[String]>) -> Vec<LineMarks> {
    let Some(lines) = lines else {
        return Vec::new();
    };

    let text = lines.join("\n");
    let spans = swarm_syntax::highlight_cached(language, &text);
    let per_line = swarm_syntax::split_lines(&text, &spans);

    lines
        .iter()
        .zip(per_line)
        .map(|(line, spans)| LineMarks {
            hash: hash_of(line),
            spans,
        })
        .collect()
}

impl FileHighlights {
    /// Highlights both sides. Heavy: call it off the UI thread.
    pub(super) fn compute(language: Language, texts: &FileTexts) -> Self {
        Self {
            old: side_marks(language, texts.old.as_deref()),
            new: side_marks(language, texts.new.as_deref()),
        }
    }

    /// The colored ranges of a line of the diff, in the text as it is drawn. `number` is
    /// the line number on `side`; `text` is the line as the diff has it.
    pub(super) fn line(
        &self,
        side: DiffSide,
        number: u32,
        text: &str,
        colors: &ThemeColors,
    ) -> Option<Vec<(Range<usize>, HighlightStyle)>> {
        let lines = match side {
            DiffSide::Old => &self.old,
            DiffSide::New => &self.new,
        };
        let marks = lines.get(usize::try_from(number).ok()?.checked_sub(1)?)?;
        if marks.hash != hash_of(text) {
            return None;
        }

        let expand = expansion(text);
        let styles = marks
            .spans
            .iter()
            .filter_map(|(range, token)| {
                let start = expand(range.start)?;
                let end = expand(range.end)?;

                (start < end).then(|| {
                    (
                        start..end,
                        HighlightStyle {
                            color: Some(token_color(*token, colors)),
                            ..HighlightStyle::default()
                        },
                    )
                })
            })
            .collect();

        Some(styles)
    }
}

/// Maps a byte offset of the line to the offset in the drawn text, where a tab is four
/// spaces. `None` for an offset inside a character.
fn expansion(text: &str) -> impl Fn(usize) -> Option<usize> + '_ {
    let tabs = text.contains('\t');

    move |offset| {
        let before = text.get(..offset)?;
        if !tabs {
            return Some(offset);
        }

        let tab_count = before.bytes().filter(|byte| *byte == b'\t').count();

        Some(offset + tab_count * 3)
    }
}

pub(super) fn token_color(token: Token, colors: &ThemeColors) -> Hsla {
    match token {
        Token::Keyword => colors.syntax_keyword,
        Token::String => colors.syntax_string,
        Token::Comment => colors.syntax_comment,
        Token::Function => colors.syntax_function,
        Token::Type => colors.syntax_type,
        Token::Number => colors.syntax_number,
        Token::Constant => colors.syntax_constant,
        Token::Punctuation => colors.syntax_punctuation,
    }
}

/// At most this many files are read at the same time.
const MAX_IN_FLIGHT: usize = 4;

/// Where the colors of a file stand.
enum State {
    /// Asked for, and waiting for a free place; the lines draw plain meanwhile.
    Queued,
    /// On its way. Dropping the task cancels the request.
    Running(Task<()>),
    Ready(Arc<FileHighlights>),
    /// The texts could not be read.
    Plain,
}

/// A file that waits for a free place.
struct Wanted {
    key: LoadKey,
    digest: u64,
    language: Language,
    /// The place of the file in the diff.
    place: usize,
}

/// A file ready to be read.
pub(super) struct Start {
    key: LoadKey,
    digest: u64,
    language: Language,
}

/// The colors of the files of the diff, each for the digest it was made for. Only files
/// whose lines are on screen are read, a few at a time.
#[derive(Default)]
pub(super) struct Highlights {
    files: HashMap<LoadKey, (u64, State)>,
    queue: Vec<Wanted>,
    /// The files whose lines the frame that is being drawn shows, with their place in the
    /// diff.
    drawing: HashMap<LoadKey, usize>,
    /// The same for the frame before.
    drawn: HashMap<LoadKey, usize>,
    /// The most files that were read at the same time.
    #[cfg(test)]
    peak_running: usize,
}

impl Highlights {
    pub(super) fn clear(&mut self) {
        self.files.clear();
        self.queue.clear();
        self.drawing.clear();
        self.drawn.clear();
    }

    /// Notes that a line of the file is on screen in the frame that is being drawn.
    pub(super) fn see(&mut self, key: &LoadKey, place: usize) {
        self.drawing.insert(key.clone(), place);
    }

    /// Starts a new frame. A file that was asked for and was on screen in no line of the
    /// frame before has left the viewport: its request is dropped, and so is its place in
    /// the queue.
    pub(super) fn settle(&mut self) {
        self.drawn = std::mem::take(&mut self.drawing);

        let drawn = &self.drawn;
        self.files.retain(|key, (_, state)| {
            !matches!(state, State::Queued | State::Running(_)) || drawn.contains_key(key)
        });
        self.queue.retain(|wanted| drawn.contains_key(&wanted.key));
    }

    /// Asks for the colors of a file. False when this digest was asked for already.
    pub(super) fn ask(
        &mut self,
        key: &LoadKey,
        digest: u64,
        language: Language,
        place: usize,
    ) -> bool {
        if !self.begin(key, digest) {
            return false;
        }

        self.queue.push(Wanted {
            key: key.clone(),
            digest,
            language,
            place,
        });

        true
    }

    /// The queued file nearest to the top of the screen, when a place is free.
    pub(super) fn next_to_start(&mut self) -> Option<Start> {
        if self.running() >= MAX_IN_FLIGHT {
            return None;
        }

        let top = self
            .drawing
            .values()
            .chain(self.drawn.values())
            .min()
            .copied()
            .unwrap_or_default();
        let nearest = self
            .queue
            .iter()
            .enumerate()
            .min_by_key(|(_, wanted)| wanted.place.abs_diff(top))
            .map(|(index, _)| index)?;
        let wanted = self.queue.remove(nearest);

        Some(Start {
            key: wanted.key,
            digest: wanted.digest,
            language: wanted.language,
        })
    }

    /// Keeps the task that reads a file, as long as the file is wanted.
    pub(super) fn run(&mut self, start: &Start, task: Task<()>) {
        match self.files.get_mut(&start.key) {
            Some((digest, state)) if *digest == start.digest => *state = State::Running(task),
            _ => drop(task),
        }

        #[cfg(test)]
        {
            self.peak_running = self.peak_running.max(self.running());
        }
    }

    #[cfg(test)]
    pub(super) fn peak_running(&self) -> usize {
        self.peak_running
    }

    pub(super) fn running(&self) -> usize {
        self.files
            .values()
            .filter(|(_, state)| matches!(state, State::Running(_)))
            .count()
    }

    #[cfg(test)]
    pub(super) fn queued(&self) -> usize {
        self.queue.len()
    }

    /// True while the file is queued or on its way.
    #[cfg(test)]
    pub(super) fn is_wanted(&self, key: &LoadKey) -> bool {
        self.files
            .get(key)
            .is_some_and(|(_, state)| matches!(state, State::Queued | State::Running(_)))
    }

    /// The colors of a file, when they are ready for this `digest`.
    pub(super) fn get(&self, key: &LoadKey, digest: u64) -> Option<Arc<FileHighlights>> {
        match self.files.get(key) {
            Some((stored, State::Ready(highlights))) if *stored == digest => {
                Some(Arc::clone(highlights))
            }
            _ => None,
        }
    }

    /// Marks the file as asked for. False when this digest was asked for already.
    fn begin(&mut self, key: &LoadKey, digest: u64) -> bool {
        if self
            .files
            .get(key)
            .is_some_and(|(stored, _)| *stored == digest)
        {
            return false;
        }

        self.files.insert(key.clone(), (digest, State::Queued));
        self.queue.retain(|wanted| wanted.key != *key);

        true
    }

    /// Keeps an answer, unless the file moved to another digest while it was on its way.
    fn finish(&mut self, key: &LoadKey, digest: u64, highlights: Option<FileHighlights>) {
        let Some((stored, state)) = self.files.get_mut(key) else {
            return;
        };
        if *stored != digest {
            return;
        }

        let done = match highlights {
            Some(highlights) => State::Ready(Arc::new(highlights)),
            None => State::Plain,
        };

        // The task that calls this is the one in `Running`: let it end by itself.
        if let State::Running(task) = std::mem::replace(state, done) {
            task.detach();
        }
    }
}

/// The language of a file of the diff, or `None` when the file stays plain: folded
/// (lock, generated and large files), binary, or a language without a grammar.
fn language_of(file: &FileDiff) -> Option<Language> {
    if file.fold.is_some() || file.binary {
        return None;
    }

    Language::from_path(Path::new(&file.path))
}

impl ReviewView {
    /// The colors of a file when they are ready. The first time one of its lines is drawn
    /// this asks for them, and the lines draw plain until the answer comes. The text does
    /// not move when it does.
    pub(super) fn colors_of(
        &mut self,
        file: usize,
        cx: &mut Context<Self>,
    ) -> Option<Arc<FileHighlights>> {
        let diff = self.diff().cloned()?;
        let data = diff.files.get(file)?;
        let key: LoadKey = (
            diff.task,
            diff.source,
            data.worktree.clone(),
            data.path.clone(),
        );

        if let Some(ready) = self.highlights.get(&key, data.digest) {
            return Some(ready);
        }

        self.highlights.see(&key, file);
        if let Some(language) = language_of(data) {
            self.highlights.ask(&key, data.digest, language, file);
        }
        self.start_colors(cx);

        None
    }

    /// A new frame begins: the files that left the viewport stop being read.
    pub(super) fn settle_colors(&mut self, cx: &mut Context<Self>) {
        self.highlights.settle();
        self.start_colors(cx);
    }

    /// Starts the files that wait, nearest to the top first, while a place is free.
    fn start_colors(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone().filter(Client::is_connected) else {
            return;
        };

        while let Some(start) = self.highlights.next_to_start() {
            let task = Self::read_colors(&start, client.clone(), cx);

            self.highlights.run(&start, task);
        }
    }

    /// Reads both sides of the file and colors them on the background executor. Dropping
    /// the task ends the wait for the answer.
    fn read_colors(start: &Start, client: Client, cx: &mut Context<Self>) -> Task<()> {
        let (key, digest, language) = (start.key.clone(), start.digest, start.language);

        cx.spawn(async move |this, cx| {
            let reading = cx.background_executor().spawn({
                let key = key.clone();

                async move {
                    let texts = client.file_texts(key.0, key.1, key.2, key.3)?;

                    Ok::<_, swarm_protocol::ClientError>(FileHighlights::compute(language, &texts))
                }
            });

            let answer = reading.await;
            if let Err(error) = &answer {
                log::warn!("cannot read a file to color it: {error}");
            }

            let updated = this.update(cx, |this, cx| {
                this.highlights.finish(&key, digest, answer.ok());
                this.start_colors(cx);
                cx.notify();
            });
            if let Err(error) = updated {
                log::debug!("the window closed before the colors arrived: {error}");
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn colors_running(&self) -> usize {
        self.highlights.running()
    }

    #[cfg(test)]
    pub(crate) fn colors_queued(&self) -> usize {
        self.highlights.queued()
    }

    #[cfg(test)]
    pub(crate) fn colors_peak_running(&self) -> usize {
        self.highlights.peak_running()
    }

    /// True while the colors of the file are queued or on their way.
    #[cfg(test)]
    pub(crate) fn colors_wanted(&self, file: usize) -> bool {
        let Some(diff) = self.diff() else {
            return false;
        };
        let Some(data) = diff.files.get(file) else {
            return false;
        };
        let key: LoadKey = (
            diff.task,
            diff.source,
            data.worktree.clone(),
            data.path.clone(),
        );

        self.highlights.is_wanted(&key)
    }

    #[cfg(test)]
    pub(crate) fn has_colors(&self, file: usize) -> bool {
        let Some(data) = self
            .diff()
            .and_then(|diff| Some((diff, diff.files.get(file)?)))
        else {
            return false;
        };
        let (diff, file) = data;
        let key: LoadKey = (
            diff.task,
            diff.source,
            file.worktree.clone(),
            file.path.clone(),
        );

        self.highlights.get(&key, file.digest).is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn rust_texts(new: &[&str]) -> FileTexts {
        FileTexts {
            old: None,
            new: Some(new.iter().map(|line| (*line).to_owned()).collect()),
        }
    }

    fn colors() -> ThemeColors {
        swarm_ui::Theme::dark().colors().clone()
    }

    #[test]
    fn a_line_gets_the_colors_of_the_whole_file() {
        let texts = rust_texts(&["let s = \"a", "b\";"]);
        let marks = FileHighlights::compute(Language::Rust, &texts);

        let first = marks.line(DiffSide::New, 1, "let s = \"a", &colors());
        let second = marks.line(DiffSide::New, 2, "b\";", &colors());

        assert!(first.is_some_and(|ranges| ranges.iter().any(|(range, _)| range.end == 10)));
        assert!(second.is_some_and(|ranges| ranges.iter().any(|(range, _)| range.start == 0)));
    }

    #[test]
    fn a_line_draws_colored_only_when_its_file_has_colors() {
        use swarm_protocol::{DiffLine, DiffLineKind};

        let line = DiffLine {
            kind: DiffLineKind::Added,
            text: "fn a() {}".to_owned(),
        };
        let marks = FileHighlights::compute(Language::Rust, &rust_texts(&["fn a() {}"]));

        let with = super::super::colored_text(&line, DiffSide::New, 1, Some(&marks), &colors());
        let without = super::super::colored_text(&line, DiffSide::New, 1, None, &colors());

        assert!(with.is_some());
        assert!(without.is_none());
    }

    #[test]
    fn a_line_whose_text_changed_stays_plain() {
        let marks = FileHighlights::compute(Language::Rust, &rust_texts(&["fn a() {}"]));

        assert!(
            marks
                .line(DiffSide::New, 1, "fn b() {}", &colors())
                .is_none()
        );
    }

    #[test]
    fn a_side_without_text_stays_plain() {
        let marks = FileHighlights::compute(Language::Rust, &rust_texts(&["fn a() {}"]));

        assert!(
            marks
                .line(DiffSide::Old, 1, "fn a() {}", &colors())
                .is_none()
        );
    }

    #[test]
    fn a_tab_is_four_spaces_in_the_colored_ranges() {
        let marks = FileHighlights::compute(Language::Rust, &rust_texts(&["\tfn a() {}"]));
        let ranges = marks
            .line(DiffSide::New, 1, "\tfn a() {}", &colors())
            .unwrap_or_default();

        // "fn" starts after the tab, which draws as four spaces.
        assert_eq!(ranges.first().map(|(range, _)| range.clone()), Some(4..6));
    }

    #[test]
    fn colors_of_an_older_digest_are_not_shown() {
        let key: LoadKey = (
            swarm_protocol::TaskId(1),
            swarm_protocol::ReviewSource::Live,
            "wt".into(),
            "a.rs".into(),
        );
        let mut highlights = Highlights::default();

        assert!(highlights.begin(&key, 1));
        highlights.finish(&key, 1, Some(FileHighlights::default()));
        assert!(highlights.get(&key, 1).is_some());

        assert!(highlights.begin(&key, 2));
        assert!(highlights.get(&key, 1).is_none());
        assert!(highlights.get(&key, 2).is_none());
    }

    #[test]
    fn an_answer_for_an_older_digest_is_dropped() {
        let key: LoadKey = (
            swarm_protocol::TaskId(1),
            swarm_protocol::ReviewSource::Live,
            "wt".into(),
            "a.rs".into(),
        );
        let mut highlights = Highlights::default();

        highlights.begin(&key, 1);
        highlights.begin(&key, 2);
        highlights.finish(&key, 1, Some(FileHighlights::default()));

        assert!(highlights.get(&key, 1).is_none());
        assert!(highlights.get(&key, 2).is_none());
    }

    fn key_of(path: &str) -> LoadKey {
        (
            swarm_protocol::TaskId(1),
            swarm_protocol::ReviewSource::Live,
            "wt".into(),
            path.into(),
        )
    }

    /// Asks for `count` files that are on screen and starts every one that may start.
    fn start_all(highlights: &mut Highlights, count: usize) -> Vec<Start> {
        for place in 0..count {
            let key = key_of(&format!("{place}.rs"));
            highlights.see(&key, place);
            highlights.ask(&key, 1, Language::Rust, place);
        }

        let mut started = Vec::new();
        while let Some(start) = highlights.next_to_start() {
            highlights.run(&start, Task::ready(()));
            started.push(start);
        }

        started
    }

    #[test]
    fn at_most_four_files_are_read_at_once() {
        let mut highlights = Highlights::default();

        let started = start_all(&mut highlights, 6);

        assert_eq!(started.len(), MAX_IN_FLIGHT);
        assert_eq!(highlights.running(), MAX_IN_FLIGHT);
        assert_eq!(highlights.queued(), 2);
    }

    #[test]
    fn a_finished_file_frees_a_place_for_the_next() {
        let mut highlights = Highlights::default();
        let started = start_all(&mut highlights, 6);

        highlights.finish(&started[0].key, 1, Some(FileHighlights::default()));
        let next = highlights.next_to_start();

        assert!(next.is_some());
    }

    #[test]
    fn the_file_nearest_to_the_top_of_the_screen_starts_first() {
        let mut highlights = Highlights::default();
        for place in [9, 3, 6] {
            let key = key_of(&format!("{place}.rs"));
            highlights.see(&key, place);
            highlights.ask(&key, 1, Language::Rust, place);
        }

        let order: Vec<PathBuf> = std::iter::from_fn(|| highlights.next_to_start())
            .map(|start| start.key.3)
            .collect();

        assert_eq!(
            order,
            [
                PathBuf::from("3.rs"),
                PathBuf::from("6.rs"),
                PathBuf::from("9.rs")
            ]
        );
    }

    #[test]
    fn a_file_that_left_the_viewport_stops_being_read_and_leaves_the_queue() {
        let mut highlights = Highlights::default();
        start_all(&mut highlights, 6);
        highlights.settle();
        let (gone, kept) = (key_of("0.rs"), key_of("5.rs"));

        highlights.see(&kept, 5);
        highlights.settle();

        assert!(!highlights.is_wanted(&gone));
        assert!(highlights.is_wanted(&kept));
        assert_eq!(highlights.running(), 0);
        assert_eq!(highlights.queued(), 1);
    }

    #[test]
    fn a_file_that_has_its_colors_stays_when_it_leaves_the_viewport() {
        let mut highlights = Highlights::default();
        let key = key_of("0.rs");
        highlights.see(&key, 0);
        highlights.ask(&key, 1, Language::Rust, 0);
        highlights.finish(&key, 1, Some(FileHighlights::default()));

        highlights.settle();
        highlights.settle();

        assert!(highlights.get(&key, 1).is_some());
    }

    #[test]
    fn a_file_is_asked_for_once_per_digest() {
        let key: LoadKey = (
            swarm_protocol::TaskId(1),
            swarm_protocol::ReviewSource::Live,
            "wt".into(),
            "a.rs".into(),
        );
        let mut highlights = Highlights::default();

        assert!(highlights.begin(&key, 1));
        assert!(!highlights.begin(&key, 1));
    }
}

#[cfg(test)]
mod daemon_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{Entity, TestAppContext};
    use swarm_protocol::{
        DiffHunk, DiffLine, DiffLineKind, Event, FileChange, Fold, Request, ReviewSource, TaskDiff,
    };

    use super::*;
    use crate::daemon::tests::{
        default_reply, diff_of, fake_daemon_replying, real_task_workspace, redraw, wait_until,
    };
    use crate::review_view::ReviewView;
    use crate::workspace::View;

    fn changed_file(path: &str, digest: u64, fold: Option<Fold>) -> FileDiff {
        let line = |kind, text: &str| DiffLine {
            kind,
            text: text.to_owned(),
        };

        FileDiff {
            worktree: PathBuf::from("/wt"),
            path: PathBuf::from(path),
            agent: None,
            change: FileChange::Modified,
            mode_change: None,
            additions: 1,
            deletions: 0,
            binary: false,
            hunks: if fold.is_some() {
                Vec::new()
            } else {
                vec![DiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 2,
                    section: String::new(),
                    lines: vec![
                        line(DiffLineKind::Context, "fn main() {"),
                        line(DiffLineKind::Added, "    let s = \"hi\";"),
                    ],
                }]
            },
            truncated: false,
            digest,
            fold,
        }
    }

    fn diff_with(file: FileDiff) -> TaskDiff {
        let mut diff = diff_of(7, ReviewSource::Live, 1);
        diff.files = vec![file];
        diff.stat.files = 1;

        diff
    }

    /// Review on a Live diff, behind a daemon that sends the texts of a Rust file and counts
    /// how often it is asked.
    fn review_of<'a>(
        file: FileDiff,
        asked: &Arc<AtomicUsize>,
        cx: &'a mut TestAppContext,
    ) -> (Entity<ReviewView>, &'a mut gpui::VisualTestContext) {
        let counter = Arc::clone(asked);
        let socket = fake_daemon_replying(move |request| match request {
            Request::FileTexts { call, .. } => {
                counter.fetch_add(1, Ordering::SeqCst);

                Some(Event::FileTexts {
                    call,
                    old: None,
                    new: Some(vec![
                        "fn main() {".to_owned(),
                        "    let s = \"hi\";".to_owned(),
                        "}".to_owned(),
                    ]),
                })
            }
            _ => None,
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);

        workspace.update(cx, |workspace, cx| {
            workspace.data.store_diff(diff_with(file));
            workspace.set_view(View::Review, cx);
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);

        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());

        (review, cx)
    }

    fn colored(review: &Entity<ReviewView>, cx: &gpui::VisualTestContext) -> bool {
        review.read_with(cx, |review, _| review.has_colors(0))
    }

    /// Review of `count` Rust files. Without `connected` the review has no daemon, so the
    /// files it asks for wait in the queue.
    fn many_files_review(
        count: usize,
        connected: bool,
        cx: &mut TestAppContext,
    ) -> (Entity<ReviewView>, &mut gpui::VisualTestContext) {
        let socket = fake_daemon_replying(move |request| match request {
            Request::FileTexts { call, .. } => Some(Event::FileTexts {
                call,
                old: None,
                new: Some(vec!["fn main() {}".to_owned()]),
            }),
            other => default_reply(&other),
        });
        let (workspace, cx, _client) = real_task_workspace(&socket, cx);
        if !connected {
            workspace.update(cx, |workspace, _| workspace.daemon = None);
        }
        let mut diff = diff_of(7, ReviewSource::Live, 1);
        diff.files = (0..count)
            .map(|number| changed_file(&format!("src/file{number}.rs"), 5, None))
            .collect();
        diff.stat.files = u32::try_from(count).unwrap_or(u32::MAX);

        workspace.update(cx, |workspace, cx| {
            workspace.data.store_diff(diff);
            workspace.set_view(View::Review, cx);
            workspace.set_review_source(ReviewSource::Live, cx);
        });
        redraw(cx);

        let review = workspace.read_with(cx, |workspace, _| workspace.review.clone());

        (review, cx)
    }

    #[gpui::test]
    fn no_more_than_four_files_are_read_at_once_and_all_get_their_colors(cx: &mut TestAppContext) {
        let (review, cx) = many_files_review(6, true, cx);

        let all = wait_until(cx, |cx| {
            redraw(cx);
            (0..6).all(|file| review.read_with(cx, |review, _| review.has_colors(file)))
        });

        let peak = review.read_with(cx, |review, _| review.colors_peak_running());
        assert!(all, "a file never got its colors");
        assert_eq!(peak, 4);
    }

    #[gpui::test]
    fn the_files_on_screen_wait_for_a_place_when_four_are_read(cx: &mut TestAppContext) {
        let (review, cx) = many_files_review(6, false, cx);
        redraw(cx);

        let (running, queued) = review.read_with(cx, |review, _| {
            (review.colors_running(), review.colors_queued())
        });

        assert_eq!((running, queued), (0, 6));
    }

    #[gpui::test]
    fn files_that_scroll_out_of_view_stop_being_wanted(cx: &mut TestAppContext) {
        let (review, cx) = many_files_review(60, false, cx);
        redraw(cx);
        let before = review.read_with(cx, |review, _| review.colors_wanted(0));

        review.update(cx, |review, cx| {
            let last = review.body.rows.len().saturating_sub(1);
            review.body_list.scroll_to(gpui::ListOffset {
                item_ix: last,
                offset_in_item: gpui::px(0.),
            });
            cx.notify();
        });
        redraw(cx);
        // Each frame of a scroll redraws the view.
        review.update(cx, |_, cx| cx.notify());
        redraw(cx);

        let after = review.read_with(cx, |review, _| review.colors_wanted(0));
        assert!(before, "the first file was never asked for");
        assert!(
            !after,
            "the first file is still wanted after it left the viewport"
        );
    }

    #[gpui::test]
    fn a_source_file_draws_plain_and_gets_its_colors_from_one_request(cx: &mut TestAppContext) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (review, cx) = review_of(changed_file("src/main.rs", 5, None), &asked, cx);

        let arrived = wait_until(cx, |cx| {
            redraw(cx);
            colored(&review, cx)
        });
        redraw(cx);

        assert!(arrived, "the colors never arrived");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn a_folded_file_is_never_asked_for_its_colors(cx: &mut TestAppContext) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (review, cx) = review_of(changed_file("src/lib.rs", 5, Some(Fold::Large)), &asked, cx);

        redraw(cx);
        redraw(cx);

        assert!(!colored(&review, cx));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn a_file_without_a_grammar_is_never_asked_for_its_colors(cx: &mut TestAppContext) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (_review, cx) = review_of(changed_file("notes.txt", 5, None), &asked, cx);

        redraw(cx);
        redraw(cx);

        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn a_new_digest_asks_again_and_the_old_colors_go(cx: &mut TestAppContext) {
        let asked = Arc::new(AtomicUsize::new(0));
        let (review, cx) = review_of(changed_file("src/main.rs", 5, None), &asked, cx);
        let first = wait_until(cx, |cx| {
            redraw(cx);
            colored(&review, cx)
        });

        let workspace = review.read_with(cx, |review, _| review.workspace.clone());
        workspace
            .update(cx, |workspace, cx| {
                let mut diff = diff_with(changed_file("src/main.rs", 6, None));
                diff.fingerprint = 2;
                workspace.data.store_diff(diff);
                workspace.sync_review(cx);
            })
            .ok();
        redraw(cx);
        let again = wait_until(cx, |cx| {
            redraw(cx);
            asked.load(Ordering::SeqCst) == 2 && colored(&review, cx)
        });

        assert!(first);
        assert!(again, "the second digest never got its colors");
    }
}
