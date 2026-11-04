// Text layout, IME and editing structure adapted from GPUI's `examples/input.rs`
// (Zed Industries, Apache-2.0).

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    A11ySubtreeBuilder, AnyElement, App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle,
    Div, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, GlobalElementId, HighlightStyle, Hsla, InspectorElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Role, ScrollEvent, SharedString, Stateful, Style, Task, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, accesskit, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::a11y::{ignore_press, track_focus_off_tab};
use crate::component::{Component, example, example_group};
use crate::fonts::{MONO_FONT, UI_FONT};
use crate::motion::reduce_motion;
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::theme::ThemeColors;

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

/// How tall a `TextArea` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Height {
    /// `rows` lines.
    Rows,
    /// The height its parent gives.
    Fill,
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

/// Colors the text of a field: it gets the text and the theme colors and returns styled byte
/// ranges. They must be sorted, apart and on char boundaries; the field drops any other.
/// Of each style, the color, font weight, font style, background and underline apply.
pub type Highlighter = fn(&str, &ThemeColors) -> Vec<(Range<usize>, HighlightStyle)>;

/// The shared editor behind [`TextInput`] and [`TextArea`].
/// Set a label: VoiceOver reads it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "mono, disabled, read-only and selecting are independent"
)]
pub struct TextField<const MULTILINE: bool> {
    focus_handle: FocusHandle,
    buffer: Buffer,
    placeholder: SharedString,
    label: SharedString,
    mono: bool,
    disabled: bool,
    read_only: bool,
    rows: usize,
    height: Height,
    size: TextFieldSize,
    chrome: Chrome,
    prefix: Option<SharedString>,
    focus_style: FocusStyle,
    role: TextFieldRole,
    expanded: Option<bool>,
    char_filter: Option<fn(char) -> bool>,
    popup: Option<Popup>,
    highlights: Highlights,
    /// A mouse button is down and dragging extends the selection.
    selecting: bool,
    scroll: Scroll,
    /// Column kept while moving up and down through shorter lines.
    goal_x: Option<Pixels>,
    /// The last painted layout, for mouse hits, IME and vertical moves.
    layout: Option<FieldLayout>,
    blink: Blink,
}

/// Builds the elements a field renders inside its focused element.
type Popup = Rc<dyn Fn(&mut Window, &mut App) -> Vec<AnyElement>>;

#[derive(Default)]
struct Scroll {
    offset: Point<Pixels>,
    /// Scroll on the next paint so the cursor is visible.
    follow_cursor: bool,
}

/// The highlighter and its last result, kept until the text or the theme changes.
#[derive(Default)]
struct Highlights {
    highlighter: Option<Highlighter>,
    cached: Option<CachedHighlights>,
}

