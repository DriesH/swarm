//! Message text as Markdown: parsed once into blocks with their styled text, then drawn
//! from those blocks.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, BorderStyle, Bounds, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveText, Pixels, SharedString, StyledText, UnderlineStyle, canvas, div, point, px,
    quad, relative,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;
use swarm_ui::{ThemeColors, Tooltip};

/// A folded message draws this many blocks. Ten blocks are always taller than the folded
/// height, so a message with more blocks is tall without measuring it.
pub(crate) const FOLD_BLOCKS: usize = 10;

/// Space between two blocks, in pixels.
const BLOCK_GAP: f32 = 6.;

/// Width of the marker column of a list item, in pixels.
const MARKER_WIDTH: f32 = 18.;

/// Space between the marker column and the text, in pixels.
const MARKER_GAP: f32 = 8.;

/// Extra indent of each list level, in pixels.
const LEVEL_INDENT: f32 = 16.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl Span {
    /// The link target when a click may open it: only web addresses.
    pub(crate) fn openable_link(&self) -> Option<&str> {
        let link = self.link.as_deref()?;
        let lower = link.to_ascii_lowercase();

        (lower.starts_with("https://") || lower.starts_with("http://")).then_some(link)
    }
}

/// A styled stretch of the text. The colors come from the theme when it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    range: Range<usize>,
    bold: bool,
    italic: bool,
    link: bool,
}

/// The spans of one block, joined into the text GPUI draws. Built once at parse time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Inline {
    pub spans: Vec<Span>,
    text: SharedString,
    runs: Vec<Run>,
    mono: Vec<(Range<usize>, SharedString)>,
    boxes: Vec<Range<usize>>,
    links: Rc<[(Range<usize>, String)]>,
}

impl Inline {
    pub(crate) fn new(spans: Vec<Span>) -> Self {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut mono = Vec::new();
        let mut boxes = Vec::new();
        let mut links = Vec::new();

        for span in &spans {
            let start = text.len();
            text.push_str(&span.text);
            let range = start..text.len();
            if range.is_empty() {
                continue;
            }

            let link = span.openable_link();
            if span.code {
                mono.push((range.clone(), SharedString::new_static(MONO_FONT)));
                boxes.push(range.clone());
            }
            if let Some(url) = link {
                links.push((range.clone(), url.to_owned()));
            }
            if span.bold || span.italic || link.is_some() {
                runs.push(Run {
                    range,
                    bold: span.bold,
                    italic: span.italic,
                    link: link.is_some(),
                });
            }
        }

        Self {
            spans,
            text: SharedString::from(text),
            runs,
            mono,
            boxes,
            links: links.into(),
        }
    }

    fn heading(spans: &[Span]) -> Self {
        Self::new(
            spans
                .iter()
                .map(|span| Span {
                    text: span.text.to_uppercase(),
                    ..span.clone()
                })
                .collect(),
        )
    }

