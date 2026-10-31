//! Comments in the Review view: the line cursor, the comment box and the threads.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, Context, Entity, FontWeight, Role, SharedString, Subscription, div, px};
use swarm_protocol::{
    COMMENT_BYTES, CommentAnchor, CommentId, CommentState, DiffLineKind, DiffSide, NewComment,
    ReviewComment, ReviewSource,
};
use swarm_ui::prelude::*;
use swarm_ui::{TextArea, TextFieldSize, ThemeColors};

use super::{
    AgentLabel, DiffStyle, FileKey, NUMBER_WIDTH, ReviewView, SIGN_WIDTH, hunk_line,
    update_workspace,
};
use crate::actions::COMPOSER_CONTEXT;
use crate::markdown;
use crate::model::TaskId;
use crate::review_comments::CommentSlot;
use crate::review_model::{
    ComposerAt, LinePos, Place, Row, ago, anchor_for, find_line, first_changed_line, place_threads,
    refind_line, side_of,
};

const TOO_LONG: &str = "Comment is too long";
const FIRST_WORDS: usize = 8;

/// The line cursor: a line of a file, kept by the file's key so it survives a new diff
/// that leaves the file as it was.
pub(super) struct Cursor {
    pub key: FileKey,
    pub hash: u64,
    pub pos: LinePos,
}

type DraftKey = (TaskId, ReviewSource, FileKey, DiffSide, u32);

/// Tells one comment box from another, even when its line moves.
type BoxId = u64;

/// The line a comment box is open on.
struct Open {
    id: BoxId,
    task: TaskId,
    source: ReviewSource,
    key: FileKey,
    anchor: CommentAnchor,
    daemon_task: swarm_protocol::TaskId,
    error: Option<SharedString>,
}

impl Open {
    fn draft_key(&self) -> DraftKey {
        (
            self.task,
            self.source,
            self.key.clone(),
            self.anchor.side,
            self.anchor.line,
        )
    }
}

/// What a box left behind on its line.
struct Draft {
    id: BoxId,
    text: String,
    error: Option<SharedString>,
}

/// The one comment box, and the unsent text of lines it left.
pub(super) struct Composer {
    pub input: Entity<TextArea>,
    open: Option<Open>,
    drafts: HashMap<DraftKey, Draft>,
    /// Boxes whose comment is on its way to the daemon.
    sending: HashSet<BoxId>,
    next_id: BoxId,
    _subscription: Subscription,
}

impl Composer {
    pub(super) fn new(cx: &mut Context<ReviewView>) -> Self {
        let input = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Comment", cx);
            area.set_rows(3, cx);
            area
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            TextInputEvent::Changed(_) => cx.notify(),
            TextInputEvent::Submitted(_) => this.submit_composer(cx),
        });

        Self {
            input,
            open: None,
            drafts: HashMap::new(),
            sending: HashSet::new(),
            next_id: 1,
            _subscription: subscription,
        }
    }

    fn new_id(&mut self) -> BoxId {
        let id = self.next_id;

        self.next_id += 1;

        id
    }

    fn is_sending(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| self.sending.contains(&open.id))
    }

    #[cfg(test)]
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

/// The reply field of a thread.
pub(super) struct ReplyBox {
    input: Entity<TextInput>,
    /// The reply is on its way to the daemon.
    sending: bool,
    _subscription: Subscription,
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// The first few words of a comment, for a folded thread.
pub(crate) fn first_words(body: &str) -> String {
    let words: Vec<&str> = body.split_whitespace().take(FIRST_WORDS + 1).collect();

    if words.len() > FIRST_WORDS {
        format!("{}\u{2026}", words[..FIRST_WORDS].join(" "))
    } else {
        words.join(" ")
    }
}

/// "Add comment" on Merged, where it waits; "Comment" on Live, where it goes at once.
pub(crate) fn submit_label(source: ReviewSource) -> &'static str {
    match source {
        ReviewSource::Merged => "Add comment",
        ReviewSource::Live => "Comment",
    }
}

/// "→ orchestrator", then the worker once the comment went to one.
fn route_text(comment: &ReviewComment, agents: &[AgentLabel]) -> String {
    let worker = comment
        .to
        .and_then(|id| agents.iter().find(|agent| agent.id == id));

    match worker {
        Some(agent) => format!("\u{2192} orchestrator \u{2192} {}", agent.name),
        None => "\u{2192} orchestrator".to_owned(),
    }
}

