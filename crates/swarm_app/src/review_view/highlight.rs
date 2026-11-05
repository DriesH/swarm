//! Syntax colors in the diff: the whole old and new text of a file is highlighted once on
//! a background thread, and the lines of the diff look their colors up by line number.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use gpui::{AppContext as _, Context, HighlightStyle, Hsla};
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

/// Where the colors of a file stand.
enum State {
    /// Asked for; the lines draw plain until they arrive.
    Pending,
    Ready(Arc<FileHighlights>),
    /// The texts could not be read.
    Plain,
}

/// The colors of the files of the diff, each for the digest it was made for.
#[derive(Default)]
pub(super) struct Highlights {
    files: HashMap<LoadKey, (u64, State)>,
}

impl Highlights {
    pub(super) fn clear(&mut self) {
        self.files.clear();
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

        self.files.insert(key.clone(), (digest, State::Pending));

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

        *state = match highlights {
            Some(highlights) => State::Ready(Arc::new(highlights)),
            None => State::Plain,
        };
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

        let client = self.client.clone().filter(Client::is_connected);
        if let (Some(language), Some(client)) = (language_of(data), client)
            && self.highlights.begin(&key, data.digest)
        {
            Self::read_colors(key, data.digest, language, client, cx);
        }

        None
    }

    /// Reads both sides of the file and colors them on the background executor.
    fn read_colors(
        key: LoadKey,
        digest: u64,
        language: Language,
        client: Client,
        cx: &mut Context<Self>,
    ) {
        let reading = {
            let key = key.clone();

            cx.background_spawn(async move {
                let texts = client.file_texts(key.0, key.1, key.2, key.3)?;

                Ok::<_, swarm_protocol::ClientError>(FileHighlights::compute(language, &texts))
            })
        };

        cx.spawn(async move |this, cx| {
            let answer = reading.await;
            if let Err(error) = &answer {
                log::warn!("cannot read a file to color it: {error}");
            }

            let updated = this.update(cx, |this, cx| {
                this.highlights.finish(&key, digest, answer.ok());
                cx.notify();
            });
            if let Err(error) = updated {
                log::debug!("the window closed before the colors arrived: {error}");
            }
        })
        .detach();
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
        diff_of, fake_daemon_replying, real_task_workspace, redraw, wait_until,
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
