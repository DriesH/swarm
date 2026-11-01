//! The Swarm bee as a menu bar template image: black on clear, which macOS tints.

use anyhow::{Context as _, Result};

use super::summary::BeeFrame;

const BEE_OPEN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24"
  fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
  <path d="M12 9c-1.6-3.6-6.4-4.4-7.2-1.8C4.1 9.6 8 10.6 12 9z"/>
  <path d="M12 9c1.6-3.6 6.4-4.4 7.2-1.8.7 2.4-3.2 3.4-7.2 1.8z"/>
  <ellipse cx="12" cy="15" rx="4.5" ry="5.5"/>
  <path d="M7.7 13.5h8.6M7.9 16.8h8.2M12 20.5V22"/>
</svg>"##;

/// The same bee with its wings squashed to 35% of their height around the shoulder, so the
/// strokes stay as thick as in the open frame.
const BEE_FLAT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24"
  fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
  <path d="M12 9C10.4 7.74 5.6 7.46 4.8 8.37C4.1 9.21 8 9.56 12 9z"/>
  <path d="M12 9C13.6 7.74 18.4 7.46 19.2 8.37C19.9 9.21 16 9.56 12 9z"/>
  <ellipse cx="12" cy="15" rx="4.5" ry="5.5"/>
  <path d="M7.7 13.5h8.6M7.9 16.8h8.2M12 20.5V22"/>
</svg>"##;

/// Menu bar icons are 18 points tall; the image is drawn at 2x for Retina.
pub(crate) const POINTS: f64 = 18.;
const PIXELS: u32 = 36;

/// The "needs you" dot, in the SVG's 24 unit coordinates.
const DOT: (f32, f32, f32) = (20.5, 3.5, 3.2);
/// Clear space around the dot, so it reads over the wing.
const DOT_GAP: f32 = 1.4;

/// Draws the bee in one of its two wing frames, with the dot when `with_dot` is set, and
/// encodes it as a PNG.
pub(crate) fn bee_png(with_dot: bool, frame: BeeFrame) -> Result<Vec<u8>> {
    let svg = match frame {
        BeeFrame::Open => BEE_OPEN,
        BeeFrame::Flat => BEE_FLAT,
    };
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .context("parse the bee svg")?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(PIXELS, PIXELS).context("make a pixmap")?;

    #[expect(
        clippy::cast_precision_loss,
        reason = "a 36 pixel image is far inside f32 range"
    )]
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

    // Only black is drawn, so alpha is all that matters and premultiplying changes nothing.
    let image = image::RgbaImage::from_raw(PIXELS, PIXELS, rgba).context("wrap the pixels")?;
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;

    Ok(png)
}

/// Clears a ring around the dot, then fills the dot.
fn paint_dot(rgba: &mut [u8], scale: f32) {
    let (center_x, center_y, radius) = (DOT.0 * scale, DOT.1 * scale, DOT.2 * scale);
    let clear_radius = radius + DOT_GAP * scale;

    for row in 0..PIXELS {
        for column in 0..PIXELS {
            #[expect(
                clippy::cast_precision_loss,
                reason = "pixel positions are far inside f32 range"
            )]
            let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
            let distance = (x - center_x).hypot(y - center_y);
            let start = (row * PIXELS + column) as usize * 4;

            if distance <= radius + 0.5 {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the value is clamped to 0..=255"
                )]
                let alpha = ((radius + 0.5 - distance).clamp(0., 1.) * 255.) as u8;
                rgba[start..start + 4].copy_from_slice(&[0, 0, 0, alpha]);
            } else if distance <= clear_radius {
                rgba[start..start + 4].fill(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(png: &[u8], x: u32, y: u32) -> u8 {
        let image = image::load_from_memory(png).map(|image| image.to_rgba8());

        image.map_or(0, |image| image.get_pixel(x, y).0[3])
    }

    #[test]
    fn the_bee_draws_something_without_the_dot() {
        let png = bee_png(false, BeeFrame::Open).unwrap_or_default();

        assert!(png.starts_with(b"\x89PNG"));
        // The body is the middle of the image; the dot's corner stays clear.
        assert!(alpha_at(&png, 18, 24) > 0 || alpha_at(&png, 18, 22) > 0);
        assert_eq!(alpha_at(&png, 30, 5), 0);
    }

    #[test]
    fn the_flat_frame_differs_from_the_open_one_only_in_the_wings() {
        let open = bee_png(false, BeeFrame::Open).unwrap_or_default();
        let flat = bee_png(false, BeeFrame::Flat).unwrap_or_default();

        assert!(flat.starts_with(b"\x89PNG"));
        assert_ne!(open, flat);
        // The open wing arches over this pixel; the flat one stays below it.
        assert!(alpha_at(&open, 12, 8) > 0);
        assert_eq!(alpha_at(&flat, 12, 8), 0);
        // The body is the same in both.
        assert_eq!(alpha_at(&open, 18, 30), alpha_at(&flat, 18, 30));
    }

    #[test]
    fn the_dot_fills_the_top_right_corner() {
        let png = bee_png(true, BeeFrame::Open).unwrap_or_default();

        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(alpha_at(&png, 30, 5), 255);
    }
}
