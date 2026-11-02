//! Colors for Markdown text in a plain text field.

use std::collections::BTreeMap;
use std::ops::Range;

use gpui::{FontStyle, FontWeight, HighlightStyle};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::theme::ThemeColors;

type Highlights = Vec<(Range<usize>, HighlightStyle)>;

/// Styles for the Markdown in `text`, for `TextArea::set_highlighter`.
/// The ranges are sorted, apart and on char boundaries. Half-written Markdown is fine:
/// what does not parse stays plain.
pub fn markdown_highlights(
    text: &str,
    colors: &ThemeColors,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut marker = Marker::new(text, colors);
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;

    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        marker.visit(&event, range);
    }

    marker.finish()
}

struct Styles {
    muted: HighlightStyle,
    heading: HighlightStyle,
    bold: HighlightStyle,
    italic: HighlightStyle,
    code: HighlightStyle,
    link: HighlightStyle,
}

impl Styles {
    fn new(colors: &ThemeColors) -> Self {
        Self {
            muted: HighlightStyle::color(colors.muted),
            heading: HighlightStyle {
                color: Some(colors.fg),
                font_weight: Some(FontWeight::BOLD),
                ..HighlightStyle::default()
            },
            bold: FontWeight::BOLD.into(),
            italic: FontStyle::Italic.into(),
            code: HighlightStyle {
                background_color: Some(colors.soft),
                ..HighlightStyle::default()
            },
            link: HighlightStyle::color(colors.accent),
        }
    }
}

/// A link that has started and not ended yet.
struct OpenLink {
    start: usize,
    end: usize,
    /// Where the link's styles go in `Marker::layers`, so what is inside the link wins.
    layer: usize,
    /// End of everything seen inside the link text. The `]` sits here.
    content_end: usize,
    inline: bool,
}

/// Collects styled ranges while the parser walks the text. Ranges may overlap and later
/// ones win; `finish` flattens them.
struct Marker<'a> {
    text: &'a str,
    styles: Styles,
    layers: Highlights,
    links: Vec<OpenLink>,
}

impl<'a> Marker<'a> {
    fn new(text: &'a str, colors: &ThemeColors) -> Self {
        Self {
            text,
            styles: Styles::new(colors),
            layers: Vec::new(),
            links: Vec::new(),
        }
    }

    fn visit(&mut self, event: &Event<'_>, range: Range<usize>) {
        if !matches!(
            event,
            Event::Start(Tag::Link { .. }) | Event::End(TagEnd::Link)
        ) && let Some(link) = self.links.last_mut()
        {
            link.content_end = link.content_end.max(range.end);
        }

        match event {
            Event::Start(Tag::Heading { .. }) => self.heading(range.start),
            Event::Start(Tag::Item) => self.list_marker(range.start),
            Event::TaskListMarker(_) => self.add(range, self.styles.muted),
            Event::Start(Tag::BlockQuote(_)) => self.quote_markers(range),
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => self.fenced_code(range),
            Event::Start(Tag::Strong) => self.delimited(range, 2, self.styles.bold),
            Event::Start(Tag::Emphasis) => self.delimited(range, 1, self.styles.italic),
            Event::Code(_) => self.inline_code(range),
            Event::Start(Tag::Link { link_type, .. }) => {
                self.open_link(range, *link_type == LinkType::Inline);
            }
            Event::End(TagEnd::Link) => self.close_link(),
            _ => {}
        }
    }

    fn finish(self) -> Highlights {
        flatten(self.text, &self.layers)
    }

    fn add(&mut self, range: Range<usize>, style: HighlightStyle) {
        if range.start < range.end {
            self.layers.push((range, style));
        }
    }

