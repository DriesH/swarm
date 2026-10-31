//! Message text as Markdown: parsed once into blocks, then drawn from those blocks.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, BorderStyle, Bounds, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveText, SharedString, StyledText, UnderlineStyle, canvas, div, point, px, quad,
    relative,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use swarm_ui::ThemeColors;
use swarm_ui::fonts::MONO_FONT;
use swarm_ui::prelude::*;

/// A message with more lines than this folds.
pub(crate) const FOLD_LINES: usize = 8;

/// Characters that fit in one line of the inspector at its default width.
const LINE_CHARACTERS: usize = 52;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Bullet,
    Number(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Paragraph(Vec<Span>),
    Heading(Vec<Span>),
    /// One list item. Nested lists are items with a higher `depth`.
    Item {
        marker: Marker,
        depth: usize,
        spans: Vec<Span>,
    },
    Code(String),
}

/// A parsed message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Markdown {
    pub blocks: Vec<Block>,
    /// True when the text takes more than [`FOLD_LINES`] lines, so it folds.
    pub tall: bool,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    lists: Vec<Option<u64>>,
    item: Option<(Marker, usize)>,
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
        let spans = std::mem::take(&mut self.spans);
        if spans.is_empty() {
            return;
        }

        self.blocks.push(if self.heading {
            Block::Heading(spans)
        } else if let Some((marker, depth)) = self.item {
            Block::Item {
                marker,
                depth,
                spans,
            }
        } else {
            Block::Paragraph(spans)
        });
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

        self.item = Some((marker, depth));
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Paragraph) => {
                if self.item.is_some() && !self.spans.is_empty() {
                    self.push_text("\n", false);
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if self.item.is_none() {
                    self.flush();
                }
            }
            Event::Start(Tag::Heading { .. }) => {
                self.flush();
                self.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.heading = false;
            }
            Event::Start(Tag::List(first)) => {
                self.flush();
                self.lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
                self.item = None;
            }
            Event::Start(Tag::Item) => {
                self.flush();
                self.start_item();
            }
            Event::End(TagEnd::Item) => {
                self.flush();
                self.item = None;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush();
                self.code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = self.code.take() {
                    self.blocks
                        .push(Block::Code(code.trim_end_matches('\n').to_owned()));
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

    let lines: usize = builder.blocks.iter().map(estimated_lines).sum();

    Markdown {
        blocks: builder.blocks,
        tall: lines > FOLD_LINES,
    }
}

fn wrapped_lines(text: &str) -> usize {
    text.split('\n')
        .map(|line| line.chars().count().div_ceil(LINE_CHARACTERS).max(1))
        .sum()
}

fn joined(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn estimated_lines(block: &Block) -> usize {
    match block {
        Block::Paragraph(spans) => wrapped_lines(&joined(spans)),
        Block::Heading(_) => 1,
        Block::Item { spans, depth, .. } => {
            let indent = "   ".repeat(depth + 1);

            wrapped_lines(&format!("{indent}{}", joined(spans)))
        }
        Block::Code(code) => wrapped_lines(code),
    }
}

/// Draws the blocks one under the other. The caller's element id scopes the block ids.
pub(crate) fn render(markdown: &Markdown, colors: &ThemeColors) -> AnyElement {
    let blocks: Vec<AnyElement> = markdown
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| render_block(index, block, colors))
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .text_size(px(13.))
        .line_height(relative(1.55))
        .text_color(colors.fg)
        .children(blocks)
        .into_any_element()
}

fn render_block(index: usize, block: &Block, colors: &ThemeColors) -> AnyElement {
    match block {
        Block::Paragraph(spans) => inline(spans, ("paragraph", index).into(), colors),
        Block::Heading(spans) => div()
            .mt(px(4.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(colors.faint)
            .child(inline(&uppercase(spans), ("heading", index).into(), colors))
            .into_any_element(),
        Block::Item {
            marker,
            depth,
            spans,
        } => {
            let marker = match marker {
                Marker::Bullet => "\u{2022}".to_owned(),
                Marker::Number(number) => format!("{number}."),
            };
            #[expect(clippy::cast_precision_loss, reason = "a handful of nesting levels")]
            let indent = *depth as f32 * 16.;

            div()
                .flex()
                .gap(px(8.))
                .pl(px(indent))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(10.))
                        .text_color(colors.faint)
                        .child(marker),
                )
                .child(div().flex_1().min_w_0().child(inline(
                    spans,
                    ("item", index).into(),
                    colors,
                )))
                .into_any_element()
        }
        Block::Code(code) => div()
            .flex()
            .flex_col()
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
            .into_any_element(),
    }
}

fn uppercase(spans: &[Span]) -> Vec<Span> {
    spans
        .iter()
        .map(|span| Span {
            text: span.text.to_uppercase(),
            ..span.clone()
        })
        .collect()
}

/// One run of text with its inline styles. Inline code gets a box behind it.
fn inline(spans: &[Span], id: ElementId, colors: &ThemeColors) -> AnyElement {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut mono: Vec<(Range<usize>, SharedString)> = Vec::new();
    let mut boxes: Vec<Range<usize>> = Vec::new();
    let mut links: Vec<(Range<usize>, String)> = Vec::new();

    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let range = start..text.len();
        if range.is_empty() {
            continue;
        }

        let mut style = HighlightStyle::default();
        let mut styled = false;

        if span.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
            styled = true;
        }
        if span.italic {
            style.font_style = Some(FontStyle::Italic);
            styled = true;
        }
        if span.code {
            mono.push((range.clone(), SharedString::new_static(MONO_FONT)));
            boxes.push(range.clone());
        }
        if let Some(url) = span.openable_link() {
            style.color = Some(colors.accent);
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(colors.accent),
                wavy: false,
            });
            links.push((range.clone(), url.to_owned()));
            styled = true;
        }
        if styled {
            highlights.push((range, style));
        }
    }

    let styled = StyledText::new(SharedString::from(text))
        .with_highlights(highlights)
        .with_font_family_overrides(mono);
    let layout = styled.layout().clone();

    let body: AnyElement = if links.is_empty() {
        styled.into_any_element()
    } else {
        let (ranges, urls): (Vec<_>, Vec<_>) = links.into_iter().unzip();

        InteractiveText::new(id, styled)
            .on_click(ranges, move |index, _, cx| {
                if let Some(url) = urls.get(index) {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    };

    if boxes.is_empty() {
        return body;
    }

    let (fill, border) = (colors.soft, colors.line);

    div()
        .relative()
        .child(
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    for range in &boxes {
                        paint_code_box(&layout, range, fill, border, window);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .child(body)
        .into_any_element()
}

/// Paints a rounded box behind a code span. A span that wraps gets one box per line end.
fn paint_code_box(
    layout: &gpui::TextLayout,
    range: &Range<usize>,
    fill: Hsla,
    border: Hsla,
    window: &mut Window,
) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    let height = layout.line_height();
    let edges = layout.bounds();

    let lines = if start.y == end.y {
        vec![(start.x, end.x, start.y)]
    } else {
        vec![
            (start.x, edges.right(), start.y),
            (edges.left(), end.x, end.y),
        ]
    };

    for (left, right, top) in lines {
        let bounds = Bounds::from_corners(
            point(left - px(2.), top + px(1.)),
            point(right + px(2.), top + height - px(1.)),
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
}

/// The parsed messages of a thread, shared with the list rows.
pub(crate) type Parsed = Rc<[Rc<Markdown>]>;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_heading_a_list_and_inline_code_become_blocks() {
        let parsed = parse("Wire `pty`.\n\n## Scope\n\n- one\n- two with `code`\n");

        assert_eq!(
            parsed.blocks,
            [
                Block::Paragraph(vec![plain("Wire "), code("pty"), plain(".")]),
                Block::Heading(vec![plain("Scope")]),
                Block::Item {
                    marker: Marker::Bullet,
                    depth: 0,
                    spans: vec![plain("one")],
                },
                Block::Item {
                    marker: Marker::Bullet,
                    depth: 0,
                    spans: vec![plain("two with "), code("code")],
                },
            ]
        );
        assert!(!parsed.tall);
    }

    #[test]
    fn numbered_and_nested_items_keep_their_marker_and_depth() {
        let parsed = parse("3. three\n4. four\n   - inner\n");

        let markers: Vec<(Marker, usize)> = parsed
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
                (Marker::Number(3), 0),
                (Marker::Number(4), 0),
                (Marker::Bullet, 1)
            ]
        );
    }

    #[test]
    fn a_fenced_block_keeps_its_lines() {
        let parsed = parse("```rust\nlet a = 1;\n\nlet b = 2;\n```\n");

        assert_eq!(
            parsed.blocks,
            [Block::Code("let a = 1;\n\nlet b = 2;".to_owned())]
        );
    }

    #[test]
    fn bold_italic_and_links_set_their_span_style() {
        let parsed = parse("**big** and *slanted* in [docs](https://example.com/a).");

        let Some(Block::Paragraph(spans)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };

        assert!(spans[0].bold && !spans[0].italic);
        assert!(spans[2].italic && !spans[2].bold);
        let link = spans.iter().find(|span| span.link.is_some());
        assert_eq!(
            link.and_then(Span::openable_link),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn only_web_links_can_be_opened() {
        let parsed = parse("[a](file:///etc/passwd) [b](javascript:alert(1)) [c](HTTP://x.dev)");

        let Some(Block::Paragraph(spans)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };
        let openable: Vec<Option<&str>> = spans
            .iter()
            .filter(|span| span.link.is_some())
            .map(Span::openable_link)
            .collect();

        assert_eq!(openable, [None, None, Some("HTTP://x.dev")]);
    }

    #[test]
    fn a_message_of_more_than_eight_lines_is_tall() {
        let short = parse("one line");
        let long = parse(&"a line of words\n\n".repeat(9));

        assert!(!short.tall);
        assert!(long.tall);
    }

    #[test]
    fn a_soft_line_break_stays_a_line_break() {
        let parsed = parse("first\nsecond");

        let Some(Block::Paragraph(spans)) = parsed.blocks.first() else {
            panic!("expected a paragraph");
        };

        assert_eq!(joined(spans), "first\nsecond");
    }
}
