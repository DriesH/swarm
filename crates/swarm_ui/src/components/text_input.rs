// Text layout, IME and editing structure adapted from GPUI's `examples/input.rs`
// (Zed Industries, Apache-2.0).

use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Div, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    GlobalElementId, Hsla, InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Role, ScrollEvent, SharedString,
    Stateful, Style, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::component::{Component, example, example_group};
use crate::fonts::{MONO_FONT, UI_FONT};
use crate::motion::reduce_motion;
use crate::prelude::*;
use crate::styles::focus_ring;

mod action {
    gpui::actions!(
        text_input,
        [
            Left,
            Right,
            Up,
            Down,
            WordLeft,
            WordRight,
            LineStart,
            LineEnd,
            DocumentStart,
            DocumentEnd,
            SelectLeft,
            SelectRight,
            SelectUp,
            SelectDown,
            SelectWordLeft,
            SelectWordRight,
            SelectToLineStart,
            SelectToLineEnd,
            SelectToDocumentStart,
            SelectToDocumentEnd,
            SelectAll,
            Backspace,
            Delete,
            DeleteWordLeft,
            DeleteWordRight,
            DeleteToLineStart,
            Newline,
            Submit,
            Undo,
            Redo,
            Copy,
            Cut,
            Paste,
            ShowCharacterPalette,
        ]
    );
}

/// The history and clipboard actions of a text field, for the Edit menu to send.
pub mod edit {
    pub use super::action::{Copy, Cut, Paste, Redo, SelectAll, Undo};
}

const TEXT_INPUT_CONTEXT: &str = "TextInput";
const TEXT_AREA_CONTEXT: &str = "TextArea";

const LINE_HEIGHT: Pixels = px(18.);
const CURSOR_WIDTH: Pixels = px(1.5);
const BLINK_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_ROWS: usize = 3;
const UNDO_LIMIT: usize = 100;

/// Binds the editing keys in the `TextInput` and `TextArea` key contexts.
pub(crate) fn init(cx: &mut App) {
    let mut bindings = Vec::new();

    for context in [TEXT_INPUT_CONTEXT, TEXT_AREA_CONTEXT] {
        let context = Some(context);
        bindings.extend([
            KeyBinding::new("left", action::Left, context),
            KeyBinding::new("right", action::Right, context),
            KeyBinding::new("alt-left", action::WordLeft, context),
            KeyBinding::new("alt-right", action::WordRight, context),
            KeyBinding::new("cmd-left", action::LineStart, context),
            KeyBinding::new("cmd-right", action::LineEnd, context),
            KeyBinding::new("home", action::LineStart, context),
            KeyBinding::new("end", action::LineEnd, context),
            KeyBinding::new("shift-left", action::SelectLeft, context),
            KeyBinding::new("shift-right", action::SelectRight, context),
            KeyBinding::new("alt-shift-left", action::SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", action::SelectWordRight, context),
            KeyBinding::new("cmd-shift-left", action::SelectToLineStart, context),
            KeyBinding::new("cmd-shift-right", action::SelectToLineEnd, context),
            KeyBinding::new("shift-home", action::SelectToLineStart, context),
            KeyBinding::new("shift-end", action::SelectToLineEnd, context),
            KeyBinding::new("cmd-a", action::SelectAll, context),
            KeyBinding::new("backspace", action::Backspace, context),
            KeyBinding::new("shift-backspace", action::Backspace, context),
            KeyBinding::new("delete", action::Delete, context),
            KeyBinding::new("alt-backspace", action::DeleteWordLeft, context),
            KeyBinding::new("alt-delete", action::DeleteWordRight, context),
            KeyBinding::new("cmd-backspace", action::DeleteToLineStart, context),
            KeyBinding::new("cmd-enter", action::Submit, context),
            KeyBinding::new("cmd-z", action::Undo, context),
            KeyBinding::new("cmd-shift-z", action::Redo, context),
            KeyBinding::new("cmd-c", action::Copy, context),
            KeyBinding::new("cmd-x", action::Cut, context),
            KeyBinding::new("cmd-v", action::Paste, context),
            KeyBinding::new("ctrl-cmd-space", action::ShowCharacterPalette, context),
        ]);
    }

    // Up and Down stay unbound in a TextInput, so a menu or list around it gets them.
    let area = Some(TEXT_AREA_CONTEXT);
    bindings.extend([
        KeyBinding::new("enter", action::Submit, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("enter", action::Newline, area),
        KeyBinding::new("shift-enter", action::Newline, area),
        KeyBinding::new("up", action::Up, area),
        KeyBinding::new("down", action::Down, area),
        KeyBinding::new("shift-up", action::SelectUp, area),
        KeyBinding::new("shift-down", action::SelectDown, area),
        KeyBinding::new("cmd-up", action::DocumentStart, area),
        KeyBinding::new("cmd-down", action::DocumentEnd, area),
        KeyBinding::new("cmd-shift-up", action::SelectToDocumentStart, area),
        KeyBinding::new("cmd-shift-down", action::SelectToDocumentEnd, area),
    ]);

    cx.bind_keys(bindings);
}

/// What a text field reports to its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    /// The user changed the text.
    Changed(SharedString),
    /// Enter in a `TextInput`, Cmd-Enter in a `TextArea`.
    Submitted(SharedString),
}

/// The height and text size of a text field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextFieldSize {
    /// 28px tall, 12px text.
    Small,
    /// 34px tall, 13px text.
    #[default]
    Medium,
    /// 40px tall, 13px text.
    Large,
}

impl TextFieldSize {
    fn height(self) -> Pixels {
        match self {
            Self::Small => px(28.),
            Self::Medium => px(34.),
            Self::Large => px(40.),
        }
    }

