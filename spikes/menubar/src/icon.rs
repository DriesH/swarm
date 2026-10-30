//! The Swarm bee as a menu bar template image (black + alpha, macOS tints it).

use anyhow::{Context as _, Result};

const BEE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24"
  fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
  <path d="M12 9c-1.6-3.6-6.4-4.4-7.2-1.8C4.1 9.6 8 10.6 12 9z"/>
  <path d="M12 9c1.6-3.6 6.4-4.4 7.2-1.8.7 2.4-3.2 3.4-7.2 1.8z"/>
  <ellipse cx="12" cy="15" rx="4.5" ry="5.5"/>
  <path d="M7.7 13.5h8.6M7.9 16.8h8.2M12 20.5V22"/>
</svg>"##;

/// Menu bar icons are 18 pt tall; render at 2x for Retina.
pub const POINTS: f64 = 18.;
const PIXELS: u32 = 36;

/// "Needs you" dot, in 24-unit viewBox coordinates.
const DOT: (f32, f32, f32) = (20.5, 3.5, 3.2);
const DOT_GAP: f32 = 1.4;

/// Renders the bee (optionally with the dot) to a PNG.
pub fn bee_png(with_dot: bool) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_str(BEE_SVG, &resvg::usvg::Options::default())
        .context("parse bee svg")?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(PIXELS, PIXELS).context("pixmap")?;
    let scale = PIXELS as f32 / 24.;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let mut rgba = pixmap.take();
    if with_dot {
        paint_dot(&mut rgba, scale);
    }
    // Only black is drawn, so alpha is all that matters; un-premultiplying is a no-op.
    let img = image::RgbaImage::from_raw(PIXELS, PIXELS, rgba).context("rgba buffer")?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// Clears a ring around the dot so it stays readable over the wing, then fills it.
fn paint_dot(rgba: &mut [u8], scale: f32) {
    let (cx, cy, r) = (DOT.0 * scale, DOT.1 * scale, DOT.2 * scale);
    let clear_r = r + DOT_GAP * scale;
    for y in 0..PIXELS {
        for x in 0..PIXELS {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let px = ((y * PIXELS + x) * 4) as usize;
            if d <= r + 0.5 {
                let alpha = ((r + 0.5 - d).clamp(0., 1.) * 255.) as u8;
                rgba[px..px + 4].copy_from_slice(&[0, 0, 0, alpha]);
            } else if d <= clear_r {
                rgba[px..px + 4].fill(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bee_png_should_render_without_dot() {
        assert!(bee_png(false).is_ok_and(|png| png.starts_with(b"\x89PNG")));
    }

    #[test]
    fn bee_png_should_render_with_dot() {
        assert!(bee_png(true).is_ok_and(|png| png.starts_with(b"\x89PNG")));
    }
}