    fn slice(&self, range: Range<usize>) -> &'a str {
        self.text.get(range).unwrap_or_default()
    }

    fn line_end(&self, start: usize) -> usize {
        self.text
            .get(start..)
            .and_then(|rest| rest.find('\n'))
            .map_or(self.text.len(), |length| start + length)
    }

    /// An ATX heading: `#` marks are muted, the text is bold. Setext headings stay plain.
    fn heading(&mut self, start: usize) {
        let line = self.slice(start..self.line_end(start));
        let line_start = line.trim_start_matches([' ', '\t']);
        let marks_start = start + line.len() - line_start.len();
        let marks = line_start.bytes().take_while(|byte| *byte == b'#').count();

        if marks == 0 {
            return;
        }

        let rest = line_start.get(marks..).unwrap_or_default();
        let content = rest.trim_start();
        let content_start = marks_start + marks + rest.len() - content.len();
        let content_end = content_start + content.trim_end().len();

        self.add(marks_start..marks_start + marks, self.styles.muted);
        self.add(content_start..content_end, self.styles.heading);
    }

    /// The `-`, `*`, `+`, `1.` or `1)` that starts a list item.
    fn list_marker(&mut self, start: usize) {
        let line = self.slice(start..self.line_end(start));
        let line_start = line.trim_start_matches([' ', '\t']);
        let marker_start = start + line.len() - line_start.len();
        let bytes = line_start.as_bytes();

        let length = match bytes.first() {
            Some(b'-' | b'*' | b'+') => 1,
            Some(byte) if byte.is_ascii_digit() => {
                let digits = bytes
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count();

                match bytes.get(digits) {
                    Some(b'.' | b')') => digits + 1,
                    _ => 0,
                }
            }
            _ => 0,
        };

        self.add(marker_start..marker_start + length, self.styles.muted);
    }

    /// The parser gives one range for the whole quote, so scan each line for its `>` marks.
    fn quote_markers(&mut self, range: Range<usize>) {
        let mut offset = range.start;

        for line in self.slice(range).split('\n') {
            let mut from = 0;

            while let Some(mark) = quote_mark_at(line.as_bytes(), from) {
                self.add(offset + mark..offset + mark + 1, self.styles.muted);
                from = mark + 1;
            }

            offset += line.len() + 1;
        }
    }

    /// Fence lines are muted, the lines between them get the code background.
    fn fenced_code(&mut self, range: Range<usize>) {
        let mut lines = Vec::new();
        let mut offset = range.start;

        for raw in self.slice(range).split_inclusive('\n') {
            lines.push((offset, raw.strip_suffix('\n').unwrap_or(raw)));
            offset += raw.len();
        }

        let Some(&(open_offset, open_line)) = lines.first() else {
            return;
        };
        let Some(open) = fence(open_line) else {
            return;
        };

        let closing = lines.len() > 1
            && lines
                .last()
                .and_then(|(_, line)| fence(line))
                .is_some_and(|close| {
                    close.marker == open.marker
                        && close.length >= open.length
                        && close.rest.trim().is_empty()
                });
        let body_end = lines.len() - usize::from(closing);

        self.add(
            open_offset..open_offset + open_line.len(),
            self.styles.muted,
        );

        for &(offset, line) in lines.get(1..body_end).unwrap_or_default() {
            self.add(offset..offset + line.len(), self.styles.code);
        }

        if let (true, Some(&(offset, line))) = (closing, lines.last()) {
            self.add(offset..offset + line.len(), self.styles.muted);
        }
    }

    /// Bold and italic: the content gets `style`, the `width` delimiters on each side are muted.
    fn delimited(&mut self, range: Range<usize>, width: usize, style: HighlightStyle) {
        if range.len() <= width * 2 {
            return;
        }

        let opening = range.start..range.start + width;
        let closing = range.end - width..range.end;
        let is_delimiter = |side: &Range<usize>| {
            self.slice(side.clone())
                .bytes()
                .all(|byte| byte == b'*' || byte == b'_')
        };

        if !is_delimiter(&opening) || !is_delimiter(&closing) {
            return;
        }

        self.add(range.start + width..range.end - width, style);
        self.add(opening, self.styles.muted);
        self.add(closing, self.styles.muted);
    }

    fn inline_code(&mut self, range: Range<usize>) {
        let ticks = self
            .slice(range.clone())
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();

        if ticks == 0 || range.len() < ticks * 2 {
            return;
        }

        self.add(range.start + ticks..range.end - ticks, self.styles.code);
        self.add(range.start..range.start + ticks, self.styles.muted);
        self.add(range.end - ticks..range.end, self.styles.muted);
    }

    fn open_link(&mut self, range: Range<usize>, inline: bool) {
        self.links.push(OpenLink {
            start: range.start,
            end: range.end,
            layer: self.layers.len(),
            content_end: range.start + 1,
            inline,
        });
    }

    /// `[text]` is accent, `(url)` is muted. Other kinds of link stay plain.
    fn close_link(&mut self) {
        let Some(link) = self.links.pop() else {
            return;
        };

        let bytes = self.text.as_bytes();
        let close = link.content_end;
        let well_formed = link.inline
            && bytes.get(link.start) == Some(&b'[')
            && bytes.get(close) == Some(&b']')
            && bytes.get(close + 1) == Some(&b'(')
            && close + 1 < link.end;

        if well_formed {
            let styles = [
                (link.start..close + 1, self.styles.link),
                (close + 1..link.end, self.styles.muted),
            ];

            self.layers.splice(link.layer..link.layer, styles);
        }
    }
}