    fn text_size(self, mono: bool) -> Pixels {
        match (self, mono) {
            (Self::Small, _) => px(12.),
            (_, true) => px(12.5),
            (_, false) => px(13.),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chrome {
    Bordered,
    Plain,
}

/// How a focused text field shows focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusStyle {
    /// A 2px accent ring.
    #[default]
    Ring,
    /// A 1px accent border and no ring.
    Border,
}

/// What a text field tells assistive technology it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextFieldRole {
    #[default]
    Text,
    /// An editable field that controls a popup list.
    ComboBox,
}

/// A one-line text field. Create it with `cx.new(|cx| TextInput::new(cx))`.
pub type TextInput = TextField<false>;

/// A text field that wraps lines and scrolls vertically.
/// Create it with `cx.new(|cx| TextArea::new(cx))`.
pub type TextArea = TextField<true>;

/// The shared editor behind [`TextInput`] and [`TextArea`].
/// Set a label: VoiceOver reads it.
pub struct TextField<const MULTILINE: bool> {
    focus_handle: FocusHandle,
    buffer: Buffer,
    placeholder: SharedString,
    label: SharedString,
    mono: bool,
    disabled: bool,
    rows: usize,
    size: TextFieldSize,
    chrome: Chrome,
    prefix: Option<SharedString>,
    focus_style: FocusStyle,
    role: TextFieldRole,
    expanded: Option<bool>,
    char_filter: Option<fn(char) -> bool>,
    /// A mouse button is down and dragging extends the selection.
    selecting: bool,
    scroll: Scroll,
    /// Column kept while moving up and down through shorter lines.
    goal_x: Option<Pixels>,
    /// The last painted layout, for mouse hits, IME and vertical moves.
    layout: Option<FieldLayout>,
    blink: Blink,
}

#[derive(Default)]
struct Scroll {
    offset: Point<Pixels>,
    /// Scroll on the next paint so the cursor is visible.
    follow_cursor: bool,
}

struct Blink {
    visible: bool,
    focused: bool,
    task: Option<Task<()>>,
}

impl<const MULTILINE: bool> EventEmitter<TextInputEvent> for TextField<MULTILINE> {}

impl<const MULTILINE: bool> Focusable for TextField<MULTILINE> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<const MULTILINE: bool> TextField<MULTILINE> {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            buffer: Buffer::default(),
            placeholder: SharedString::default(),
            label: SharedString::default(),
            mono: false,
            disabled: false,
            rows: DEFAULT_ROWS,
            size: TextFieldSize::default(),
            chrome: Chrome::Bordered,
            prefix: None,
            focus_style: FocusStyle::default(),
            role: TextFieldRole::default(),
            expanded: None,
            char_filter: None,
            selecting: false,
            scroll: Scroll::default(),
            goal_x: None,
            layout: None,
            blink: Blink {
                visible: true,
                focused: false,
                task: None,
            },
        }
    }

    pub fn text(&self) -> SharedString {
        SharedString::from(self.buffer.text.clone())
    }

    /// Replaces the text, puts the cursor at the end and clears undo history.
    /// Does not emit [`TextInputEvent::Changed`].
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.replace_text(text.into(), false, cx);
    }

    /// Like [`Self::set_text`], with the cursor at the start, so the field shows the
    /// beginning of a long text.
    pub fn set_text_at_start(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.replace_text(text.into(), true, cx);
    }

    fn replace_text(&mut self, text: SharedString, at_start: bool, cx: &mut Context<Self>) {
        let text = Self::sanitize(&text);
        let cursor = if at_start { 0 } else { text.len() };

        self.buffer = Buffer {
            text,
            selection: cursor..cursor,
            ..Buffer::default()
        };
        self.layout = None;
        self.scroll = Scroll {
            follow_cursor: true,
            ..Scroll::default()
        };
        cx.notify();
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// The name VoiceOver reads for the field.
    pub fn set_label(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.label = label.into();
        cx.notify();
    }

    /// The name VoiceOver reads for the field.
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// Geist Mono at 12.5px, for names, paths and branches.
    pub fn set_mono(&mut self, mono: bool, cx: &mut Context<Self>) {
        self.mono = mono;
        cx.notify();
    }

    /// A disabled field is dimmed, cannot be focused and ignores input.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        self.selecting = false;
        cx.notify();
    }

    pub fn set_size(&mut self, size: TextFieldSize, cx: &mut Context<Self>) {
        self.size = size;
        cx.notify();
    }

    /// `false` removes the border, background, focus ring and side padding.
    pub fn set_bordered(&mut self, bordered: bool, cx: &mut Context<Self>) {
        self.chrome = if bordered {
            Chrome::Bordered
        } else {
            Chrome::Plain
        };
        cx.notify();
    }

    /// Faint text before the value. It is not part of the value and cannot be selected.
    pub fn set_prefix(&mut self, prefix: Option<SharedString>, cx: &mut Context<Self>) {
        self.prefix = prefix;
        cx.notify();
    }

    pub fn set_focus_style(&mut self, focus_style: FocusStyle, cx: &mut Context<Self>) {
        self.focus_style = focus_style;
        cx.notify();
    }

    pub fn set_role(&mut self, role: TextFieldRole, cx: &mut Context<Self>) {
        self.role = role;
        cx.notify();
    }

    /// Whether the popup this field controls is open. `None` leaves it unset.
    pub fn set_expanded(&mut self, expanded: Option<bool>, cx: &mut Context<Self>) {
        self.expanded = expanded;
        cx.notify();
    }

    /// Keeps only the characters `filter` accepts, for typing and paste.
    /// Text that is rejected as a whole changes nothing.
    pub fn set_char_filter(&mut self, filter: Option<fn(char) -> bool>, cx: &mut Context<Self>) {
        self.char_filter = filter;
        cx.notify();
    }

    /// Sanitizes `text` and drops rejected characters.
    /// `None` when something was typed and all of it was rejected.
    fn accept(&self, text: &str) -> Option<String> {
        let clean = Self::sanitize(text);

        let Some(filter) = self.char_filter else {
            return Some(clean);
        };

        let kept: String = clean
            .chars()
            .filter(|character| filter(*character))
            .collect();

        (!kept.is_empty() || clean.is_empty()).then_some(kept)
    }

    fn sanitize(text: &str) -> String {
        if MULTILINE {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace("\r\n", " ").replace(['\r', '\n'], " ")
        }
    }

    fn key_context() -> &'static str {
        if MULTILINE {
            TEXT_AREA_CONTEXT
        } else {
            TEXT_INPUT_CONTEXT
        }
    }

    /// Starts or stops the blink when focus changes; called from render.
    fn sync_blink(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.blink.focused != focused {
            self.blink.focused = focused;
            self.restart_blink(cx);
        }
    }

    /// Shows the cursor now and blinks it from here, so it stays visible while typing.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink.visible = true;
        self.blink.task = None;

        if !self.blink.focused || reduce_motion(cx) {
            return;
        }

        self.blink.task = Some(cx.spawn(async move |field, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;

                let toggled = field.update(cx, |field, cx| {
                    // Reduce Motion may have turned on since the blink started.
                    field.blink.visible = reduce_motion(cx) || !field.blink.visible;
                    cx.notify();

                    !reduce_motion(cx)
                });

                if !toggled.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    fn selection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll.follow_cursor = true;
        self.restart_blink(cx);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout = None;
        self.goal_x = None;
        self.scroll.follow_cursor = true;
        self.restart_blink(cx);
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextInputEvent::Changed(self.text()));
        cx.notify();
    }

    fn move_by(
        &mut self,
        motion: Motion,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (target, goal_x) = self.motion_target(motion, select);

        if select {
            self.buffer.select_to(target);
        } else {
            self.buffer.move_to(target);
        }

        self.goal_x = goal_x;
        self.selection_changed(window, cx);
    }

    /// Where a motion puts the cursor, and the column to keep for vertical moves.
    fn motion_target(&self, motion: Motion, select: bool) -> (usize, Option<Pixels>) {
        let buffer = &self.buffer;
        let cursor = buffer.cursor();
        let collapse = !select && !buffer.selection.is_empty();

        let target = match motion {
            Motion::Left if collapse => buffer.selection.start,
            Motion::Right if collapse => buffer.selection.end,
            Motion::Left => buffer.previous_boundary(cursor),
            Motion::Right => buffer.next_boundary(cursor),
            Motion::WordLeft => buffer.previous_word_start(cursor),
            Motion::WordRight => buffer.next_word_end(cursor),
            Motion::LineStart => buffer.line_start(cursor),
            Motion::LineEnd => buffer.line_end(cursor),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => buffer.text.len(),
            Motion::Up => return self.vertical_target(false),
            Motion::Down => return self.vertical_target(true),
        };

        (target, None)
    }

    /// One visual row up or down; past the first or last row it goes to the start or end.
    fn vertical_target(&self, down: bool) -> (usize, Option<Pixels>) {
        let fallback = if down { self.buffer.text.len() } else { 0 };

        let Some(layout) = self
            .layout
            .as_ref()
            .filter(|_| !self.buffer.text.is_empty())
        else {
            return (fallback, None);
        };

        let position = layout.position_for(self.buffer.cursor());
        let goal_x = self.goal_x.unwrap_or(position.x);
        let row_middle = layout.line_height * 0.5;
        let y = if down {
            position.y + layout.line_height + row_middle
        } else {
            position.y - layout.line_height + row_middle
        };

        if y < px(0.) || y >= layout.height() {
            return (fallback, Some(goal_x));
        }

        let target = self.buffer.snap(layout.index_for(point(goal_x, y)));

        (target, Some(goal_x))
    }

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        if self.buffer.text.is_empty() {
            return 0;
        }

        match &self.layout {
            Some(layout) => self.buffer.snap(layout.index_for(position - layout.origin)),
            None => self.buffer.cursor(),
        }
    }

    fn insert(&mut self, text: &str, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.accept(text) else {
            return;
        };
        let range = self
            .buffer
            .marked
            .clone()
            .unwrap_or(self.buffer.selection.clone());

        self.buffer.replace(range, &text, kind);
        self.text_changed(window, cx);
    }

    fn delete_with(
        &mut self,
        delete: fn(&mut Buffer) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if delete(&mut self.buffer) {
            self.text_changed(window, cx);
        } else {
            window.play_system_bell();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.index_for_mouse(event.position);

        match event.click_count {
            2 => self.buffer.select_range(self.buffer.word_range(index)),
            3.. => self
                .buffer
                .select_range(self.buffer.line_start(index)..self.buffer.line_end(index)),
            _ if event.modifiers.shift => self.buffer.select_to(index),
            _ => self.buffer.move_to(index),
        }

        // Dragging after a double or triple click would drop the word or line.
        self.selecting = event.click_count < 2;
        self.goal_x = None;
        self.selection_changed(window, cx);
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            let index = self.index_for_mouse(event.position);
            self.buffer.select_to(index);
            self.selection_changed(window, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = &self.layout else {
            return;
        };

        let max = (layout.height() - layout.bounds.size.height).max(px(0.));
        let delta = event.delta.pixel_delta(layout.line_height);
        let offset = (self.scroll.offset.y - delta.y).clamp(px(0.), max);

        if offset != self.scroll.offset.y {
            self.scroll.offset.y = offset;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn copy(&mut self, _: &action::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            let selected = self.buffer.selected_text().to_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn cut(&mut self, _: &action::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection.is_empty() {
            let selected = self.buffer.selected_text().to_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
            self.buffer
                .replace(self.buffer.selection.clone(), "", EditKind::Other);
            self.text_changed(window, cx);
        }
    }

    fn paste(&mut self, _: &action::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text, EditKind::Other, window, cx);
        }
    }

    fn undo(&mut self, _: &action::Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.undo() {
            self.text_changed(window, cx);
        }
    }

    fn redo(&mut self, _: &action::Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.redo() {
            self.text_changed(window, cx);
        }
    }

    fn submit(&mut self, _: &action::Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submitted(self.text()));
    }

    fn newline(&mut self, _: &action::Newline, window: &mut Window, cx: &mut Context<Self>) {
        if MULTILINE {
            self.insert("\n", EditKind::Other, window, cx);
        }
    }

    fn select_all(&mut self, _: &action::SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_range(0..self.buffer.text.len());
        self.selection_changed(window, cx);
    }

    fn on_actions(element: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        macro_rules! motion {
            ($element:expr, $action:ty, $motion:expr, $select:expr) => {
                $element.on_action(cx.listener(|field, _: &$action, window, cx| {
                    field.move_by($motion, $select, window, cx);
                }))
            };
        }

        macro_rules! delete {
            ($element:expr, $action:ty, $delete:expr) => {
                $element.on_action(cx.listener(|field, _: &$action, window, cx| {
                    field.delete_with($delete, window, cx);
                }))
            };
        }

        let element = motion!(element, action::Left, Motion::Left, false);
        let element = motion!(element, action::Right, Motion::Right, false);
        let element = motion!(element, action::Up, Motion::Up, false);
        let element = motion!(element, action::Down, Motion::Down, false);
        let element = motion!(element, action::WordLeft, Motion::WordLeft, false);
        let element = motion!(element, action::WordRight, Motion::WordRight, false);
        let element = motion!(element, action::LineStart, Motion::LineStart, false);
        let element = motion!(element, action::LineEnd, Motion::LineEnd, false);
        let element = motion!(element, action::DocumentStart, Motion::DocumentStart, false);
        let element = motion!(element, action::DocumentEnd, Motion::DocumentEnd, false);
        let element = motion!(element, action::SelectLeft, Motion::Left, true);
        let element = motion!(element, action::SelectRight, Motion::Right, true);
        let element = motion!(element, action::SelectUp, Motion::Up, true);
        let element = motion!(element, action::SelectDown, Motion::Down, true);
        let element = motion!(element, action::SelectWordLeft, Motion::WordLeft, true);
        let element = motion!(element, action::SelectWordRight, Motion::WordRight, true);
        let element = motion!(element, action::SelectToLineStart, Motion::LineStart, true);
        let element = motion!(element, action::SelectToLineEnd, Motion::LineEnd, true);
        let element = motion!(
            element,
            action::SelectToDocumentStart,
            Motion::DocumentStart,
            true
        );
        let element = motion!(
            element,
            action::SelectToDocumentEnd,
            Motion::DocumentEnd,
            true
        );

        let element = delete!(element, action::Backspace, Buffer::backspace);
        let element = delete!(element, action::Delete, Buffer::delete);
        let element = delete!(element, action::DeleteWordLeft, Buffer::delete_word_left);
        let element = delete!(element, action::DeleteWordRight, Buffer::delete_word_right);
        let element = delete!(
            element,
            action::DeleteToLineStart,
            Buffer::delete_to_line_start
        );

        element
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(|_: &action::ShowCharacterPalette, window, _| {
                window.show_character_palette();
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when(MULTILINE, |element| {
                element.on_scroll(cx.listener(Self::on_scroll))
            })
    }
}

impl TextArea {
    /// How many lines show before the text scrolls. At least one.
    pub fn set_rows(&mut self, rows: usize, cx: &mut Context<Self>) {
        self.rows = rows.max(1);
        cx.notify();
    }
}

impl<const MULTILINE: bool> Render for TextField<MULTILINE> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = !self.disabled && self.focus_handle.is_focused(window);
        self.sync_blink(focused, cx);

        let colors = cx.theme().colors();
        let (line, raised, accent, fg, faint) = (
            colors.line,
            colors.raised,
            colors.accent,
            colors.fg,
            colors.faint,
        );

        let role = match (self.role, MULTILINE) {
            (TextFieldRole::ComboBox, _) => Role::EditableComboBox,
            (TextFieldRole::Text, true) => Role::MultilineTextInput,
            (TextFieldRole::Text, false) => Role::TextInput,
        };
        let font = if self.mono { MONO_FONT } else { UI_FONT };
        let font_size = self.size.text_size(self.mono);
        let bordered = self.chrome == Chrome::Bordered;
        let border_focus = bordered && self.focus_style == FocusStyle::Border;
        let ring_focus = bordered && self.focus_style == FocusStyle::Ring;
        let placeholder = self.placeholder.clone();

        let field = div()
            .id("text-field")
            .role(role)
            .aria_label(self.label.clone())
            .aria_value(self.text())
            .when(!placeholder.is_empty(), |field| {
                field.aria_placeholder(placeholder)
            })
            .when_some(self.expanded, |field, expanded| {
                field.aria_expanded(expanded)
            })
            .key_context(Self::key_context())
            .flex()
            .items_center()
            .w_full()
            .map(|field| {
                if MULTILINE {
                    field.py(px(8.))
                } else {
                    field.h(self.size.height())
                }
            })
            .when(bordered, |field| {
                field
                    .px(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(line)
                    .bg(raised)
            })
            .font_family(font)
            .text_size(font_size)
            .line_height(LINE_HEIGHT)
            .text_color(fg)
            .when_some(self.prefix.clone(), |field, prefix| {
                field.child(div().flex_none().mr(px(6.)).text_color(faint).child(prefix))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(FieldText { field: cx.entity() }),
            );

        if self.disabled {
            field.opacity(0.5)
        } else {
            Self::on_actions(field, cx)
                .track_focus(&self.focus_handle)
                .cursor(CursorStyle::IBeam)
                .when(ring_focus, |field| {
                    field.focus(|style| focus_ring(style, accent))
                })
                .when(border_focus, |field| {
                    field.focus(|style| style.border_color(accent))
                })
        }
    }
}

impl<const MULTILINE: bool> EntityInputHandler for TextField<MULTILINE> {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));

        self.buffer.text.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection),
            reversed: self.buffer.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .as_ref()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.marked = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.insert(&text, EditKind::Other, window, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }

        let Some(text) = self.accept(text) else {
            return;
        };
        let range = range_utf16
            .map(|range| self.buffer.range_from_utf16(&range))
            .or(self.buffer.marked.clone())
            .unwrap_or(self.buffer.selection.clone());

        self.buffer.replace(range, &text, EditKind::Typing);
        self.text_changed(window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }

        let text = Self::sanitize(text);
        let range = range_utf16
            .map(|range| self.buffer.range_from_utf16(&range))
            .or(self.buffer.marked.clone())
            .unwrap_or(self.buffer.selection.clone());

        self.buffer
            .replace_and_mark(range, &text, new_selected_range_utf16);
        self.text_changed(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range_utf16);
        let start = layout.position_for(range.start);
        let end = layout.position_for(range.end);

        let width = if end.y == start.y {
            (end.x - start.x).max(CURSOR_WIDTH)
        } else {
            CURSOR_WIDTH
        };

        Some(Bounds::new(
            layout.origin + start,
            size(width, layout.line_height),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };

        let range = self.buffer.range_from_utf16(&range_utf16);

        layout.selection_rects(&range)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;

        if !layout.bounds.contains(&point) {
            return None;
        }

        let index = self.index_for_mouse(point);

        Some(self.buffer.offset_to_utf16(index))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.buffer.range_from_utf16(&range_utf16);
        self.buffer.select_range(range);
        self.selection_changed(window, cx);
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer.text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.disabled
    }
}

