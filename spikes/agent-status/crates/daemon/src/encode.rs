//! Turns libghostty render rows into plain wire rows (`proto::Row`).

use libghostty_vt::{
    render::{CellIterator, RowIteration},
    screen::{CellContentTag, CellWide},
    style::{RgbColor, Underline},
};
use proto::{BgSpan, Rgb, Row, TextSpan};

pub fn rgb(c: RgbColor) -> Rgb {
    Rgb(c.r, c.g, c.b)
}

/// Encodes one row. `text` is a scratch buffer reused between calls.
pub fn row(
    row: &RowIteration<'static, '_>,
    cells: &mut CellIterator<'static>,
    default_fg: RgbColor,
    default_bg: RgbColor,
    text: &mut String,
) -> libghostty_vt::error::Result<Row> {
    let mut out = Row::default();
    let mut cells = cells.update(row)?;
    let mut x: u16 = 0;
    let mut last_wide = false;
    while let Some(cell) = cells.next() {
        let col = x;
        x += 1;
        let raw = cell.raw_cell()?;
        let wide = raw.wide()?;
        if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
            continue;
        }
        let width = if wide == CellWide::Wide { 2 } else { 1 };
        let tag = raw.content_tag()?;
        // Each query is an FFI call, so skip style and color lookups for plain cells.
        let style = if raw.has_styling()? {
            Some(cell.style()?)
        } else {
            None
        };
        let mut fg = default_fg;
        let mut bg = None;
        if style.is_some() {
            fg = cell.fg_color()?.unwrap_or(default_fg);
            bg = cell.bg_color()?;
        } else if matches!(
            tag,
            CellContentTag::BgColorPalette | CellContentTag::BgColorRgb
        ) {
            bg = cell.bg_color()?;
        }
        if style.is_some_and(|s| s.inverse) {
            let old_fg = fg;
            fg = bg.unwrap_or(default_bg);
            bg = Some(old_fg);
        }
        if let Some(bg) = bg.filter(|bg| *bg != default_bg) {
            push_bg(&mut out.bgs, col, width, rgb(bg));
        }

        text.clear();
        match tag {
            CellContentTag::Codepoint => {
                if let Some(c) = char::from_u32(raw.codepoint()?).filter(|c| *c != '\0') {
                    text.push(c);
                }
            }
            CellContentTag::CodepointGrapheme => cell.graphemes_utf8(text)?,
            _ => {}
        }
        if text.is_empty() || text == " " || style.is_some_and(|s| s.invisible) {
            continue;
        }
        let flags = style.map_or(0, |s| {
            let mut f = 0;
            if s.bold {
                f |= TextSpan::BOLD;
            }
            if s.italic {
                f |= TextSpan::ITALIC;
            }
            if s.faint {
                f |= TextSpan::FAINT;
            }
            if s.underline != Underline::None {
                f |= TextSpan::UNDERLINE;
            }
            if s.underline == Underline::Curly {
                f |= TextSpan::CURLY;
            }
            if s.strikethrough {
                f |= TextSpan::STRIKE;
            }
            f
        });
        // Wide glyphs get their own span: the renderer spaces glyphs one cell apart.
        let merge = width == 1 && !last_wide;
        last_wide = width == 2;
        push_text(&mut out.texts, col, width, text, rgb(fg), flags, merge);
    }
    Ok(out)
}

fn push_bg(bgs: &mut Vec<BgSpan>, col: u16, width: u16, color: Rgb) {
    if let Some(last) = bgs.last_mut()
        && last.color == color
        && last.col + last.len == col
    {
        last.len += width;
    } else {
        bgs.push(BgSpan {
            col,
            len: width,
            color,
        });
    }
}

fn push_text(
    spans: &mut Vec<TextSpan>,
    col: u16,
    width: u16,
    text: &str,
    fg: Rgb,
    flags: u8,
    merge: bool,
) {
    if merge
        && let Some(last) = spans.last_mut()
        && last.col + last.cells == col
        && last.fg == fg
        && last.flags == flags
    {
        last.text.push_str(text);
        last.cells += 1;
        return;
    }
    spans.push(TextSpan {
        col,
        cells: width,
        text: text.to_owned(),
        fg,
        flags,
    });
}