/// The `>` after only spaces, looking from `from`.
fn quote_mark_at(line: &[u8], from: usize) -> Option<usize> {
    let rest = line.get(from..)?;
    let spaces = rest
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();

    (rest.get(spaces) == Some(&b'>')).then_some(from + spaces)
}

struct Fence<'a> {
    marker: u8,
    length: usize,
    /// What follows the run of fence characters: the info string of an opening fence.
    rest: &'a str,
}

fn fence(line: &str) -> Option<Fence<'_>> {
    let line = line.trim_start_matches([' ', '\t', '>']);
    let marker = *line
        .as_bytes()
        .first()
        .filter(|byte| matches!(byte, b'`' | b'~'))?;
    let length = line.bytes().take_while(|byte| *byte == marker).count();

    (length >= 3).then(|| Fence {
        marker,
        length,
        rest: line.get(length..).unwrap_or_default(),
    })
}

/// `top` wins wherever it sets a field.
fn overlay(base: HighlightStyle, top: HighlightStyle) -> HighlightStyle {
    HighlightStyle {
        color: top.color.or(base.color),
        font_weight: top.font_weight.or(base.font_weight),
        font_style: top.font_style.or(base.font_style),
        background_color: top.background_color.or(base.background_color),
        underline: top.underline.or(base.underline),
        strikethrough: top.strikethrough.or(base.strikethrough),
        fade_out: top.fade_out.or(base.fade_out),
    }
}