fn state_word(state: CommentState) -> &'static str {
    match state {
        CommentState::Pending => "Pending",
        CommentState::Sent => "Sent",
        CommentState::Resolved => "Resolved",
    }
}

fn state_tag(comment: &ReviewComment, now: u64) -> Tag {
    match comment.state {
        CommentState::Pending => Tag::new("Pending").tone(TagTone::Warn),
        CommentState::Sent => {
            let when = ago(comment.sent_at.unwrap_or(comment.created_at), now);
            let text = if when == "now" {
                "Sent now".to_owned()
            } else {
                format!("Sent {when} ago")
            };

            Tag::new(text).tone(TagTone::Accent)
        }
        CommentState::Resolved => Tag::new("Resolved"),
    }
}

impl ReviewView {
    /// Works out where the comments of the task show, after the files or the comments
    /// changed.
    pub(super) fn place_comments(&mut self) {
        let Some(input) = self.input.clone() else {
            return;
        };
        let Some(diff) = input.diff.as_ref() else {
            self.threads = Rc::from([]);
            self.open_counts = Rc::from([]);
            return;
        };
        let comments = Rc::clone(&input.comments);
        let threads = place_threads(diff, &comments);

        let mut counts = vec![0_u32; diff.files.len()];
        for thread in &threads {
            let open = comments
                .get(thread.root)
                .is_some_and(|root| root.state != CommentState::Resolved);

            if let (true, Some(count)) = (open, counts.get_mut(thread.file)) {
                *count += 1;
            }
        }

        let mut replies: HashMap<CommentId, Vec<usize>> = HashMap::new();
        for (index, comment) in comments.iter().enumerate() {
            if let Some(parent) = comment.parent {
                replies.entry(parent).or_default().push(index);
            }
        }

        self.parsed
            .retain(|id, _| comments.iter().any(|comment| comment.id == *id));
        for comment in comments.iter() {
            self.parsed
                .entry(comment.id)
                .or_insert_with(|| Rc::new(markdown::parse(&comment.body)));
        }

        self.reply_boxes
            .retain(|id, _| comments.iter().any(|comment| comment.id == *id));
        self.card_errors
            .retain(|id, _| comments.iter().any(|comment| comment.id == *id));
        self.unfolded
            .retain(|id| comments.iter().any(|comment| comment.id == *id));

        self.threads = threads.into();
        self.open_counts = counts.into();
        self.replies_of = Rc::new(replies);
        self.resolve_cursor();
    }

    /// Keeps the cursor on its line when the file is as it was, else drops it.
    fn resolve_cursor(&mut self) {
        let Some(cursor) = self.cursor.take() else {
            return;
        };
        let found = self
            .infos
            .iter()
            .position(|info| info.key == cursor.key && info.hash == cursor.hash);

        self.cursor = found.map(|file| Cursor {
            pos: LinePos {
                file: u32::try_from(file).unwrap_or(u32::MAX),
                ..cursor.pos
            },
            ..cursor
        });
    }

    #[cfg(test)]
    pub(crate) fn cursor_pos(&self) -> Option<LinePos> {
        self.cursor.as_ref().map(|cursor| cursor.pos)
    }

    #[cfg(test)]
    pub(crate) fn composer_is_open(&self) -> bool {
        self.composer.is_open()
    }

    #[cfg(test)]
    pub(crate) fn composer_text(&self, cx: &App) -> SharedString {
        self.composer.input.read(cx).text()
    }

    #[cfg(test)]
    pub(crate) fn composer_input(&self) -> &Entity<TextArea> {
        &self.composer.input
    }

    #[cfg(test)]
    pub(crate) fn composer_error(&self) -> Option<SharedString> {
        self.composer.open.as_ref()?.error.clone()
    }