#[derive(Clone, Copy, Debug)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

/// One shaped line of text (wrapped into rows in a `TextArea`) and where it sits.
struct LaidLine {
    /// Byte offset of the line in the text.
    start: usize,
    /// Top of the line, from the top of the text.
    y: Pixels,
    line: WrappedLine,
}

/// The painted text, kept so mouse clicks, IME and up/down can map points and offsets.
struct FieldLayout {
    lines: Vec<LaidLine>,
    line_height: Pixels,
    /// The visible text area.
    bounds: Bounds<Pixels>,
    /// Where the text starts in the window, after scrolling.
    origin: Point<Pixels>,
}

impl FieldLayout {
    fn new(shaped: Vec<WrappedLine>, line_height: Pixels, bounds: Bounds<Pixels>) -> Self {
        let mut start = 0;
        let mut y = px(0.);

        let lines = shaped
            .into_iter()
            .map(|line| {
                let laid = LaidLine { start, y, line };
                start += laid.line.len() + 1;
                y += laid.line.size(line_height).height;
                laid
            })
            .collect();

        Self {
            lines,
            line_height,
            bounds,
            origin: bounds.origin,
        }
    }

    fn height(&self) -> Pixels {
        self.lines.last().map_or(self.line_height, |last| {
            last.y + last.line.size(self.line_height).height
        })
    }

