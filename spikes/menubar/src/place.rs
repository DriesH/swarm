//! Turns the status item's Cocoa frame into GPUI window bounds.
//!
//! GPUI places a window relative to a display's top-left corner. Moving the
//! NSWindow afterwards calls back into GPUI while the app is borrowed
//! ("RefCell already borrowed"), so compute the bounds up front.

use gpui::{Bounds, DisplayId, Pixels, point, px, size};
use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSRect, NSString};

const GAP: f64 = 4.;
const EDGE: f64 = 8.;

pub fn below(
    anchor: NSRect,
    screen: &NSScreen,
    width: f32,
    height: f32,
) -> Option<(DisplayId, Bounds<Pixels>)> {
    let display = display_id(screen)?;
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let w = f64::from(width);
    let x = (anchor.origin.x + anchor.size.width / 2. - w / 2.)
        .min(visible.origin.x + visible.size.width - w - EDGE)
        .max(visible.origin.x + EDGE);
    let top = anchor.origin.y - GAP;
    let origin = point(
        px((x - frame.origin.x) as f32),
        px((frame.origin.y + frame.size.height - top) as f32),
    );
    eprintln!(
        "[place] screen {:?} visible {:?} -> display-relative {origin:?}",
        frame, visible
    );
    Some((display, Bounds::new(origin, size(px(width), px(height)))))
}

fn display_id(screen: &NSScreen) -> Option<DisplayId> {
    let description = screen.deviceDescription();
    let number = description.objectForKey(&NSString::from_str("NSScreenNumber"))?;
    let number: &AnyObject = &number;
    // SAFETY: AppKit documents `NSScreenNumber` as an NSNumber holding the CGDirectDisplayID.
    let id: u32 = unsafe { msg_send![number, unsignedIntValue] };
    Some(DisplayId::new(u64::from(id)))
}