    #[cfg(test)]
    pub(crate) fn thread_places(&self) -> Vec<(usize, Place, DiffSide)> {
        self.threads
            .iter()
            .map(|thread| (thread.root, thread.place, thread.side))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn reply_for_test(&mut self, text: &str, root: CommentId, cx: &mut Context<Self>) {
        let input = self.reply_input(root, cx);

        input.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
        self.submit_reply(root, cx);
    }

    #[cfg(test)]
    pub(crate) fn reply_sending(&self, root: CommentId) -> bool {
        self.reply_boxes
            .get(&root)
            .is_some_and(|reply| reply.sending)
    }

    #[cfg(test)]
    pub(crate) fn reply_input_for_test(
        &mut self,
        root: CommentId,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        self.reply_input(root, cx)
    }

    #[cfg(test)]
    pub(crate) fn card_error(&self, id: CommentId) -> Option<SharedString> {
        self.card_errors.get(&id).cloned()
    }

    #[cfg(test)]
    pub(crate) fn composer_sending(&self) -> bool {
        self.composer.is_sending()
    }

    #[cfg(test)]
    pub(crate) fn composer_line(&self) -> Option<u32> {
        self.composer.open.as_ref().map(|open| open.anchor.line)
    }

    #[cfg(test)]
    pub(crate) fn draft_text(&self, pos: LinePos) -> Option<String> {
        let input = self.input.as_ref()?;
        let file = input.diff.as_ref()?.files.get(pos.file as usize)?;
        let anchor = anchor_for(file, pos.hunk as usize, pos.line as usize, pos.side)?;
        let key = (
            input.task,
            input.source,
            (file.worktree.clone(), file.path.clone()),
            anchor.side,
            anchor.line,
        );

        self.composer
            .drafts
            .get(&key)
            .map(|draft| draft.text.clone())
    }

    #[cfg(test)]
    pub(crate) fn submit_composer_for_test(&mut self, cx: &mut Context<Self>) {
        self.submit_composer(cx);
    }

    #[cfg(test)]
    pub(crate) fn is_folded(&self, id: CommentId) -> bool {
        self.input.as_ref().is_some_and(|input| {
            input
                .comments
                .iter()
                .any(|comment| comment.id == id && comment.state == CommentState::Resolved)
        }) && !self.unfolded.contains(&id)
    }

    #[cfg(test)]
    pub(crate) fn open_count(&self, file: usize) -> u32 {
        self.open_counts.get(file).copied().unwrap_or(0)
    }

    /// Where the open comment box sits in the current diff: under its line, else at the
    /// top of its file card. `None` when its file is not in the diff.
    pub(super) fn composer_at(&self) -> Option<ComposerAt> {
        let open = self.composer.open.as_ref()?;
        let input = self.input.as_ref()?;

        if open.task != input.task || open.source != input.source {
            return None;
        }

        let diff = input.diff.as_ref()?;
        let file = diff
            .files
            .iter()
            .position(|file| (file.worktree.clone(), file.path.clone()) == open.key)?;
        let data = diff.files.get(file)?;

        let under = find_line(data, open.anchor.side, open.anchor.line).filter(|(hunk, line)| {
            data.hunks
                .get(*hunk as usize)
                .and_then(|hunk| hunk.lines.get(*line as usize))
                .is_some_and(|found| found.text == open.anchor.text)
        });

        Some(match under {
            Some((hunk, line)) => ComposerAt::Line(LinePos {
                file: u32::try_from(file).unwrap_or(u32::MAX),
                hunk,
                line,
                side: open.anchor.side,
            }),
            None => ComposerAt::Top { file },
        })
    }

    /// Moves the open comment box to where its line is in a new diff, so it stays put
    /// under the same code. A line that is gone keeps its number: the box then shows at
    /// the top of the card.
    pub(super) fn follow_composer(&mut self) {
        let Some(input) = self.input.as_ref() else {
            return;
        };
        let Some(open) = self.composer.open.as_mut() else {
            return;
        };
        let Some(diff) = input.diff.as_ref() else {
            return;
        };

        if open.task != input.task || open.source != input.source {
            return;
        }

        let file = diff
            .files
            .iter()
            .find(|file| (file.worktree.clone(), file.path.clone()) == open.key);

        if let Some(line) = file.and_then(|file| refind_line(file, &open.anchor)) {
            open.anchor.line = line;
        }
    }

    /// The commentable lines on screen, in order, as `(row, position)`.
    fn line_positions(&self) -> Vec<(usize, LinePos)> {
        let Some(diff) = self.diff() else {
            return Vec::new();
        };
        let mut found = Vec::new();

        for (index, row) in self.body.rows.iter().enumerate() {
            match *row {
                Row::Line {
                    file, hunk, line, ..
                } => {
                    let side =
                        hunk_line(diff, file, hunk, line).and_then(|data| side_of(data.kind));

                    if let Some(side) = side {
                        found.push((
                            index,
                            LinePos {
                                file,
                                hunk,
                                line,
                                side,
                            },
                        ));
                    }
                }
                Row::Pair {
                    file,
                    hunk,
                    left,
                    right,
                } => {
                    for (side, half) in [(DiffSide::Old, left), (DiffSide::New, right)] {
                        let Some(half) = half else {
                            continue;
                        };
                        let real = hunk_line(diff, file, hunk, half.line)
                            .is_some_and(|data| data.kind != DiffLineKind::NoNewline);

                        if real {
                            found.push((
                                index,
                                LinePos {
                                    file,
                                    hunk,
                                    line: half.line,
                                    side,
                                },
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        found
    }

    /// The first changed line of the selected file that shows.
    fn first_line_of_selection(&self) -> Option<LinePos> {
        let selected = self.selected_file()?;
        let file = self.diff()?.files.get(selected)?;
        let at = first_changed_line(file, selected)?;

        self.line_positions()
            .iter()
            .any(|(_, pos)| *pos == at)
            .then_some(at)
    }

    /// Puts the cursor on a line.
    pub(super) fn set_cursor(&mut self, pos: LinePos, cx: &mut Context<Self>) {
        let Some(info) = self.infos.get(pos.file as usize) else {
            return;
        };

        self.cursor = Some(Cursor {
            key: info.key.clone(),
            hash: info.hash,
            pos,
        });
        cx.notify();
    }

    /// Moves the cursor to the next or previous commentable line. Without a cursor it
    /// goes to the first changed line of the selected file.
    pub(super) fn move_cursor(&mut self, step: isize, cx: &mut Context<Self>) {
        let all = self.line_positions();
        let current = self.cursor.as_ref().map(|cursor| cursor.pos);
        let side = current.map(|pos| pos.side);

        let candidates: Vec<(usize, LinePos)> = all
            .into_iter()
            .filter(|(_, pos)| {
                self.style == DiffStyle::Unified || side.is_none_or(|side| pos.side == side)
            })
            .collect();
        if candidates.is_empty() {
            return;
        }

        let at = current.and_then(|current| {
            candidates.iter().position(|(_, pos)| {
                pos.file == current.file && pos.hunk == current.hunk && pos.line == current.line
            })
        });

        let target = match at {
            Some(at) => candidates.get(at.saturating_add_signed(step).min(candidates.len() - 1)),
            None => self
                .first_line_of_selection()
                .and_then(|first| candidates.iter().find(|(_, pos)| *pos == first))
                .or_else(|| candidates.first()),
        };

        let Some((row, pos)) = target.copied() else {
            return;
        };

        self.set_cursor(pos, cx);
        self.body_list.scroll_to_reveal_item(row);
    }

    /// Opens the comment box on the cursor, else on the first changed line of the
    /// selected file.
    pub(crate) fn comment_on_line(&mut self, cx: &mut Context<Self>) {
        let pos = self
            .cursor
            .as_ref()
            .map(|cursor| cursor.pos)
            .or_else(|| self.first_line_of_selection());

        if let Some(pos) = pos {
            self.open_composer(pos, cx);
        }
    }

    /// Opens the comment box under a line. A draft left on that line comes back; the
    /// text of the box it replaces is kept as a draft of its own line.
    pub(crate) fn open_composer(&mut self, pos: LinePos, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        let Some(diff) = input.diff.as_ref() else {
            return;
        };
        let Some(file) = diff.files.get(pos.file as usize) else {
            return;
        };
        let Some(anchor) = anchor_for(file, pos.hunk as usize, pos.line as usize, pos.side) else {
            return;
        };

        self.park_draft(cx);

        let key: FileKey = (file.worktree.clone(), file.path.clone());
        let draft_key = (
            input.task,
            input.source,
            key.clone(),
            anchor.side,
            anchor.line,
        );
        let draft = self.composer.drafts.remove(&draft_key);

        let open = Open {
            id: draft
                .as_ref()
                .map_or_else(|| self.composer.new_id(), |draft| draft.id),
            task: input.task,
            source: input.source,
            key,
            anchor,
            daemon_task: diff.task,
            error: draft.as_ref().and_then(|draft| draft.error.clone()),
        };
        let text = draft.map(|draft| draft.text).unwrap_or_default();

        self.composer
            .input
            .update(cx, |area, cx| area.set_text(text, cx));
        self.composer.open = Some(open);
        self.set_cursor(pos, cx);
        self.focus_composer = true;

        self.after_card_change();
        let composer_row = self
            .body
            .rows
            .iter()
            .position(|row| matches!(row, Row::Composer { .. }));
        if let Some(row) = composer_row {
            self.body_list.scroll_to_reveal_item(row);
        }

        cx.notify();
    }

    /// Keeps the open box as the draft of its line. A box whose comment is on its way
    /// is kept too, so its answer finds it.
    fn park_draft(&mut self, cx: &Context<Self>) {
        let Some(open) = &self.composer.open else {
            return;
        };
        let text = self.composer.input.read(cx).text().to_string();
        let sending = self.composer.sending.contains(&open.id);

        if text.trim().is_empty() && !sending {
            self.composer.drafts.remove(&open.draft_key());
        } else {
            self.composer.drafts.insert(
                open.draft_key(),
                Draft {
                    id: open.id,
                    text,
                    error: open.error.clone(),
                },
            );
        }
    }

    /// Closes the comment box and drops its draft.
    pub(crate) fn cancel_composer(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.composer.open.take() else {
            return;
        };

        self.composer.drafts.remove(&open.draft_key());
        self.composer
            .input
            .update(cx, |area, cx| area.set_text("", cx));
        self.focus_body = true;

        self.after_card_change();
        cx.notify();
    }

    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.input.read(cx).text();
        let body = text.trim().to_owned();
        let composer = &mut self.composer;
        let Some(open) = composer.open.as_mut() else {
            return;
        };

        if composer.sending.contains(&open.id) || body.is_empty() {
            return;
        }
        if body.len() > COMMENT_BYTES {
            open.error = Some(TOO_LONG.into());
            cx.notify();
            return;
        }

        open.error = None;
        let slot = CommentSlot::Composer(open.id);
        let comment = NewComment {
            task: open.daemon_task,
            parent: None,
            source: open.source,
            anchor: open.anchor.clone(),
            body,
        };
        composer.sending.insert(open.id);

        update_workspace(&self.workspace, cx, |workspace, cx| {
            workspace.add_review_comment(comment, slot, cx);
        });
        cx.notify();
    }

    /// What the daemon answered to an add, delete or resolve.
    pub(crate) fn comment_result(
        &mut self,
        slot: CommentSlot,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        match slot {
            CommentSlot::Composer(id) => self.composer_answer(id, result, cx),
            CommentSlot::Reply(id) => self.reply_answer(id, result, cx),
            CommentSlot::Card(id) => match result {
                Ok(()) => {
                    self.card_errors.remove(&id);
                }
                Err(message) => {
                    self.card_errors.insert(id, message);
                }
            },
        }

        cx.notify();
    }

    /// Applies an answer to the box it was sent from, which may have left the screen.
    fn composer_answer(
        &mut self,
        id: BoxId,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.composer.sending.remove(&id);
        let shown = self
            .composer
            .open
            .as_ref()
            .is_some_and(|open| open.id == id);

        match result {
            Ok(()) if shown => {
                self.composer.open = None;
                self.composer
                    .input
                    .update(cx, |area, cx| area.set_text("", cx));
                self.focus_body = true;
                self.after_card_change();
            }
            Ok(()) => self.composer.drafts.retain(|_, draft| draft.id != id),
            Err(message) if shown => {
                if let Some(open) = self.composer.open.as_mut() {
                    open.error = Some(message);
                }
            }
            Err(message) => {
                if let Some(draft) = self
                    .composer
                    .drafts
                    .values_mut()
                    .find(|draft| draft.id == id)
                {
                    draft.error = Some(message);
                }
            }
        }
    }

    /// Applies an answer to a reply: the field clears on success and opens again on
    /// failure.
    fn reply_answer(
        &mut self,
        root: CommentId,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        if let Some(reply) = self.reply_boxes.get_mut(&root) {
            reply.sending = false;
            reply.input.update(cx, |input, cx| {
                input.set_disabled(false, cx);

                if result.is_ok() {
                    input.set_text("", cx);
                }
            });
        }

        match result {
            Ok(()) => {
                self.card_errors.remove(&root);
            }
            Err(message) => {
                self.card_errors.insert(root, message);
            }
        }
    }

    /// Forgets the draft and the box of a task that closed.
    pub(crate) fn forget_task_comments(&mut self, task: TaskId, cx: &mut Context<Self>) {
        self.composer.drafts.retain(|key, _| key.0 != task);

        if self
            .composer
            .open
            .as_ref()
            .is_some_and(|open| open.task == task)
        {
            self.composer.open = None;
            self.composer
                .input
                .update(cx, |area, cx| area.set_text("", cx));
        }
    }

    fn reply_input(&mut self, root: CommentId, cx: &mut Context<Self>) -> Entity<TextInput> {
        let reply = self.reply_boxes.entry(root).or_insert_with(|| {
            let input = cx.new(|cx| {
                let mut input = TextInput::new(cx);
                input.set_label("Reply", cx);
                input.set_placeholder("Reply", cx);
                input.set_size(TextFieldSize::Small, cx);
                input.set_bordered(false, cx);
                input
            });
            let subscription = cx.subscribe(&input, move |this, _, event, cx| {
                if matches!(event, TextInputEvent::Submitted(_)) {
                    this.submit_reply(root, cx);
                }
            });

            ReplyBox {
                input,
                sending: false,
                _subscription: subscription,
            }
        });

        reply.input.clone()
    }

    /// Sends the text of a thread's reply field as a reply.
    pub(crate) fn submit_reply(&mut self, root: CommentId, cx: &mut Context<Self>) {
        let Some(reply) = self.reply_boxes.get_mut(&root) else {
            return;
        };
        let body = reply.input.read(cx).text().trim().to_owned();
        let parent = self
            .input
            .as_ref()
            .and_then(|input| input.comments.iter().find(|comment| comment.id == root))
            .cloned();
        let Some(parent) = parent else {
            return;
        };

        if reply.sending || body.is_empty() {
            return;
        }
        if body.len() > COMMENT_BYTES {
            self.card_errors.insert(root, TOO_LONG.into());
            cx.notify();
            return;
        }

        self.card_errors.remove(&root);
        reply.sending = true;
        reply
            .input
            .update(cx, |input, cx| input.set_disabled(true, cx));

        let comment = NewComment {
            task: parent.task,
            parent: Some(parent.id),
            source: parent.source,
            anchor: parent.anchor,
            body,
        };

        update_workspace(&self.workspace, cx, |workspace, cx| {
            workspace.add_review_comment(comment, CommentSlot::Reply(root), cx);
        });
        cx.notify();
    }

    /// The indent of a comment row: under the gutter, and in the column of its side
    /// in Split.
    fn indented(&self, side: DiffSide, card: AnyElement) -> AnyElement {
        let gutter = NUMBER_WIDTH * 2. + SIGN_WIDTH;

        match self.style {
            DiffStyle::Unified => div()
                .w_full()
                .pl(px(gutter))
                .pr(px(12.))
                .py(px(6.))
                .child(card)
                .into_any_element(),
            DiffStyle::Split => {
                let column = div()
                    .w_1_2()
                    .min_w_0()
                    .pl(px(NUMBER_WIDTH + SIGN_WIDTH))
                    .pr(px(12.))
                    .py(px(6.))
                    .child(card);
                let empty = div().w_1_2();

                let row = div().flex().w_full();
                match side {
                    DiffSide::Old => row.child(column).child(empty),
                    DiffSide::New => row.child(empty).child(column),
                }
                .into_any_element()
            }
        }
    }

    pub(super) fn thread_row(
        &mut self,
        thread: usize,
        index: usize,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(placed) = self.threads.get(thread).copied() else {
            return div().into_any_element();
        };
        let Some(input) = self.input.clone() else {
            return div().into_any_element();
        };
        let Some(root) = input.comments.get(placed.root).cloned() else {
            return div().into_any_element();
        };
        let name = self
            .infos
            .get(placed.file)
            .map_or_else(SharedString::default, |info| info.name.clone());
        let now = input.fixed_now.unwrap_or_else(system_now);
        let folded = root.state == CommentState::Resolved && !self.unfolded.contains(&root.id);

        let card = if folded {
            Self::folded_thread(&root, &name, index, colors, cx)
        } else {
            self.open_thread(&root, placed.place == Place::Top, &name, now, colors, cx)
        };

        self.indented(placed.side, card)
    }

    fn folded_thread(
        root: &ReviewComment,
        name: &SharedString,
        index: usize,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = root.id;
        let view = cx.entity();
        let (line, raised, active, accent) =
            (colors.line, colors.raised, colors.active, colors.accent);

        h_flex()
            .id(("comment-folded", index))
            .debug_selector(|| format!("comment-folded-{}", root.id.0))
            .role(Role::Button)
            .aria_label(format!(
                "Resolved comment on {name} line {}: {}",
                root.anchor.line,
                first_words(&root.body)
            ))
            .aria_expanded(false)
            .focusable()
            .tab_stop(true)
            .cursor_pointer()
            .h(px(30.))
            .px(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .hover(move |style| style.bg(active))
            .focus_visible(move |style| swarm_ui::styles::focus_ring(style, accent))
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| {
                    this.unfolded.insert(id);
                    this.after_card_change();
                    cx.notify();
                });
            })
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                div().flex_1().min_w_0().flex().child(
                    Label::new(format!("Resolved \u{b7} {}", first_words(&root.body)))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                ),
            )
            .into_any_element()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat element tree reads best in one place"
    )]
    fn open_thread(
        &mut self,
        root: &ReviewComment,
        at_top: bool,
        name: &SharedString,
        now: u64,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = root.id;
        let numeric = usize::try_from(id.0).unwrap_or(usize::MAX);
        let resolved = root.state == CommentState::Resolved;
        let view = cx.entity();
        let reply_input = self.reply_input(id, cx);
        let replies: Vec<ReviewComment> = self
            .replies_of
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|index| self.input.as_ref()?.comments.get(*index).cloned())
            .collect();
        let error = self.card_errors.get(&id).cloned();
        let input_agents = self
            .input
            .as_ref()
            .map_or_else(|| Rc::from([]), |input| Rc::clone(&input.agents));
        let (line, raised) = (colors.line, colors.raised);

        let header = h_flex()
            .gap(px(8.))
            .child(
                Label::new("You")
                    .size(LabelSize::Small)
                    .weight(FontWeight::SEMIBOLD),
            )
            .child(
                Label::new(format!("line {}", root.anchor.line))
                    .size(LabelSize::Small)
                    .color(Color::Faint),
            )
            .child(state_tag(root, now))
            .when(root.outdated, |header| {
                header.child(Tag::new("Outdated").tone(TagTone::Warn))
            })
            .child(div().flex_1())
            .child(
                div().min_w_0().flex().child(
                    Label::new(route_text(root, &input_agents))
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint)
                        .truncate(),
                ),
            )
            .when(resolved, |header| {
                let fold_view = view.clone();

                header.child(
                    IconButton::new(
                        ("comment-fold", numeric),
                        IconName::ChevronDown,
                        "Fold thread",
                    )
                    .color(Color::Muted)
                    .on_click(move |_, _, cx| {
                        fold_view.update(cx, |this, cx| {
                            this.unfolded.remove(&id);
                            this.after_card_change();
                            cx.notify();
                        });
                    }),
                )
            });

        let quote = at_top.then(|| {
            div()
                .border_l_2()
                .border_color(line)
                .pl(px(8.))
                .min_w_0()
                .flex()
                .child(
                    Label::new(super::display_text(&root.anchor.text))
                        .mono()
                        .size(LabelSize::XSmall)
                        .color(Color::Faint)
                        .truncate(),
                )
        });

        let body = |comment: &ReviewComment| {
            self.parsed.get(&comment.id).map(|parsed| {
                div()
                    .id((
                        "comment-body",
                        usize::try_from(comment.id.0).unwrap_or(usize::MAX),
                    ))
                    .child(markdown::render(parsed, false, colors))
            })
        };

        let reply_rows: Vec<AnyElement> = replies
            .iter()
            .map(|reply| {
                v_flex()
                    .gap(px(4.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(line)
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                Label::new("You")
                                    .size(LabelSize::Small)
                                    .weight(FontWeight::SEMIBOLD),
                            )
                            .child(state_tag(reply, now)),
                    )
                    .children(body(reply))
                    .into_any_element()
            })
            .collect();

        let workspace = self.workspace.clone();
        let task = root.task;
        let action = if root.state == CommentState::Pending {
            Button::new(("comment-delete", numeric), "Delete")
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, _, cx| {
                    update_workspace(&workspace, cx, |workspace, cx| {
                        workspace.delete_review_comment(task, id, cx);
                    });
                })
        } else {
            Button::new(
                ("comment-resolve", numeric),
                if resolved { "Reopen" } else { "Resolve" },
            )
            .variant(ButtonVariant::Secondary)
            .on_click(move |_, _, cx| {
                update_workspace(&workspace, cx, |workspace, cx| {
                    workspace.resolve_review_comment(task, id, !resolved, cx);
                });
            })
        };

        let label = format!(
            "Comment by You on {name} line {}, {}{}",
            root.anchor.line,
            state_word(root.state),
            if root.outdated { ", outdated" } else { "" }
        );

        v_flex()
            .id(("comment-thread", numeric))
            .debug_selector(|| format!("comment-thread-{numeric}"))
            .role(Role::Group)
            .aria_label(label)
            .w_full()
            .min_w_0()
            .rounded(px(8.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .child(
                v_flex()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(line)
                    .child(header)
                    .children(quote)
                    .children(body(root))
                    .children(reply_rows),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(6.))
                    .child(div().flex_1().min_w_0().child(reply_input))
                    .child(action),
            )
            .children(error.map(|message| {
                div()
                    .id(("comment-error", numeric))
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .px(px(12.))
                    .pb(px(8.))
                    .child(Label::new(message).size(LabelSize::Small).color(Color::Del))
            }))
            .into_any_element()
    }

    pub(super) fn composer_row(
        &mut self,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let outdated = matches!(self.composer_at(), Some(ComposerAt::Top { .. }));
        let sending = self.composer.is_sending();
        let Some(open) = self.composer.open.as_ref() else {
            return div().into_any_element();
        };
        let open_source = open.source;
        let side = open.anchor.side;
        let error = open.error.clone();
        let line_number = open.anchor.line;
        let quoted = outdated.then(|| super::display_text(&open.anchor.text));
        let file_name = open
            .key
            .1
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let empty = self.composer.input.read(cx).text().trim().is_empty();
        let (line, raised) = (colors.line, colors.raised);
        let view = cx.entity();
        let submit_view = view.clone();

        let card = v_flex()
            .id("comment-composer")
            .debug_selector(|| "comment-composer".to_owned())
            .key_context(COMPOSER_CONTEXT)
            .role(Role::Group)
            .aria_label(format!(
                "New comment on {file_name} line {line_number}{}",
                if outdated { ", outdated" } else { "" }
            ))
            .w_full()
            .min_w_0()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(line)
            .bg(raised)
            .children(quoted.map(|text| {
                v_flex()
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .id("comment-composer-outdated")
                            .debug_selector(|| "comment-composer-outdated".to_owned())
                            .gap(px(8.))
                            .child(
                                Label::new(format!("line {line_number}"))
                                    .size(LabelSize::Small)
                                    .color(Color::Faint),
                            )
                            .child(Tag::new("Outdated").tone(TagTone::Warn)),
                    )
                    .child(
                        div()
                            .border_l_2()
                            .border_color(line)
                            .pl(px(8.))
                            .min_w_0()
                            .flex()
                            .child(
                                Label::new(text)
                                    .mono()
                                    .size(LabelSize::XSmall)
                                    .color(Color::Faint)
                                    .truncate(),
                            ),
                    )
            }))
            .child(self.composer.input.clone())
            .children(error.map(|message| {
                div()
                    .id("comment-composer-error")
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .child(Label::new(message).size(LabelSize::Small).color(Color::Del))
            }))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        Button::new("comment-cancel", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .debug_selector(|| "comment-cancel".to_owned())
                            .on_click(move |_, _, cx| {
                                view.update(cx, ReviewView::cancel_composer);
                            }),
                    )
                    .child(
                        Button::new("comment-submit", submit_label(open_source))
                            .variant(ButtonVariant::Primary)
                            .debug_selector(|| "comment-submit".to_owned())
                            .disabled(empty || sending)
                            .on_click(move |_, _, cx| {
                                submit_view.update(cx, ReviewView::submit_composer);
                            }),
                    ),
            )
            .into_any_element();

        self.indented(side, card)
    }
}

#[cfg(test)]
mod tests {
    use super::first_words;

    #[test]
    fn a_folded_thread_shows_the_first_words() {
        assert_eq!(first_words("Rename it"), "Rename it");
        assert_eq!(
            first_words("one two three four five six seven eight nine ten"),
            "one two three four five six seven eight\u{2026}"
        );
    }
}