    fn width(&self) -> Pixels {
        self.lines
            .iter()
            .map(|laid| laid.line.width())
            .max()
            .unwrap_or_default()
    }

    /// Position of a byte offset, from the top left of the text.
    fn position_for(&self, offset: usize) -> Point<Pixels> {
        let Some(laid) = self.lines.iter().rev().find(|laid| laid.start <= offset) else {
            return Point::default();
        };

        let local = (offset - laid.start).min(laid.line.len());

        laid.line
            .position_for_index(local, self.line_height)
            .map_or(point(px(0.), laid.y), |position| {
                point(position.x, laid.y + position.y)
            })
    }

    /// The byte offset closest to a point, measured from the top left of the text.
    fn index_for(&self, position: Point<Pixels>) -> usize {
        let y = position
            .y
            .clamp(px(0.), (self.height() - px(1.)).max(px(0.)));

        let Some(laid) = self.lines.iter().rev().find(|laid| laid.y <= y) else {
            return 0;
        };

        let height = laid.line.size(self.line_height).height;
        let local = point(
            position.x.max(px(0.)),
            (y - laid.y).clamp(px(0.), (height - px(1.)).max(px(0.))),
        );
        let (Ok(index) | Err(index)) = laid
            .line
            .closest_index_for_position(local, self.line_height);

        laid.start + index
    }

