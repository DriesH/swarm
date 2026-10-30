//! `NSStatusItem` bridge. GPUI has no status item API.

use anyhow::{Context as _, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSBitmapImageFileType, NSCellImagePosition, NSImage, NSScreen, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSDictionary, NSRect, NSSize, NSString};

use crate::icon;

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and this type has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = flume::Sender<()>]
    struct ClickTarget;

    unsafe impl NSObjectProtocol for ClickTarget {}

    impl ClickTarget {
        #[unsafe(method(clicked:))]
        fn clicked(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().send(());
        }
    }
);

impl ClickTarget {
    fn new(mtm: MainThreadMarker, clicks: flume::Sender<()>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(clicks);
        // SAFETY: `NSObject`'s `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

pub struct StatusItem {
    item: Retained<NSStatusItem>,
    plain: Retained<NSImage>,
    with_dot: Retained<NSImage>,
    // The button's target is weak; keep it alive here.
    _target: Retained<ClickTarget>,
    mtm: MainThreadMarker,
}

impl StatusItem {
    pub fn new(mtm: MainThreadMarker, clicks: flume::Sender<()>) -> Result<Self> {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let plain = template_image(&icon::bee_png(false)?)?;
        let with_dot = template_image(&icon::bee_png(true)?)?;
        let target = ClickTarget::new(mtm, clicks);

        let button = item.button(mtm).context("status item has no button")?;
        button.setImage(Some(&plain));
        button.setImagePosition(NSCellImagePosition::ImageLeft);
        // SAFETY: `clicked:` is defined on the target with the action signature.
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(clicked:)));
        }
        button.setAccessibilityLabel(Some(&NSString::from_str("Swarm")));

        Ok(Self {
            item,
            plain,
            with_dot,
            _target: target,
            mtm,
        })
    }

    pub fn set(&self, percent: Option<f32>, needs_you: bool) {
        let Some(button) = self.item.button(self.mtm) else {
            return;
        };
        let title = percent.map_or_else(|| " –".to_string(), |p| format!(" {p:.0}%"));
        button.setTitle(&NSString::from_str(&title));
        button.setImage(Some(if needs_you {
            &self.with_dot
        } else {
            &self.plain
        }));
        let a11y = match (percent, needs_you) {
            (Some(p), true) => format!("Swarm, {p:.0}% used, needs you"),
            (Some(p), false) => format!("Swarm, {p:.0}% used"),
            (None, true) => "Swarm, needs you".into(),
            (None, false) => "Swarm".into(),
        };
        button.setAccessibilityLabel(Some(&NSString::from_str(&a11y)));
    }

    /// Clicks the button through AppKit, exercising the real target/action path.
    pub fn perform_click(&self) {
        if let Some(button) = self.item.button(self.mtm) {
            // SAFETY: `nil` sender is allowed.
            unsafe { button.performClick(None) };
        }
    }

    /// The button's frame in Cocoa screen coordinates (origin bottom-left).
    /// Also returns the screen the menu bar is on.
    pub fn anchor(&self) -> Option<(NSRect, Retained<NSScreen>)> {
        let window = self.item.button(self.mtm)?.window()?;
        let screen = window.screen().or_else(|| NSScreen::mainScreen(self.mtm))?;
        Some((window.frame(), screen))
    }

    /// PNG of what the button draws, via AppKit's own view cache (no screen recording).
    pub fn snapshot_png(&self) -> Result<Vec<u8>> {
        let button = self.item.button(self.mtm).context("no button")?;
        let bounds = button.bounds();
        let rep = button
            .bitmapImageRepForCachingDisplayInRect(bounds)
            .context("no bitmap rep")?;
        button.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        // SAFETY: an empty properties dictionary is valid for PNG.
        let data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }
        .context("png encode")?;
        Ok(data.to_vec())
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

fn template_image(png: &[u8]) -> Result<Retained<NSImage>> {
    let data = NSData::with_bytes(png);
    let image = NSImage::initWithData(NSImage::alloc(), &data).context("NSImage from png")?;
    image.setSize(NSSize::new(icon::POINTS, icon::POINTS));
    image.setTemplate(true);
    Ok(image)
}