    fn highlights(&self, colors: &ThemeColors) -> Vec<(Range<usize>, HighlightStyle)> {
        self.runs
            .iter()
            .map(|run| {
                let mut style = HighlightStyle::default();

                if run.bold {
                    style.font_weight = Some(FontWeight::SEMIBOLD);
                }
                if run.italic {
                    style.font_style = Some(FontStyle::Italic);
                }
                if run.link {
                    style.color = Some(colors.accent);
                    style.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(colors.accent),
                        wavy: false,
                    });
                }

                (run.range.clone(), style)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Bullet,
    Number(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Paragraph(Inline),
    Heading(Inline),
    /// One list item, or a block that continues one: it has no marker but keeps the
    /// item's depth. Nested lists are items with a higher `depth`.
    Item {
        marker: Option<Marker>,
        depth: usize,
        inline: Inline,
    },
    /// A code block, indented under its list item when `depth` is set.
    Code {
        depth: Option<usize>,
        code: String,
    },
}

/// A parsed message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Markdown {
    pub blocks: Vec<Block>,
}

impl Markdown {
    /// True when the message has more blocks than a folded one draws, which makes it tall.
    pub(crate) fn overflows(&self) -> bool {
        self.blocks.len() > FOLD_BLOCKS
    }
}

struct OpenItem {
    marker: Marker,
    depth: usize,
    started: bool,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    lists: Vec<Option<u64>>,
    items: Vec<OpenItem>,
    heading: bool,
    code: Option<String>,
    bold: usize,
    italic: usize,
    link: Option<String>,
}

impl Builder {
    fn push_text(&mut self, text: &str, code: bool) {
        if let Some(block) = &mut self.code {
            block.push_str(text);
            return;
        }

        self.spans.push(Span {
            text: text.to_owned(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            code,
            link: self.link.clone(),
        });
    }

    fn flush(&mut self) {
        let mut spans = std::mem::take(&mut self.spans);

        while let Some(last) = spans.last_mut() {
            let kept = last.text.trim_end_matches('\n').len();
            last.text.truncate(kept);

            if !last.text.is_empty() {
                break;
            }
            spans.pop();
        }
        if spans.is_empty() {
            return;
        }

        let block = if self.heading {
            Block::Heading(Inline::heading(&spans))
        } else if let Some(item) = self.items.last_mut() {
            let marker = (!item.started).then_some(item.marker);
            item.started = true;

            Block::Item {
                marker,
                depth: item.depth,
                inline: Inline::new(spans),
            }
        } else {
            Block::Paragraph(Inline::new(spans))
        };

        self.blocks.push(block);
    }

    fn start_item(&mut self) {
        let depth = self.lists.len().saturating_sub(1);
        let marker = match self.lists.last_mut() {
            Some(Some(next)) => {
                let marker = Marker::Number(*next);
                *next += 1;

                marker
            }
            _ => Marker::Bullet,
        };

        self.items.push(OpenItem {
            marker,
            depth,
            started: false,
        });
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::HtmlBlock) | Event::End(TagEnd::Paragraph | TagEnd::HtmlBlock) => {
                self.flush();
            }
            Event::Start(Tag::List(first)) => {
                self.flush();
                self.lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
            }
            Event::Start(Tag::Heading { .. }) => {
                self.flush();
                self.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.heading = false;
            }
            Event::Start(Tag::Item) => {
                self.flush();
                self.start_item();
            }
            Event::End(TagEnd::Item) => {
                self.flush();
                self.items.pop();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush();
                self.code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = self.code.take() {
                    self.blocks.push(Block::Code {
                        depth: self.items.last().map(|item| item.depth),
                        code: code.trim_end_matches('\n').to_owned(),
                    });
                }
            }
            Event::Start(Tag::Strong) => self.bold += 1,
            Event::End(TagEnd::Strong) => self.bold = self.bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => self.italic += 1,
            Event::End(TagEnd::Emphasis) => self.italic = self.italic.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => self.link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => self.link = None,
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push_text(&text, false);
            }
            Event::Code(text) => self.push_text(&text, true),
            Event::SoftBreak | Event::HardBreak => self.push_text("\n", false),
            _ => {}
        }
    }
}

/// Parses message text. Call it once per message and keep the result.
pub(crate) fn parse(text: &str) -> Markdown {
    let mut builder = Builder::default();

    for event in Parser::new(text) {
        builder.handle(event);
    }
    builder.flush();

    Markdown {
        blocks: builder.blocks,
    }
}

/// Draws the blocks one under the other. A folded message draws only the first
/// [`FOLD_BLOCKS`]. The caller's element id scopes the block ids.
pub(crate) fn render(markdown: &Markdown, folded: bool, colors: &ThemeColors) -> AnyElement {
    let shown = if folded { FOLD_BLOCKS } else { usize::MAX };
    let blocks: Vec<AnyElement> = markdown
        .blocks
        .iter()
        .take(shown)
        .enumerate()
        .map(|(index, block)| render_block(index, block, colors))
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(BLOCK_GAP))
        .text_size(px(13.))
        .line_height(relative(1.55))
        .text_color(colors.fg)
        .children(blocks)
        .into_any_element()
}

/// The space left of a list item's text, in pixels.
fn item_indent(depth: usize) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a handful of nesting levels")]
    let levels = depth as f32;

    levels * LEVEL_INDENT
}