    /// Selection rectangles in window coordinates, one or more per line.
    fn selection_rects(&self, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
        let width = self.bounds.size.width;
        let line_height = self.line_height;
        let mut rects = Vec::new();

        for laid in &self.lines {
            let end = laid.start + laid.line.len();

            if range.end < laid.start || range.start > end {
                continue;
            }

            // A selected line break paints to the right edge, like macOS.
            let through_break = range.end > end;
            let from = self.position_for(range.start.max(laid.start));
            let to = self.position_for(range.end.min(end));
            let last_right = if through_break { width.max(to.x) } else { to.x };

            if from.y == to.y {
                if last_right > from.x {
                    rects.push(Bounds::from_corners(
                        from,
                        point(last_right, from.y + line_height),
                    ));
                }
            } else {
                rects.push(Bounds::from_corners(
                    from,
                    point(width.max(from.x), from.y + line_height),
                ));
                rects.push(Bounds::from_corners(
                    point(px(0.), from.y + line_height),
                    point(width, to.y),
                ));
                rects.push(Bounds::from_corners(
                    point(px(0.), to.y),
                    point(last_right, to.y + line_height),
                ));
            }
        }

        rects
            .into_iter()
            .filter(|rect| rect.size.width > px(0.) && rect.size.height > px(0.))
            .map(|rect| Bounds::new(self.origin + rect.origin, rect.size))
            .collect()
    }
}

/// Moves `scroll` the least amount that shows `start..end` in a viewport of `viewport`.
fn follow(scroll: Pixels, start: Pixels, end: Pixels, viewport: Pixels) -> Pixels {
    if start < scroll {
        start
    } else if end > scroll + viewport {
        end - viewport
    } else {
        scroll
    }
}