/// Turns overlapping layers (later wins) into sorted ranges that do not overlap.
/// Drops empty ranges, ranges past the end and ranges inside a char.
fn flatten(text: &str, layers: &[(Range<usize>, HighlightStyle)]) -> Highlights {
    let mut edges = Vec::new();

    for (index, (range, _)) in layers.iter().enumerate() {
        let valid = range.start < range.end
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);

        if valid {
            edges.push((range.start, true, index));
            edges.push((range.end, false, index));
        }
    }

    edges.sort_unstable_by_key(|(position, _, _)| *position);

    let mut active: BTreeMap<usize, HighlightStyle> = BTreeMap::new();
    let mut flat: Highlights = Vec::new();
    let mut cursor = 0;

    for (position, starts, index) in edges {
        if position > cursor {
            if !active.is_empty() {
                let style = active
                    .values()
                    .fold(HighlightStyle::default(), |merged, top| {
                        overlay(merged, *top)
                    });

                match flat.last_mut() {
                    Some((last, last_style)) if last.end == cursor && *last_style == style => {
                        last.end = position;
                    }
                    _ => flat.push((cursor..position, style)),
                }
            }

            cursor = position;
        }

        match layers.get(index) {
            Some((_, style)) if starts => {
                active.insert(index, *style);
            }
            _ => {
                active.remove(&index);
            }
        }
    }

    flat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn colors() -> ThemeColors {
        Theme::dark().colors().clone()
    }

    fn highlights(text: &str) -> Highlights {
        markdown_highlights(text, &colors())
    }

    fn muted() -> HighlightStyle {
        HighlightStyle::color(colors().muted)
    }

    fn code() -> HighlightStyle {
        HighlightStyle {
            background_color: Some(colors().soft),
            ..HighlightStyle::default()
        }
    }

    fn bold() -> HighlightStyle {
        FontWeight::BOLD.into()
    }

    fn italic() -> HighlightStyle {
        FontStyle::Italic.into()
    }

    /// The style of the piece of highlighted text that covers all of `needle`.
    fn style_of(text: &str, needle: &str) -> Option<HighlightStyle> {
        let start = text.find(needle)?;
        let end = start + needle.len();

        highlights(text)
            .into_iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
            .map(|(_, style)| style)
    }

    /// No highlight touches `needle`.
    fn is_plain(text: &str, needle: &str) -> bool {
        let Some(start) = text.find(needle) else {
            return false;
        };
        let end = start + needle.len();

        highlights(text)
            .iter()
            .all(|(range, _)| range.end <= start || end <= range.start)
    }

    fn assert_well_formed(text: &str) {
        let mut previous_end = 0;

        for (range, _) in highlights(text) {
            assert!(range.start < range.end, "empty range in {text:?}");
            assert!(range.start >= previous_end, "overlap in {text:?}");
            assert!(range.end <= text.len(), "range past the end of {text:?}");
            assert!(text.is_char_boundary(range.start), "{range:?} in {text:?}");
            assert!(text.is_char_boundary(range.end), "{range:?} in {text:?}");

            previous_end = range.end;
        }
    }

    #[test]
    fn a_heading_is_bold_in_the_text_color_and_its_marks_are_muted() {
        let text = "## Plan for docs";
        let heading = HighlightStyle {
            color: Some(colors().fg),
            font_weight: Some(FontWeight::BOLD),
            ..HighlightStyle::default()
        };

        assert_eq!(style_of(text, "Plan for docs"), Some(heading));
        assert_eq!(style_of(text, "##"), Some(muted()));
    }

    #[test]
    fn a_heading_in_a_list_or_a_quote_is_found() {
        assert_eq!(style_of("> # Quoted", "#"), Some(muted()));
        assert_eq!(style_of("- # Listed", "#"), Some(muted()));
    }

    #[test]
    fn seven_hashes_or_no_space_is_not_a_heading() {
        assert!(is_plain("####### Too deep", "Too deep"));
        assert!(is_plain("#tag", "tag"));
    }

    #[test]
    fn inline_styles_inside_a_heading_keep_the_heading_bold() {
        let text = "# Add `docs` now";
        let style = style_of(text, "docs");

        assert_eq!(
            style.map(|style| (style.font_weight, style.background_color)),
            Some((Some(FontWeight::BOLD), Some(colors().soft)))
        );
    }

    #[test]
    fn list_markers_are_muted() {
        for (text, marker) in [
            ("- one", "-"),
            ("* one", "*"),
            ("+ one", "+"),
            ("1. one", "1."),
            ("12) one", "12)"),
            ("  - nested", "-"),
        ] {
            assert_eq!(style_of(text, marker), Some(muted()), "{text}");
        }
    }

    #[test]
    fn list_text_is_plain() {
        assert!(is_plain("- one", "one"));
    }

    #[test]
    fn a_dash_without_a_space_is_not_a_list() {
        assert!(is_plain("-one", "-one"));
    }

    #[test]
    fn the_task_box_is_muted() {
        let text = "- [ ] open\n- [x] done";

        assert_eq!(style_of(text, "[ ]"), Some(muted()));
        assert_eq!(style_of(text, "[x]"), Some(muted()));
    }

    #[test]
    fn quote_marks_are_muted_on_every_line() {
        let text = "> first\n> second\n> > deeper";
        let marks: Vec<_> = highlights(text)
            .into_iter()
            .map(|(range, _)| &text[range])
            .collect();

        assert_eq!(marks, [">", ">", ">", ">"]);
    }

    #[test]
    fn quote_text_is_plain() {
        assert!(is_plain("> quoted", "quoted"));
    }

    #[test]
    fn bold_text_is_bold_and_its_delimiters_are_muted() {
        for text in ["a **bold** b", "a __bold__ b"] {
            assert_eq!(style_of(text, "bold"), Some(bold()), "{text}");
            assert_eq!(style_of(text, &text[2..4]), Some(muted()), "{text}");
        }
    }

    #[test]
    fn italic_text_is_italic_and_its_delimiters_are_muted() {
        for (text, delimiter) in [("a *soft* b", "*"), ("a _soft_ b", "_")] {
            assert_eq!(style_of(text, "soft"), Some(italic()), "{text}");
            assert_eq!(style_of(text, delimiter), Some(muted()), "{text}");
        }
    }

    #[test]
    fn bold_italic_text_has_both() {
        let style = style_of("***both***", "both");

        assert_eq!(
            style.map(|style| (style.font_weight, style.font_style)),
            Some((Some(FontWeight::BOLD), Some(FontStyle::Italic)))
        );
    }

    #[test]
    fn inline_code_has_a_soft_background() {
        let text = "run `cargo test` now";

        assert_eq!(style_of(text, "cargo test"), Some(code()));
        assert!(is_plain(text, "run"));
        assert!(is_plain(text, "now"));
    }

    #[test]
    fn the_backticks_of_inline_code_are_muted() {
        let text = "a ``x`` b";
        let marks: Vec<_> = highlights(text)
            .into_iter()
            .filter(|(_, style)| *style == muted())
            .map(|(range, _)| &text[range])
            .collect();

        assert_eq!(marks, ["``", "``"]);
    }

    #[test]
    fn markdown_inside_inline_code_is_plain_code() {
        assert_eq!(style_of("`**x**`", "**x**"), Some(code()));
    }

    #[test]
    fn a_fenced_block_mutes_its_fences_and_shades_its_code() {
        let text = "before\n```rust\nlet x = 1;\n\nlet y = 2;\n```\nafter";

        assert_eq!(style_of(text, "```rust"), Some(muted()));
        assert_eq!(style_of(text, "let x = 1;"), Some(code()));
        assert_eq!(style_of(text, "let y = 2;"), Some(code()));
        assert!(is_plain(text, "before"));
        assert!(is_plain(text, "after"));
    }

    #[test]
    fn the_closing_fence_is_muted() {
        let text = "```\ncode\n```";
        let fences: Vec<_> = highlights(text)
            .into_iter()
            .filter(|(_, style)| *style == muted())
            .map(|(range, _)| range)
            .collect();

        assert_eq!(fences, [0..3, 9..12]);
    }

    #[test]
    fn tilde_fences_work_too() {
        let text = "~~~\ncode\n~~~";

        assert_eq!(style_of(text, "code"), Some(code()));
        assert_eq!(style_of(text, "~~~"), Some(muted()));
    }

    #[test]
    fn markdown_inside_a_fenced_block_is_not_styled() {
        let text = "```\n# not a heading\n- not a list\n**not bold**\n```";

        assert_eq!(style_of(text, "# not a heading"), Some(code()));
        assert_eq!(style_of(text, "- not a list"), Some(code()));
        assert_eq!(style_of(text, "**not bold**"), Some(code()));
    }

    #[test]
    fn a_fence_that_never_closes_shades_the_rest() {
        let text = "```rust\nlet x = 1;\nlet y = 2;";

        assert_eq!(style_of(text, "```rust"), Some(muted()));
        assert_eq!(style_of(text, "let x = 1;"), Some(code()));
        assert_eq!(style_of(text, "let y = 2;"), Some(code()));
    }

    #[test]
    fn a_link_has_accent_text_and_a_muted_url() {
        let text = "see [the roadmap](docs/ROADMAP.md) now";
        let accent = HighlightStyle::color(colors().accent);

        assert_eq!(style_of(text, "[the roadmap]"), Some(accent));
        assert_eq!(style_of(text, "(docs/ROADMAP.md)"), Some(muted()));
        assert!(is_plain(text, "see"));
        assert!(is_plain(text, "now"));
    }

    #[test]
    fn code_inside_link_text_keeps_its_own_styles() {
        let text = "[`a`](u)";

        assert_eq!(
            style_of(text, "a").map(|style| style.background_color),
            Some(Some(colors().soft))
        );
        assert_eq!(style_of(text, "`"), Some(muted()));
    }

    #[test]
    fn delimiters_inside_a_heading_stay_muted() {
        let style = style_of("# a **b** c", "**");

        assert_eq!(style.map(|style| style.color), Some(Some(colors().muted)));
    }

    #[test]
    fn plain_text_has_no_styles() {
        assert!(highlights("just words, 1 + 2 = 3.\nand more words").is_empty());
    }

    #[test]
    fn half_typed_markdown_stays_plain() {
        for text in [
            "**open",
            "*open",
            "`open",
            "[text](",
            "[text](url",
            "[text",
            "![alt](",
        ] {
            assert!(highlights(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn half_typed_markdown_never_overlaps() {
        for text in [
            "",
            "#",
            "# ",
            "-",
            "- [",
            "- [ ",
            "- [ ]",
            ">",
            "> > ",
            "```",
            "``",
            "~~~~",
            "```rust\n",
            "```rust\nfn main() {",
            "**",
            "****",
            "***a**",
            "**a *b**",
            "_a **b_ c**",
            "[a](b)(c)",
            "[a][b]",
            "[**a**](b)",
            "[[a]](b)",
            "1.",
            "99999999999999999999. x",
            "> ```\n> code",
            "- ```\n  code\n  ```",
            "\n\n\n",
            "\t- tab",
            "a\r\nb\r\n",
        ] {
            assert_well_formed(text);
        }
    }

    #[test]
    fn every_prefix_of_a_document_is_well_formed() {
        let document = "# Café ☕\n\n> ça **va** 👍🏽?\n\n- [x] déjà *vu* `ü`\n1. [lien](https://exemple.fr/é)\n\n```sh\necho 日本\n```\n";

        for end in 0..=document.len() {
            if document.is_char_boundary(end) {
                assert_well_formed(&document[..end]);
            }
        }
    }

    #[test]
    fn emoji_and_accents_get_whole_char_ranges() {
        let text = "**é👍🏽** `ü` [日本](x)";

        assert_well_formed(text);
        assert_eq!(style_of(text, "é👍🏽"), Some(bold()));
        assert_eq!(style_of(text, "ü"), Some(code()));
    }

    #[test]
    fn a_long_text_stays_well_formed() {
        let text = "- [ ] item with **bold** and `code` and [link](url)\n".repeat(2000);

        assert_well_formed(&text);
    }
}