fn render_block(index: usize, block: &Block, colors: &ThemeColors) -> AnyElement {
    match block {
        Block::Paragraph(inline) => render_inline(inline, ("paragraph", index).into(), colors),
        Block::Heading(inline) => div()
            .mt(px(4.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(colors.faint)
            .child(render_inline(inline, ("heading", index).into(), colors))
            .into_any_element(),
        Block::Item {
            marker,
            depth,
            inline,
        } => {
            let marker = match marker {
                Some(Marker::Bullet) => "\u{2022}".to_owned(),
                Some(Marker::Number(number)) => format!("{number}."),
                None => String::new(),
            };

            div()
                .flex()
                .gap(px(MARKER_GAP))
                .pl(px(item_indent(*depth)))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(MARKER_WIDTH))
                        .text_color(colors.faint)
                        .child(marker),
                )
                .child(div().flex_1().min_w_0().child(render_inline(
                    inline,
                    ("item", index).into(),
                    colors,
                )))
                .into_any_element()
        }
        Block::Code { depth, code } => {
            let indent = depth.map_or(0., |depth| item_indent(depth) + MARKER_WIDTH + MARKER_GAP);

            div()
                .flex()
                .flex_col()
                .ml(px(indent))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(colors.line)
                .bg(colors.term)
                .overflow_hidden()
                .font_family(MONO_FONT)
                .text_size(px(11.5))
                .line_height(relative(1.5))
                .children(code.split('\n').map(|line| {
                    div().whitespace_nowrap().child(if line.is_empty() {
                        SharedString::new_static(" ")
                    } else {
                        SharedString::from(line.to_owned())
                    })
                }))
                .into_any_element()
        }
    }
}