fn text_runs(len: usize, font: Font, color: Hsla, marked: Option<Range<usize>>) -> Vec<TextRun> {
    let run = TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let Some(marked) = marked.filter(|marked| marked.end <= len) else {
        return vec![run];
    };

    let underline = UnderlineStyle {
        color: Some(color),
        thickness: px(1.),
        wavy: false,
    };

    [
        TextRun {
            len: marked.start,
            ..run.clone()
        },
        TextRun {
            len: marked.end - marked.start,
            underline: Some(underline),
            ..run.clone()
        },
        TextRun {
            len: len - marked.end,
            ..run
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

/// Paints a field's text, selection and cursor, and registers it for IME.
struct FieldText<const MULTILINE: bool> {
    field: Entity<TextField<MULTILINE>>,
}

struct FieldPaint {
    layout: Option<FieldLayout>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    scroll: Point<Pixels>,
}

impl<const MULTILINE: bool> IntoElement for FieldText<MULTILINE> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<const MULTILINE: bool> Element for FieldText<MULTILINE> {
    type RequestLayoutState = ();
    type PrepaintState = FieldPaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let rows = if MULTILINE {
            self.field.read(cx).rows
        } else {
            1
        };

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows).into();

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let field = self.field.read(cx);
        let colors = cx.theme().colors();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let focused = field.focus_handle.is_focused(window);
        let buffer = &field.buffer;
        let empty = buffer.text.is_empty();

        let (text, color, marked) = if empty {
            (field.placeholder.clone(), colors.faint, None)
        } else {
            let text = SharedString::from(buffer.text.clone());
            (text, style.color, buffer.marked.clone())
        };

        let runs = text_runs(text.len(), style.font(), color, marked);
        let wrap_width = MULTILINE.then(|| (bounds.size.width - CURSOR_WIDTH).max(px(0.)));
        let shaped = match window
            .text_system()
            .shape_text(text, font_size, &runs, wrap_width, None)
        {
            Ok(lines) => lines.into_iter().collect(),
            Err(error) => {
                log::error!("cannot shape text field: {error:#}");
                Vec::new()
            }
        };

        let mut layout = FieldLayout::new(shaped, line_height, bounds);
        let cursor = if empty {
            Point::default()
        } else {
            layout.position_for(buffer.cursor())
        };

        let mut scroll = field.scroll.offset;
        let viewport = bounds.size;

        if MULTILINE {
            if field.scroll.follow_cursor {
                scroll.y = follow(scroll.y, cursor.y, cursor.y + line_height, viewport.height);
            }

            scroll.x = px(0.);
            scroll.y = scroll
                .y
                .clamp(px(0.), (layout.height() - viewport.height).max(px(0.)));
        } else {
            if field.scroll.follow_cursor {
                scroll.x = follow(scroll.x, cursor.x, cursor.x + CURSOR_WIDTH, viewport.width);
            }

            let max_x = (layout.width() + CURSOR_WIDTH - viewport.width).max(px(0.));
            scroll.x = scroll.x.clamp(px(0.), max_x);
            scroll.y = px(0.);
        }

        layout.origin = bounds.origin - scroll;

        let selections = if focused && !empty && !buffer.selection.is_empty() {
            layout
                .selection_rects(&buffer.selection)
                .into_iter()
                .map(|rect| fill(rect, colors.selection))
                .collect()
        } else {
            Vec::new()
        };

        let show_cursor = focused && field.blink.visible && buffer.selection.is_empty();
        let cursor = show_cursor.then(|| {
            fill(
                Bounds::new(layout.origin + cursor, size(CURSOR_WIDTH, line_height)),
                colors.fg,
            )
        });

        FieldPaint {
            layout: Some(layout),
            selections,
            cursor,
            scroll,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        paint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus_handle = field.focus_handle.clone();

        if !field.disabled {
            let handler = ElementInputHandler::new(bounds, self.field.clone());
            window.handle_input(&focus_handle, handler, cx);
        }

        let Some(layout) = paint.layout.take() else {
            return;
        };

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in paint.selections.drain(..) {
                window.paint_quad(selection);
            }

            for laid in &layout.lines {
                let origin = layout.origin + point(px(0.), laid.y);
                let painted = laid.line.paint(
                    origin,
                    layout.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                if let Err(error) = painted {
                    log::error!("cannot paint text field: {error:#}");
                }
            }

            if let Some(cursor) = paint.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        let scroll = paint.scroll;
        self.field.update(cx, |field, _| {
            field.layout = Some(layout);
            field.scroll.offset = scroll;
            field.scroll.follow_cursor = false;
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    selection: Range<usize>,
    reversed: bool,
}

/// Text, selection and undo history, with no GPUI types so it can be tested alone.
/// Offsets are UTF-8 byte offsets; moves and deletes step over whole graphemes.
#[derive(Debug, Default)]
struct Buffer {
    text: String,
    selection: Range<usize>,
    /// The cursor is at `selection.start` instead of `selection.end`.
    reversed: bool,
    /// IME text still being composed.
    marked: Option<Range<usize>>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Typing or deleting right after an edit of the same kind joins its undo step.
    last_edit: Option<EditKind>,
}

impl Buffer {
    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn selected_text(&self) -> &str {
        self.text.get(self.selection.clone()).unwrap_or_default()
    }

    /// Clamps to the text and to a char boundary, so slicing never panics.
    fn clamp(&self, offset: usize) -> usize {
        self.text.floor_char_boundary(offset)
    }

    /// The grapheme boundary at or before `offset`.
    fn snap(&self, offset: usize) -> usize {
        if offset >= self.text.len() {
            return self.text.len();
        }

        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .take_while(|index| *index <= offset)
            .last()
            .unwrap_or(0)
    }

    fn move_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        self.selection = offset..offset;
        self.reversed = false;
        self.last_edit = None;
    }

    /// Moves the cursor end of the selection and keeps the other end.
    fn select_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);

        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }

        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }

        self.last_edit = None;
    }

    fn select_range(&mut self, range: Range<usize>) {
        let (start, end) = (self.clamp(range.start), self.clamp(range.end));
        self.selection = start.min(end)..start.max(end);
        self.reversed = false;
        self.last_edit = None;
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .map(|(index, _)| index)
            .find(|index| *index < offset)
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|index| *index > offset)
            .unwrap_or(self.text.len())
    }

    /// Start of the word before `offset`, skipping spaces and punctuation, like Option-Left.
    fn previous_word_start(&self, offset: usize) -> usize {
        self.text
            .split_word_bound_indices()
            .rev()
            .filter(|(start, _)| *start < offset)
            .find(|(_, segment)| is_word(segment))
            .map_or(0, |(start, _)| start)
    }

    /// End of the word after `offset`, skipping spaces and punctuation, like Option-Right.
    fn next_word_end(&self, offset: usize) -> usize {
        self.text
            .split_word_bound_indices()
            .map(|(start, segment)| (start + segment.len(), segment))
            .filter(|(end, _)| *end > offset)
            .find(|(_, segment)| is_word(segment))
            .map_or(self.text.len(), |(end, _)| end)
    }

    /// The word, space run or symbol under `offset`, for double-click.
    fn word_range(&self, offset: usize) -> Range<usize> {
        let mut segments = self
            .text
            .split_word_bound_indices()
            .map(|(start, segment)| start..start + segment.len());

        let at_end = offset >= self.text.len();

        segments
            .find(|range| range.contains(&offset) || (at_end && range.end == offset))
            .unwrap_or(offset..offset)
    }

    fn line_start(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);

        self.text[..offset].rfind('\n').map_or(0, |index| index + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);

        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
            reversed: self.reversed,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.reversed = snapshot.reversed;
        self.marked = None;
        self.last_edit = None;
    }

    fn record_undo(&mut self, kind: EditKind) {
        let joins = kind != EditKind::Other && self.last_edit == Some(kind);

        if !joins {
            self.undo_stack.push(self.snapshot());

            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
        }

        self.redo_stack.clear();
        self.last_edit = Some(kind);
    }

    /// Replaces `range` with `new_text` and puts the cursor after it.
    /// Returns where the new text starts.
    fn replace(&mut self, range: Range<usize>, new_text: &str, kind: EditKind) -> usize {
        let (start, end) = (self.clamp(range.start), self.clamp(range.end));
        let (start, end) = (start.min(end), start.max(end));

        self.record_undo(kind);
        self.text.replace_range(start..end, new_text);

        let cursor = start + new_text.len();
        self.selection = cursor..cursor;
        self.reversed = false;
        self.marked = None;

        start
    }

    /// Replaces `range` with IME text that is still being composed.
    /// `selected_utf16` is relative to the new text.
    fn replace_and_mark(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        let start = self.replace(range, new_text, EditKind::Typing);

        self.marked = (!new_text.is_empty()).then(|| start..start + new_text.len());

        if let Some(selected) = selected_utf16 {
            let base = self.offset_to_utf16(start);
            let inserted = new_text.encode_utf16().count();
            let selected = base + selected.start.min(inserted)..base + selected.end.min(inserted);
            self.selection = self.range_from_utf16(&selected);
        }
    }

    /// Deletes the selection, or from the cursor to `target(cursor)` when nothing is selected.
    fn delete_toward(&mut self, target: fn(&Self, usize) -> usize, kind: EditKind) -> bool {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor();
            let target = target(self, cursor);
            cursor.min(target)..cursor.max(target)
        } else {
            self.selection.clone()
        };

        if range.is_empty() {
            return false;
        }

        self.replace(range, "", kind);

        true
    }

    fn backspace(&mut self) -> bool {
        self.delete_toward(Self::previous_boundary, EditKind::Deleting)
    }

    fn delete(&mut self) -> bool {
        self.delete_toward(Self::next_boundary, EditKind::Deleting)
    }

    fn delete_word_left(&mut self) -> bool {
        self.delete_toward(Self::previous_word_start, EditKind::Other)
    }

    fn delete_word_right(&mut self) -> bool {
        self.delete_toward(Self::next_word_end, EditKind::Other)
    }

    /// Cmd-Backspace: to the start of the line, or the line break before it.
    fn delete_to_line_start(&mut self) -> bool {
        self.delete_toward(
            |buffer, cursor| {
                let start = buffer.line_start(cursor);

                if start == cursor {
                    buffer.previous_boundary(cursor)
                } else {
                    start
                }
            },
            EditKind::Other,
        )
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };

        self.redo_stack.push(self.snapshot());
        self.restore(snapshot);

        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };

        self.undo_stack.push(self.snapshot());
        self.restore(snapshot);

        true
    }

    fn offset_from_utf16(&self, offset_utf16: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;

        for character in self.text.chars() {
            if utf16 >= offset_utf16 {
                break;
            }

            utf16 += character.len_utf16();
            utf8 += character.len_utf8();
        }

        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;

        for character in self.text.chars() {
            if utf8 >= offset {
                break;
            }

            utf8 += character.len_utf8();
            utf16 += character.len_utf16();
        }

        utf16
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let (start, end) = (
            self.offset_from_utf16(range.start),
            self.offset_from_utf16(range.end),
        );

        start.min(end)..start.max(end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

fn preview_field(field: impl IntoElement) -> impl IntoElement {
    div().w(px(240.)).child(field)
}

impl Component for TextInput {
    fn name() -> &'static str {
        "TextInput"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let name = window.use_keyed_state("text-input-name", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Name", cx);
            input.set_mono(true, cx);
            input.set_text("docs-site", cx);
            input
        });
        let filter = window.use_keyed_state("text-input-filter", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Filter files", cx);
            input.set_placeholder("Filter files", cx);
            input
        });
        let disabled = window.use_keyed_state("text-input-disabled", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Task branch", cx);
            input.set_mono(true, cx);
            input.set_text("swarm/docs-site", cx);
            input.set_disabled(true, cx);
            input
        });
        let small = window.use_keyed_state("text-input-small", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Small field", cx);
            input.set_placeholder("Small", cx);
            input.set_size(TextFieldSize::Small, cx);
            input
        });
        let search = window.use_keyed_state("text-input-borderless", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Search commands", cx);
            input.set_placeholder("Search commands", cx);
            input.set_bordered(false, cx);
            input.set_prefix(Some("\u{203a}".into()), cx);
            input
        });
        let orchestrator = window.use_keyed_state("text-input-border-focus", cx, |_, cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Message the orchestrator", cx);
            input.set_placeholder("Message the orchestrator", cx);
            input.set_size(TextFieldSize::Large, cx);
            input.set_focus_style(FocusStyle::Border, cx);
            input
        });
        let message = window.use_keyed_state("text-area-message", cx, |_, cx| {
            let mut area = TextArea::new(cx);
            area.set_label("First message", cx);
            area.set_placeholder("Optional", cx);
            area.set_rows(3, cx);
            area
        });

        v_flex()
            .gap_6()
            .child(example_group(
                "TextInput",
                [
                    example("Name", preview_field(name)),
                    example("Filter files", preview_field(filter)),
                    example("Disabled", preview_field(disabled)),
                    example("Small", preview_field(small)),
                    example("Borderless with prefix", preview_field(search)),
                    example("Border focus", preview_field(orchestrator)),
                ],
            ))
            .child(example_group(
                "TextArea",
                [example("First message", preview_field(message))],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    fn focus(input: &Entity<TextInput>, cx: &mut VisualTestContext) {
        let handle = input.read_with(cx, Focusable::focus_handle);

        cx.update(|window, cx| handle.focus(window, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_cursor_stops_blinking_when_reduce_motion_turns_on(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new(cx));

        focus(&input, cx);
        cx.executor().advance_clock(BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!input.read_with(cx, |input, _| input.blink.visible));

        // Shown again, then Reduce Motion turns on: the next tick must not hide it.
        cx.executor().advance_clock(BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.blink.visible));

        cx.update(|_, cx| cx.set_reduce_motion(true));
        cx.executor().advance_clock(BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.blink.visible));

        cx.executor().advance_clock(BLINK_INTERVAL * 3);
        cx.run_until_parked();
        assert!(input.read_with(cx, |input, _| input.blink.visible));
    }

    #[gpui::test]
    fn the_cursor_does_not_blink_under_reduce_motion(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::theme::init(cx);
            cx.set_reduce_motion(true);
        });
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new(cx));

        focus(&input, cx);
        cx.executor().advance_clock(BLINK_INTERVAL * 3);
        cx.run_until_parked();

        assert!(input.read_with(cx, |input, _| input.blink.visible));
    }

    fn buffer(text: &str, cursor: usize) -> Buffer {
        Buffer {
            text: text.to_owned(),
            selection: cursor..cursor,
            ..Buffer::default()
        }
    }

    #[test]
    fn option_left_jumps_to_the_previous_word_start() {
        let text = "git push --force origin";

        assert_eq!(buffer(text, text.len()).previous_word_start(text.len()), 17);
        assert_eq!(buffer(text, 17).previous_word_start(17), 11);
        assert_eq!(buffer(text, 11).previous_word_start(11), 4);
        assert_eq!(buffer(text, 2).previous_word_start(2), 0);
    }

    #[test]
    fn option_right_jumps_to_the_next_word_end() {
        let text = "docs-site  main";

        assert_eq!(buffer(text, 0).next_word_end(0), 4);
        assert_eq!(buffer(text, 4).next_word_end(4), 9);
        assert_eq!(buffer(text, 9).next_word_end(9), 15);
        assert_eq!(buffer(text, 15).next_word_end(15), 15);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut buffer = buffer("docs-site", 0);
        buffer.select_range(0..4);

        buffer.replace(buffer.selection.clone(), "web", EditKind::Typing);

        assert_eq!(buffer.text, "web-site");
        assert_eq!(buffer.selection, 3..3);
    }

    #[test]
    fn shift_moves_extend_and_flip_the_selection() {
        let mut buffer = buffer("abcdef", 3);

        buffer.select_to(5);
        buffer.select_to(1);

        assert_eq!(buffer.selection, 1..3);
        assert_eq!(buffer.cursor(), 1);
    }

    #[test]
    fn option_backspace_deletes_the_previous_word() {
        let mut buffer = buffer("run the tests", 13);

        buffer.delete_word_left();

        assert_eq!(buffer.text, "run the ");
    }

    #[test]
    fn option_backspace_skips_trailing_spaces_and_punctuation() {
        let mut buffer = buffer("fix(input): ", 12);

        buffer.delete_word_left();

        assert_eq!(buffer.text, "fix(");
    }

    #[test]
    fn cmd_backspace_deletes_to_the_line_start() {
        let mut buffer = buffer("first\nsecond line", 17);

        buffer.delete_to_line_start();

        assert_eq!(buffer.text, "first\n");
    }

    #[test]
    fn backspace_removes_a_whole_accented_letter() {
        let decomposed = "cafe\u{301}";
        let mut precomposed = buffer("café", "café".len());
        let mut combining = buffer(decomposed, decomposed.len());

        precomposed.backspace();
        combining.backspace();

        assert_eq!(
            (precomposed.text.as_str(), combining.text.as_str()),
            ("caf", "caf")
        );
    }

    #[test]
    fn arrows_step_over_a_whole_emoji() {
        let text = "a👍🏽b";
        let buffer = buffer(text, 1);

        assert_eq!(buffer.next_boundary(1), text.len() - 1);
        assert_eq!(buffer.previous_boundary(text.len() - 1), 1);
    }

    #[test]
    fn delete_removes_a_whole_emoji() {
        let mut buffer = buffer("a👍🏽b", 1);

        buffer.delete();

        assert_eq!(buffer.text, "ab");
    }

    #[test]
    fn snap_never_lands_inside_a_grapheme() {
        let text = "é👍🏽";
        let buffer = buffer(text, 0);

        assert!((0..=text.len()).all(|offset| {
            let snapped = buffer.snap(offset);
            snapped == text.len()
                || buffer
                    .text
                    .grapheme_indices(true)
                    .any(|(index, _)| index == snapped)
        }));
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let buffer = buffer("é👍🏽x", 0);
        let x = buffer.text.len() - 1;

        assert_eq!(buffer.offset_from_utf16(buffer.offset_to_utf16(x)), x);
    }

    #[test]
    fn double_click_selects_the_word_under_the_pointer() {
        let buffer = buffer("git push --force", 0);

        assert_eq!(buffer.word_range(6), 4..8);
    }

    #[test]
    fn undo_restores_typing_as_one_step_and_redo_brings_it_back() {
        let mut buffer = buffer("", 0);

        for character in ["h", "i"] {
            buffer.replace(buffer.selection.clone(), character, EditKind::Typing);
        }

        buffer.undo();
        let undone = buffer.text.clone();
        buffer.redo();

        assert_eq!((undone.as_str(), buffer.text.as_str()), ("", "hi"));
    }

    #[test]
    fn moving_the_cursor_starts_a_new_undo_step() {
        let mut buffer = buffer("", 0);

        buffer.replace(0..0, "a", EditKind::Typing);
        buffer.move_to(0);
        buffer.replace(0..0, "b", EditKind::Typing);
        buffer.undo();

        assert_eq!(buffer.text, "a");
    }

    #[test]
    fn ime_marks_the_composed_text() {
        let mut buffer = buffer("A", 1);

        buffer.replace_and_mark(1..1, "日本", Some(2..2));

        assert_eq!(buffer.marked, Some(1..7));
        assert_eq!(buffer.selection, 7..7);
    }

    #[test]
    fn single_line_fields_turn_pasted_line_breaks_into_spaces() {
        assert_eq!(TextInput::sanitize("a\r\nb\nc"), "a b c");
        assert_eq!(TextArea::sanitize("a\r\nb"), "a\nb");
    }
}