struct CachedHighlights {
    revision: u64,
    colors: ThemeColors,
    ranges: Vec<(Range<usize>, HighlightStyle)>,
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
            read_only: false,
            rows: DEFAULT_ROWS,
            height: Height::Rows,
            size: TextFieldSize::default(),
            chrome: Chrome::Bordered,
            prefix: None,
            focus_style: FocusStyle::default(),
            role: TextFieldRole::default(),
            expanded: None,
            char_filter: None,
            popup: None,
            highlights: Highlights::default(),
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
            revision: self.buffer.revision + 1,
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

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// A disabled field is dimmed and ignores input. It keeps its focus handle, so focus
    /// does not drop, but it is not a Tab stop.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        self.selecting = false;
        cx.notify();
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// A read-only field can be focused, selected and copied, but not edited.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        self.read_only = read_only;
        cx.notify();
    }

    fn editable(&self) -> bool {
        !self.disabled && !self.read_only
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

    /// Renders `build`'s elements inside the focused element, after the text line.
    /// Position them absolutely. Give the active one a role, an id and
    /// `aria_active_descendant()`: GPUI honours that only under a focused ancestor.
    pub fn set_popup(
        &mut self,
        build: Option<impl Fn(&mut Window, &mut App) -> Vec<AnyElement> + 'static>,
        cx: &mut Context<Self>,
    ) {
        self.popup = build.map(|build| Rc::new(build) as Popup);
        cx.notify();
    }

    /// Keeps only the characters `filter` accepts, for typing and paste.
    /// Text that is rejected as a whole changes nothing.
    pub fn set_char_filter(&mut self, filter: Option<fn(char) -> bool>, cx: &mut Context<Self>) {
        self.char_filter = filter;
        cx.notify();
    }

    /// Colors the text. The highlighter runs again only after the text or the theme changes.
    /// The placeholder is never colored.
    pub fn set_highlighter(&mut self, highlighter: Option<Highlighter>, cx: &mut Context<Self>) {
        self.highlights = Highlights {
            highlighter,
            cached: None,
        };
        cx.notify();
    }

    /// `true` makes a `TextArea` take the full height its parent gives, instead of `rows`
    /// lines. A `TextInput` ignores it.
    pub fn set_fill(&mut self, fill: bool, cx: &mut Context<Self>) {
        self.height = if fill { Height::Fill } else { Height::Rows };
        cx.notify();
    }

    /// Puts the cursor at the byte `offset`, clamped to the text and moved back to a char
    /// boundary. Clears the selection and scrolls so the cursor shows on the next paint.
    pub fn set_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.move_to(offset);
        self.goal_x = None;
        self.scroll.follow_cursor = true;
        self.restart_blink(cx);
        cx.notify();
    }

    /// Runs the highlighter when the text or the theme changed since its last run.
    fn refresh_highlights(&mut self, colors: &ThemeColors) {
        let Some(highlighter) = self.highlights.highlighter else {
            return;
        };

        let fresh = self.highlights.cached.as_ref().is_some_and(|cached| {
            cached.revision == self.buffer.revision && cached.colors == *colors
        });

        if fresh {
            return;
        }

        let ranges = valid_highlights(&self.buffer.text, highlighter(&self.buffer.text, colors));

        self.highlights.cached = Some(CachedHighlights {
            revision: self.buffer.revision,
            colors: colors.clone(),
            ranges,
        });
    }

    fn highlight_ranges(&self) -> &[(Range<usize>, HighlightStyle)] {
        self.highlights
            .cached
            .as_ref()
            .map_or(&[], |cached| cached.ranges.as_slice())
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
        if !self.editable() {
            return;
        }

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
        if !self.editable() {
            return;
        }

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
        if self.editable() && !self.buffer.selection.is_empty() {
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
        if self.editable() && self.buffer.undo() {
            self.text_changed(window, cx);
        }
    }

    fn redo(&mut self, _: &action::Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.editable() && self.buffer.redo() {
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
        let fill = MULTILINE && self.height == Height::Fill;
        let popup = self
            .popup
            .as_ref()
            .map(|build| build(window, cx))
            .unwrap_or_default();

        let field = div()
            .id("text-field")
            .role(role)
            .aria_label(self.label.clone())
            .aria_value(self.text())
            .a11y_synthetic_children(a11y_text_children(
                &self.buffer.text,
                &self.buffer.selection,
                self.buffer.reversed,
            ))
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
                    field.py(px(8.)).when(fill, Styled::h_full)
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
                    .when(fill, Styled::h_full)
                    .child(FieldText { field: cx.entity() }),
            )
            .children(popup);

        let field = if self.disabled {
            ignore_press(track_focus_off_tab(field.opacity(0.5), &self.focus_handle))
        } else {
            Self::on_actions(field, cx)
                .track_focus(&self.focus_handle)
                .cursor(CursorStyle::IBeam)
        };

        field
            .when(ring_focus, |field| {
                field.focus(|style| focus_ring(style, accent))
            })
            .when(border_focus, |field| {
                field.focus(|style| style.border_color(accent))
            })
            .a11y_flags()
            .disabled(self.disabled)
            .read_only(self.read_only)
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
        if !self.editable() {
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
        if !self.editable() {
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
        self.editable()
    }
}

/// One line of the text as an accessibility text run. A line break belongs to the line it ends.
#[derive(Debug, PartialEq, Eq)]
struct TextRunInfo {
    value: String,
    character_lengths: Vec<u8>,
}

/// The text as runs, with the selection as `(run, character in run)` for each end.
#[derive(Debug, PartialEq, Eq)]
struct A11yText {
    runs: Vec<TextRunInfo>,
    anchor: (usize, usize),
    focus: (usize, usize),
}

fn a11y_text(text: &str, selection: &Range<usize>, reversed: bool) -> A11yText {
    let mut starts = Vec::new();
    let mut runs: Vec<TextRunInfo> = Vec::new();
    let mut offset = 0;

    for line in text.split_inclusive('\n') {
        starts.push(offset);
        offset += line.len();
        runs.push(run_info(line));
    }

    // Nothing after a final line break, or no text at all: the cursor still needs a run.
    if text.is_empty() || text.ends_with('\n') {
        starts.push(text.len());
        runs.push(run_info(""));
    }

    let position = |offset: usize| {
        let run = starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let within = text
            .get(starts[run]..offset)
            .map_or(0, |part| part.chars().count());

        (run, within)
    };

    let (start, end) = (position(selection.start), position(selection.end));
    let (anchor, focus) = if reversed { (end, start) } else { (start, end) };

    A11yText {
        runs,
        anchor,
        focus,
    }
}

fn run_info(line: &str) -> TextRunInfo {
    TextRunInfo {
        value: line.to_owned(),
        character_lengths: line
            .chars()
            .map(|character| u8::try_from(character.len_utf8()).unwrap_or(u8::MAX))
            .collect(),
    }
}

/// Text runs for the field's node and the selection, so a screen reader can read the
/// text by line and follow the caret.
fn a11y_text_children(
    text: &str,
    selection: &Range<usize>,
    reversed: bool,
) -> impl FnOnce(&mut A11ySubtreeBuilder) + 'static {
    let described = a11y_text(text, selection, reversed);

    move |builder| {
        let ids: Vec<accesskit::NodeId> = (0..described.runs.len())
            .map(|index| builder.synthetic_node_id(index))
            .collect();

        for (id, run) in ids.iter().zip(described.runs) {
            let mut node = accesskit::Node::new(Role::TextRun);
            node.set_value(run.value);
            node.set_character_lengths(run.character_lengths);
            builder.push_child(*id, node);
        }

        let position = |(run, character_index): (usize, usize)| accesskit::TextPosition {
            node: ids[run],
            character_index,
        };

        builder
            .parent_node()
            .set_text_selection(accesskit::TextSelection {
                anchor: position(described.anchor),
                focus: position(described.focus),
            });
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

/// Keeps the ranges that are sorted, apart, non-empty and on char boundaries, so a bad
/// highlighter cannot make shaping panic. A range that ends past the text or inside a char
/// is cut back; the others that break the rules are dropped.
fn valid_highlights(
    text: &str,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut valid = Vec::with_capacity(highlights.len());
    let mut previous_end = 0;

    for (range, style) in highlights {
        let end = text.floor_char_boundary(range.end);
        let usable =
            range.start >= previous_end && range.start < end && text.is_char_boundary(range.start);

        if usable {
            valid.push((range.start..end, style));
            previous_end = end;
        }
    }

    valid
}

/// The runs that cover `len` bytes: `highlights` (valid, see [`valid_highlights`]) on top of
/// `color`, and the IME `marked` range underlined on top of both.
fn text_runs(
    len: usize,
    font: Font,
    color: Hsla,
    highlights: &[(Range<usize>, HighlightStyle)],
    marked: Option<Range<usize>>,
) -> Vec<TextRun> {
    let plain = TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    if len == 0 {
        return vec![plain];
    }

    let mut runs = Vec::with_capacity(highlights.len() * 2 + 1);
    let mut end = 0;

    for (range, style) in highlights {
        if range.start > end {
            runs.push(TextRun {
                len: range.start - end,
                ..plain.clone()
            });
        }

        runs.push(highlighted_run(&plain, range.len(), style));
        end = range.end;
    }

    if end < len {
        runs.push(TextRun {
            len: len - end,
            ..plain
        });
    }

    match marked.filter(|marked| marked.end <= len) {
        Some(marked) => underline_marked(runs, &marked),
        None => runs,
    }
}

fn highlighted_run(plain: &TextRun, len: usize, style: &HighlightStyle) -> TextRun {
    let mut font = plain.font.clone();

    if let Some(weight) = style.font_weight {
        font.weight = weight;
    }

    if let Some(font_style) = style.font_style {
        font.style = font_style;
    }

    TextRun {
        len,
        font,
        color: style.color.unwrap_or(plain.color),
        background_color: style.background_color,
        underline: style.underline,
        strikethrough: None,
    }
}

/// Splits the runs at the ends of `marked` and underlines the part inside.
fn underline_marked(runs: Vec<TextRun>, marked: &Range<usize>) -> Vec<TextRun> {
    let mut underlined = Vec::with_capacity(runs.len() + 2);
    let mut start = 0;

    for run in runs {
        let end = start + run.len;
        let inside_start = marked.start.clamp(start, end);
        let inside_end = marked.end.clamp(start, end);
        let parts = [
            (start..inside_start, false),
            (inside_start..inside_end, true),
            (inside_end..end, false),
        ];

        for (part, inside) in parts {
            if part.is_empty() {
                continue;
            }

            let underline = inside.then_some(UnderlineStyle {
                color: Some(run.color),
                thickness: px(1.),
                wavy: false,
            });

            underlined.push(TextRun {
                len: part.len(),
                underline: underline.or(run.underline),
                ..run.clone()
            });
        }

        start = end;
    }

    underlined
}

/// Paints a field's text, selection and cursor, and registers it for IME.
struct FieldText<const MULTILINE: bool> {
    field: Entity<TextField<MULTILINE>>,
}

struct FieldPaint {
    layout: Option<FieldLayout>,
    /// Some text has a background, which sits under the selection.
    backgrounds: bool,
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
        let field = self.field.read(cx);
        let rows = if MULTILINE { field.rows } else { 1 };

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = if MULTILINE && field.height == Height::Fill {
            relative(1.).into()
        } else {
            (window.line_height() * rows).into()
        };

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
        if self.field.read(cx).highlights.highlighter.is_some() {
            let colors = cx.theme().colors().clone();
            self.field
                .update(cx, |field, _| field.refresh_highlights(&colors));
        }

        let colors = cx.theme().colors();
        let field = self.field.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let focused = field.focus_handle.is_focused(window);
        let buffer = &field.buffer;
        let empty = buffer.text.is_empty();

        let (text, color, highlights, marked) = if empty {
            (field.placeholder.clone(), colors.faint, &[][..], None)
        } else {
            let text = SharedString::from(buffer.text.clone());
            let highlights = field.highlight_ranges();
            (text, style.color, highlights, buffer.marked.clone())
        };

        let backgrounds = highlights
            .iter()
            .any(|(_, style)| style.background_color.is_some());
        let runs = text_runs(text.len(), style.font(), color, highlights, marked);
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
            backgrounds,
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
            if paint.backgrounds {
                for laid in &layout.lines {
                    let origin = layout.origin + point(px(0.), laid.y);
                    let painted = laid.line.paint_background(
                        origin,
                        layout.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );

                    if let Err(error) = painted {
                        log::error!("cannot paint text field background: {error:#}");
                    }
                }
            }

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
    /// Goes up whenever `text` changes, so a result computed from the text can tell it is stale.
    revision: u64,
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
        self.revision += 1;
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
        self.revision += 1;

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

const MARKDOWN_SAMPLE: &str = "\
# Docs site

Add a **docs site** with a page per _component_.

- [x] Pick the `mdbook` layout
- [ ] Write the first page

> Keep it short

```sh
cargo run -p gallery
```

See [the roadmap](docs/ROADMAP.md).";

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

        let markdown = window.use_keyed_state("text-area-markdown", cx, |_, cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Plan", cx);
            area.set_fill(true, cx);
            area.set_highlighter(Some(markdown_highlights), cx);
            area.set_text_at_start(MARKDOWN_SAMPLE, cx);
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
                [
                    example("First message", preview_field(message)),
                    example("Markdown", div().w(px(360.)).h(px(260.)).child(markdown)),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

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

    #[test]
    fn a_text_with_two_lines_is_two_runs_and_the_break_ends_the_first() {
        let described = a11y_text("ab\ncd", &(0..0), false);
        let values: Vec<_> = described
            .runs
            .iter()
            .map(|run| run.value.as_str())
            .collect();

        assert_eq!(values, ["ab\n", "cd"]);
    }

    #[test]
    fn a_run_lists_the_byte_length_of_each_character() {
        let described = a11y_text("a\u{e9}\u{1f41d}", &(0..0), false);

        assert_eq!(described.runs[0].character_lengths, [1, 2, 4]);
    }

    #[test]
    fn empty_text_is_one_empty_run_with_the_caret_in_it() {
        let described = a11y_text("", &(0..0), false);

        assert_eq!(described.runs.len(), 1);
        assert_eq!((described.anchor, described.focus), ((0, 0), (0, 0)));
    }

    #[test]
    fn a_caret_at_the_start_of_the_second_line_is_in_the_second_run() {
        let described = a11y_text("ab\ncd", &(3..3), false);

        assert_eq!(described.focus, (1, 0));
    }

    #[test]
    fn a_caret_after_a_final_line_break_gets_an_empty_run() {
        let described = a11y_text("ab\n", &(3..3), false);

        assert_eq!((described.runs.len(), described.focus), (2, (1, 0)));
    }

    #[test]
    fn the_caret_counts_characters_not_bytes() {
        let described = a11y_text("\u{e9}\u{e9}x", &(4..4), false);

        assert_eq!(described.focus, (0, 2));
    }

    #[test]
    fn a_selection_puts_the_anchor_at_its_start_and_the_focus_at_its_end() {
        let described = a11y_text("hello world", &(0..5), false);

        assert_eq!((described.anchor, described.focus), ((0, 0), (0, 5)));
    }

    #[test]
    fn a_reversed_selection_puts_the_focus_at_its_start() {
        let described = a11y_text("hello world", &(0..5), true);

        assert_eq!((described.anchor, described.focus), ((0, 5), (0, 0)));
    }

    #[test]
    fn a_selection_can_cross_lines() {
        let described = a11y_text("ab\ncd", &(1..4), false);

        assert_eq!((described.anchor, described.focus), ((0, 1), (1, 1)));
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

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// A parent of a known height around a field.
    struct Frame<const MULTILINE: bool> {
        field: Entity<TextField<MULTILINE>>,
        height: Pixels,
    }

    impl<const MULTILINE: bool> Render for Frame<MULTILINE> {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().h(self.height).child(self.field.clone())
        }
    }

    fn framed<const MULTILINE: bool>(
        height: Pixels,
        cx: &mut TestAppContext,
    ) -> (Entity<TextField<MULTILINE>>, &mut VisualTestContext) {
        cx.update(crate::theme::init);

        let (frame, cx) = cx.add_window_view(|_, cx| Frame {
            field: cx.new(TextField::<MULTILINE>::new),
            height,
        });
        let field = frame.read_with(cx, |frame, _| frame.field.clone());

        (field, cx)
    }

    fn painted_height<const MULTILINE: bool>(
        field: &Entity<TextField<MULTILINE>>,
        cx: &mut VisualTestContext,
    ) -> Option<Pixels> {
        field.read_with(cx, |field, _| {
            field
                .layout
                .as_ref()
                .map(|layout| layout.bounds.size.height)
        })
    }

    #[gpui::test]
    fn popup_children_render_inside_the_focused_element(cx: &mut TestAppContext) {
        let (input, cx) = framed::<false>(px(40.), cx);
        let child = cx.update(|_, cx| cx.focus_handle());
        let child_handle = child.clone();

        input.update(cx, |input, cx| {
            input.set_popup(
                Some(move |_: &mut Window, _: &mut App| {
                    vec![
                        div()
                            .id("popup-child")
                            .track_focus(&child_handle)
                            .into_any_element(),
                    ]
                }),
                cx,
            );
        });
        draw(cx);
        cx.update(|window, cx| window.focus(&child, cx));

        let contains = input.read_with(cx, |input, _| input.focus_handle.clone());
        assert!(cx.update(|window, cx| contains.contains_focused(window, cx)));
    }

    #[gpui::test]
    fn a_field_without_a_popup_does_not_contain_other_focus(cx: &mut TestAppContext) {
        let (input, cx) = framed::<false>(px(40.), cx);
        let other = cx.update(|_, cx| cx.focus_handle());

        draw(cx);
        cx.update(|window, cx| window.focus(&other, cx));

        let handle = input.read_with(cx, |input, _| input.focus_handle.clone());
        assert!(!cx.update(|window, cx| handle.contains_focused(window, cx)));
    }

    #[gpui::test]
    fn a_filled_area_takes_the_height_of_its_parent(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| area.set_fill(true, cx));
        draw(cx);

        // The parent's height minus the 8px padding above and below and the 1px borders.
        assert_eq!(painted_height(&area, cx), Some(px(300. - 16. - 2.)));
    }

    /// A header of 20px and a body that grows, as in a tab.
    struct Tab {
        area: Entity<TextArea>,
    }

    impl Render for Tab {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .h(px(300.))
                .child(div().h(px(20.)))
                .child(div().flex_1().child(self.area.clone()))
        }
    }

    #[gpui::test]
    fn a_filled_area_takes_the_height_of_a_growing_parent(cx: &mut TestAppContext) {
        cx.update(crate::theme::init);
        let (tab, cx) = cx.add_window_view(|_, cx| Tab {
            area: cx.new(TextArea::new),
        });
        let area = tab.read_with(cx, |tab, _| tab.area.clone());

        area.update(cx, |area, cx| area.set_fill(true, cx));
        draw(cx);

        assert_eq!(painted_height(&area, cx), Some(px(300. - 20. - 16. - 2.)));
    }

    #[gpui::test]
    fn an_area_without_fill_keeps_its_rows(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| {
            area.set_fill(true, cx);
            area.set_fill(false, cx);
        });
        draw(cx);

        assert_eq!(painted_height(&area, cx), Some(LINE_HEIGHT * DEFAULT_ROWS));
    }

    #[gpui::test]
    fn a_text_input_ignores_fill(cx: &mut TestAppContext) {
        let (input, cx) = framed::<false>(px(300.), cx);

        input.update(cx, |input, cx| input.set_fill(true, cx));
        draw(cx);

        assert_eq!(painted_height(&input, cx), Some(LINE_HEIGHT));
    }

    #[gpui::test]
    fn a_filled_area_scrolls_when_the_text_is_taller_than_the_parent(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(100.), cx);
        let lines = (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>();

        area.update(cx, |area, cx| {
            area.set_fill(true, cx);
            area.set_text(lines.join("\n"), cx);
        });
        draw(cx);

        let offset = area.read_with(cx, |area, _| area.scroll.offset.y);

        // The cursor is at the end: 30 lines of 18px, less the parent's 100px minus its padding
        // and borders.
        assert_eq!(offset, px(30. * 18. - (100. - 16. - 2.)));
    }

    thread_local! {
        static HIGHLIGHT_RUNS: Cell<usize> = const { Cell::new(0) };
    }

    fn counting_highlighter(
        text: &str,
        colors: &ThemeColors,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        HIGHLIGHT_RUNS.with(|runs| runs.set(runs.get() + 1));

        vec![(0..text.len(), HighlightStyle::color(colors.accent))]
    }

    fn highlight_runs() -> usize {
        HIGHLIGHT_RUNS.with(Cell::get)
    }

    fn highlighted_area(cx: &mut TestAppContext) -> (Entity<TextArea>, &mut VisualTestContext) {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| {
            area.set_text("some text", cx);
            area.set_highlighter(Some(counting_highlighter), cx);
        });
        draw(cx);

        (area, cx)
    }

    #[gpui::test]
    fn the_highlighter_runs_once_for_two_paints_of_the_same_text(cx: &mut TestAppContext) {
        let (_area, cx) = highlighted_area(cx);

        draw(cx);

        assert_eq!(highlight_runs(), 1);
    }

    #[gpui::test]
    fn the_highlighter_runs_again_after_typing(cx: &mut TestAppContext) {
        let (area, cx) = highlighted_area(cx);

        area.update_in(cx, |area, window, cx| {
            area.replace_text_in_range(None, "!", window, cx);
        });
        draw(cx);

        assert_eq!(highlight_runs(), 2);
    }

    #[gpui::test]
    fn the_highlighter_runs_again_after_undo(cx: &mut TestAppContext) {
        let (area, cx) = highlighted_area(cx);

        area.update_in(cx, |area, window, cx| {
            area.replace_text_in_range(None, "!", window, cx);
        });
        draw(cx);
        area.update_in(cx, |area, window, cx| {
            area.undo(&action::Undo, window, cx);
        });
        draw(cx);

        assert_eq!(highlight_runs(), 3);
    }

    #[gpui::test]
    fn the_highlighter_runs_again_after_set_text(cx: &mut TestAppContext) {
        let (area, cx) = highlighted_area(cx);

        area.update(cx, |area, cx| area.set_text("other text", cx));
        draw(cx);

        assert_eq!(highlight_runs(), 2);
    }

    #[gpui::test]
    fn the_highlighter_runs_again_after_the_theme_changes(cx: &mut TestAppContext) {
        let (_area, cx) = highlighted_area(cx);

        cx.update(|_, cx| crate::theme::set_accent(crate::theme::Accent::Honey, cx));
        draw(cx);

        assert_eq!(highlight_runs(), 2);
    }

    fn broken_highlighter(_: &str, colors: &ThemeColors) -> Vec<(Range<usize>, HighlightStyle)> {
        let style = HighlightStyle::color(colors.accent);

        // For "é👍🏽b": inside a char, ends inside a char, overlapping, empty and past the end.
        vec![
            (1..2, style),
            (0..3, style),
            (2..5, style),
            (0..1, style),
            (4..4, style),
            (3..99, style),
            (10..99, style),
        ]
    }

    #[gpui::test]
    fn a_broken_highlighter_leaves_only_whole_char_ranges_in_the_text(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| {
            area.set_text("é👍🏽b", cx);
            area.set_highlighter(Some(broken_highlighter), cx);
        });
        draw(cx);

        let ranges: Vec<_> = area.read_with(cx, |area, _| {
            area.highlight_ranges()
                .iter()
                .map(|(range, _)| range.clone())
                .collect()
        });

        assert_eq!(ranges, [0..2, 10..11]);
    }

    #[test]
    fn highlights_keep_only_sorted_whole_char_ranges_inside_the_text() {
        let style = HighlightStyle::default();
        let text = "aéb";
        let highlights = vec![
            (0..1, style),
            (1..2, style),
            (2..3, style),
            (1..3, style),
            (2..4, style),
            (3..99, style),
            (0..1, style),
        ];

        let valid: Vec<_> = valid_highlights(text, highlights)
            .into_iter()
            .map(|(range, _)| range)
            .collect();

        assert_eq!(valid, [0..1, 1..3, 3..4]);
    }

    fn font() -> Font {
        gpui::font("Geist")
    }

    #[test]
    fn runs_without_highlights_are_one_run() {
        let runs = text_runs(5, font(), Hsla::default(), &[], None);

        assert_eq!(runs.iter().map(|run| run.len).collect::<Vec<_>>(), [5]);
    }

    #[test]
    fn runs_follow_the_highlights() {
        let highlights = [(
            2..4,
            HighlightStyle {
                color: Some(gpui::red()),
                font_weight: Some(gpui::FontWeight::BOLD),
                font_style: Some(gpui::FontStyle::Italic),
                background_color: Some(gpui::blue()),
                ..HighlightStyle::default()
            },
        )];

        let runs = text_runs(6, font(), Hsla::default(), &highlights, None);

        assert_eq!(
            runs.iter().map(|run| run.len).collect::<Vec<_>>(),
            [2, 2, 2]
        );
        assert_eq!(
            (
                runs[1].color,
                runs[1].font.weight,
                runs[1].font.style,
                runs[1].background_color
            ),
            (
                gpui::red(),
                gpui::FontWeight::BOLD,
                gpui::FontStyle::Italic,
                Some(gpui::blue())
            )
        );
    }

    #[test]
    fn the_plain_runs_between_highlights_keep_the_text_color() {
        let style = HighlightStyle::color(gpui::red());
        let highlights = [(1..2, style), (3..4, style)];

        let runs = text_runs(5, font(), gpui::green(), &highlights, None);

        assert_eq!(
            runs.iter().map(|run| run.color).collect::<Vec<_>>(),
            [
                gpui::green(),
                gpui::red(),
                gpui::green(),
                gpui::red(),
                gpui::green()
            ]
        );
    }

    #[test]
    fn the_marked_range_is_underlined_on_top_of_a_highlight() {
        let highlights = [(
            0..6,
            HighlightStyle {
                background_color: Some(gpui::blue()),
                underline: Some(UnderlineStyle {
                    wavy: true,
                    ..UnderlineStyle::default()
                }),
                ..HighlightStyle::default()
            },
        )];

        let runs = text_runs(6, font(), Hsla::default(), &highlights, Some(2..4));

        assert_eq!(
            runs.iter()
                .map(|run| (run.len, run.underline.map(|underline| underline.wavy)))
                .collect::<Vec<_>>(),
            [(2, Some(true)), (2, Some(false)), (2, Some(true))]
        );
    }

    #[test]
    fn the_marked_range_keeps_the_highlight_background() {
        let highlights = [(
            0..6,
            HighlightStyle {
                background_color: Some(gpui::blue()),
                ..HighlightStyle::default()
            },
        )];

        let runs = text_runs(6, font(), Hsla::default(), &highlights, Some(2..4));

        assert_eq!(runs[1].background_color, Some(gpui::blue()));
    }

    fn cursor_of(text: &str, offset: usize, cx: &mut TestAppContext) -> usize {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| {
            area.set_text(text, cx);
            area.set_cursor(offset, cx);
        });

        area.read_with(cx, |area, _| area.buffer.cursor())
    }

    #[gpui::test]
    fn set_cursor_puts_the_cursor_at_the_offset(cx: &mut TestAppContext) {
        assert_eq!(cursor_of("abc", 1, cx), 1);
    }

    #[gpui::test]
    fn set_cursor_clamps_past_the_end(cx: &mut TestAppContext) {
        assert_eq!(cursor_of("abc", 99, cx), 3);
    }

    #[gpui::test]
    fn set_cursor_moves_back_to_a_char_boundary(cx: &mut TestAppContext) {
        // "é" takes the bytes 1 and 2 of "aéb".
        assert_eq!(cursor_of("aéb", 2, cx), 1);
    }

    #[gpui::test]
    fn set_cursor_clears_the_selection(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(300.), cx);

        area.update(cx, |area, cx| {
            area.set_text("abc", cx);
            area.buffer.select_range(0..3);
            area.set_cursor(1, cx);
        });

        assert_eq!(
            area.read_with(cx, |area, _| area.buffer.selection.clone()),
            1..1
        );
    }

    #[gpui::test]
    fn set_cursor_scrolls_so_the_cursor_shows_on_the_next_paint(cx: &mut TestAppContext) {
        let (area, cx) = framed::<true>(px(100.), cx);
        let lines = (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>();

        area.update(cx, |area, cx| {
            area.set_fill(true, cx);
            area.set_text(lines.join("\n"), cx);
        });
        draw(cx);
        let at_the_end = area.read_with(cx, |area, _| area.scroll.offset.y);

        area.update(cx, |area, cx| area.set_cursor(0, cx));
        draw(cx);
        let at_the_start = area.read_with(cx, |area, _| area.scroll.offset.y);

        assert_eq!((at_the_end > px(0.), at_the_start), (true, px(0.)));
    }
}