/// One run of text with its inline styles. Inline code gets a box behind it.
fn render_inline(inline: &Inline, id: ElementId, colors: &ThemeColors) -> AnyElement {
    let styled = StyledText::new(inline.text.clone())
        .with_highlights(inline.highlights(colors))
        .with_font_family_overrides(inline.mono.clone());
    let layout = styled.layout().clone();

    let body: AnyElement = if inline.links.is_empty() {
        styled.into_any_element()
    } else {
        let ranges: Vec<Range<usize>> = inline
            .links
            .iter()
            .map(|(range, _)| range.clone())
            .collect();
        let (opened, shown) = (Rc::clone(&inline.links), Rc::clone(&inline.links));

        InteractiveText::new(id, styled)
            .on_click(ranges, move |index, _, cx| {
                if let Some((_, url)) = opened.get(index) {
                    cx.open_url(url);
                }
            })
            .tooltip(move |index, _, cx| {
                link_at(&shown, index).map(|url| Tooltip::text(url.to_owned(), cx))
            })
            .into_any_element()
    };

    if inline.boxes.is_empty() {
        return body;
    }

    let (fill, border) = (colors.soft, colors.line);
    let (text, boxes) = (inline.text.clone(), inline.boxes.clone());

    div()
        .relative()
        .child(
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    for range in &boxes {
                        for line in code_lines(&layout, &text, range) {
                            paint_code_box(&layout, &line, fill, border, window);
                        }
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .child(body)
        .into_any_element()
}

/// The address of the link that covers byte `index` of the text.
fn link_at(links: &[(Range<usize>, String)], index: usize) -> Option<&str> {
    links
        .iter()
        .find(|(range, _)| range.contains(&index))
        .map(|(_, url)| url.as_str())
}

/// One visual line of a code span: where its box starts and ends, and the top of the line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CodeLine {
    left: Pixels,
    right: Pixels,
    top: Pixels,
}

/// The visual lines a code span covers, from the position of each of its characters.
/// A character starts a new line when the position after it is on another line, because
/// the position of the first character of a wrapped line reads as the end of the line
/// before. Trailing spaces do not widen a line.
fn code_lines(layout: &gpui::TextLayout, text: &str, range: &Range<usize>) -> Vec<CodeLine> {
    let Some(span) = text.get(range.clone()) else {
        return Vec::new();
    };
    let mut lines: Vec<CodeLine> = Vec::new();

    for (offset, character) in span.char_indices() {
        let index = range.start + offset;
        let (Some(start), Some(end)) = (
            layout.position_for_index(index),
            layout.position_for_index(index + character.len_utf8()),
        ) else {
            continue;
        };

        if start.y != end.y {
            let left = layout.bounds().left();

            lines.push(CodeLine {
                left,
                right: left,
                top: end.y,
            });
        } else if lines.is_empty() {
            lines.push(CodeLine {
                left: start.x,
                right: start.x,
                top: start.y,
            });
        }

        if let Some(line) = lines.last_mut()
            && !character.is_whitespace()
        {
            line.right = end.x;
        }
    }

    lines
}

fn paint_code_box(
    layout: &gpui::TextLayout,
    line: &CodeLine,
    fill: Hsla,
    border: Hsla,
    window: &mut Window,
) {
    let height = layout.line_height();
    let bounds = Bounds::from_corners(
        point(line.left - px(2.), line.top + px(1.)),
        point(line.right + px(2.), line.top + height - px(1.)),
    );

    window.paint_quad(quad(
        bounds,
        px(4.),
        fill,
        px(1.),
        border,
        BorderStyle::Solid,
    ));
}

/// The parsed messages of a thread, shared with the list rows.
pub(crate) type Parsed = Rc<[Rc<Markdown>]>;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, TextLayout};

    fn plain(text: &str) -> Span {
        Span {
            text: text.to_owned(),
            bold: false,
            italic: false,
            code: false,
            link: None,
        }
    }

    fn code(text: &str) -> Span {
        Span {
            code: true,
            ..plain(text)
        }
    }

    fn item(marker: Option<Marker>, depth: usize, text: &str) -> Block {
        Block::Item {
            marker,
            depth,
            inline: Inline::new(vec![plain(text)]),
        }
    }

    #[test]
    fn a_heading_a_list_and_inline_code_become_blocks() {
        let parsed = parse("Wire `pty`.\n\n## Scope\n\n- one\n- two with `code`\n");

        assert_eq!(
            parsed.blocks,
            [
                Block::Paragraph(Inline::new(vec![plain("Wire "), code("pty"), plain(".")])),
                Block::Heading(Inline::new(vec![plain("SCOPE")])),
                item(Some(Marker::Bullet), 0, "one"),
                Block::Item {
                    marker: Some(Marker::Bullet),
                    depth: 0,
                    inline: Inline::new(vec![plain("two with "), code("code")]),
                },
            ]
        );
        assert!(!parsed.overflows());
    }

    #[test]
    fn numbered_and_nested_items_keep_their_marker_and_depth() {
        let parsed = parse("3. three\n4. four\n   - inner\n");

        let markers: Vec<(Option<Marker>, usize)> = parsed
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Item { marker, depth, .. } => Some((*marker, *depth)),
                _ => None,
            })
            .collect();

        assert_eq!(
            markers,
            [
                (Some(Marker::Number(3)), 0),
                (Some(Marker::Number(4)), 0),
                (Some(Marker::Bullet), 1)
            ]
        );
    }

    #[test]
    fn a_code_block_and_text_inside_an_item_draw_the_marker_once() {
        let parsed = parse("1. step one\n\n   ```\n   code\n   ```\n\n   more text\n2. two\n");

        assert_eq!(
            parsed.blocks,
            [
                item(Some(Marker::Number(1)), 0, "step one"),
                Block::Code {
                    depth: Some(0),
                    code: "code".to_owned(),
                },
                item(None, 0, "more text"),
                item(Some(Marker::Number(2)), 0, "two"),
            ]
        );
    }

    #[test]
    fn text_after_a_nested_list_keeps_the_indent_of_its_item() {
        let parsed = parse("- a\n  - b\n\n  after nested\n- c\n");

        assert_eq!(
            parsed.blocks,
            [
                item(Some(Marker::Bullet), 0, "a"),
                item(Some(Marker::Bullet), 1, "b"),
                item(None, 0, "after nested"),
                item(Some(Marker::Bullet), 0, "c"),
            ]
        );
    }

    #[test]
    fn a_fenced_block_keeps_its_lines() {
        let parsed = parse("```rust\nlet a = 1;\n\nlet b = 2;\n```\n");

        assert_eq!(
            parsed.blocks,
            [Block::Code {
                depth: None,
                code: "let a = 1;\n\nlet b = 2;".to_owned(),
            }]
        );
    }

    #[test]
    fn an_html_block_is_its_own_plain_text_block() {
        let parsed = parse("before\n<div>\nhi\n</div>\n\nafter\n");

        let texts: Vec<&str> = parsed
            .blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph(inline) => inline.text.as_ref(),
                other => panic!("expected paragraphs, got {other:?}"),
            })
            .collect();

        assert_eq!(texts, ["before", "<div>\nhi\n</div>", "after"]);
    }

    #[test]
    fn bold_italic_and_links_set_their_span_style() {
        let parsed = parse("**big** and *slanted* in [docs](https://example.com/a).");

        let Some(Block::Paragraph(inline)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };
        let spans = &inline.spans;

        assert!(spans[0].bold && !spans[0].italic);
        assert!(spans[2].italic && !spans[2].bold);
        let link = spans.iter().find(|span| span.link.is_some());
        assert_eq!(
            link.and_then(Span::openable_link),
            Some("https://example.com/a")
        );
        assert_eq!(inline.links.len(), 1);
    }

    #[test]
    fn only_web_links_can_be_opened() {
        let parsed = parse("[a](file:///etc/passwd) [b](javascript:alert(1)) [c](HTTP://x.dev)");

        let Some(Block::Paragraph(inline)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };
        let openable: Vec<Option<&str>> = inline
            .spans
            .iter()
            .filter(|span| span.link.is_some())
            .map(Span::openable_link)
            .collect();

        assert_eq!(openable, [None, None, Some("HTTP://x.dev")]);
        assert_eq!(inline.links.len(), 1);
    }

    #[test]
    fn a_message_with_more_than_the_folded_blocks_overflows() {
        let ten = parse(&"a block\n\n".repeat(FOLD_BLOCKS));
        let eleven = parse(&"a block\n\n".repeat(FOLD_BLOCKS + 1));

        assert!(!ten.overflows());
        assert!(eleven.overflows());
    }

    #[test]
    fn a_soft_line_break_stays_a_line_break() {
        let parsed = parse("first\nsecond");

        let Some(Block::Paragraph(inline)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };

        assert_eq!(inline.text.as_ref(), "first\nsecond");
    }

    #[test]
    fn a_link_shows_its_address_and_other_text_shows_none() {
        let parsed = parse("see [docs](https://example.com/a) or [local](file:///x) now");

        let Some(Block::Paragraph(inline)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };
        let text = inline.text.as_ref();
        let at = |needle: &str| link_at(&inline.links, text.find(needle).unwrap_or(usize::MAX));

        assert_eq!(at("docs"), Some("https://example.com/a"));
        assert_eq!(at("see"), None);
        assert_eq!(at("local"), None);
    }

    const WRAPPED: &str = "alpha beta gamma delta epsilon zeta";

    struct WrapProbe {
        layout: Option<TextLayout>,
    }

    impl Render for WrapProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let styled = StyledText::new(WRAPPED);
            self.layout = Some(styled.layout().clone());

            div().w(px(60.)).child(styled)
        }
    }

    fn wrapped_layout(cx: &mut TestAppContext) -> TextLayout {
        cx.update(swarm_ui::init);
        let (view, cx) = cx.add_window_view(|_, _| WrapProbe { layout: None });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        view.read_with(cx, |probe, _| probe.layout.clone())
            .expect("the layout")
    }

    fn lines_of(layout: &TextLayout, needle: &str) -> Vec<CodeLine> {
        let start = WRAPPED.find(needle).expect("the needle");

        code_lines(layout, WRAPPED, &(start..start + needle.len()))
    }

    #[gpui::test]
    fn a_code_span_that_wraps_ends_each_box_at_its_last_word(cx: &mut TestAppContext) {
        let layout = wrapped_layout(cx);
        let edges = layout.bounds();

        let lines = lines_of(&layout, "gamma delta");

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].left, px(0.));
        assert!(lines[0].right < edges.right(), "{lines:?}");
        assert_eq!(lines[1].left, edges.left());
        assert!(lines[1].top > lines[0].top);
    }

    #[gpui::test]
    fn a_code_span_over_three_lines_gets_a_box_for_each(cx: &mut TestAppContext) {
        let layout = wrapped_layout(cx);

        let lines = lines_of(&layout, "beta gamma delta");

        let tops: Vec<Pixels> = lines.iter().map(|line| line.top).collect();
        assert_eq!(tops, [px(26.), px(52.), px(78.)]);
        assert!(lines.iter().all(|line| line.left <= px(10.)));
    }

    #[gpui::test]
    fn a_code_span_that_starts_a_wrapped_line_paints_nothing_on_the_line_before(
        cx: &mut TestAppContext,
    ) {
        let layout = wrapped_layout(cx);

        let lines = lines_of(&layout, "delta");

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].top, px(78.));
        assert_eq!(lines[0].left, layout.bounds().left());
        assert!(lines[0].right > lines[0].left);
    }

    #[gpui::test]
    fn spaces_at_the_end_of_a_line_do_not_widen_its_box(cx: &mut TestAppContext) {
        let layout = wrapped_layout(cx);

        let lines = lines_of(&layout, "gamma ");

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!((f32::from(lines[0].right) - 48.).abs() < 0.1, "{lines:?}");
    }
}
