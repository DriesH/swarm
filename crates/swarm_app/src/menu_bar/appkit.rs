//! The `NSStatusItem`. GPUI has no status item API, so a small `objc2` bridge owns it.
//!
//! Nothing here touches GPUI state. A click sends into a channel that a GPUI task reads.

use std::rc::Rc;

use anyhow::{Context as _, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSCellImagePosition, NSImage, NSScreen, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSRect, NSSize, NSString};

use super::place::{Anchor, Frame, Screen};
use super::{Look, StatusItem, icon};

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
            if let Err(error) = self.ivars().send(()) {
                log::debug!("the menu bar task is gone: {error}");
            }
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

struct AppKitStatusItem {
    item: Retained<NSStatusItem>,
    plain: Retained<NSImage>,
    with_dot: Retained<NSImage>,
    /// The button's target is weak, so it is kept alive here.
    _target: Retained<ClickTarget>,
    mtm: MainThreadMarker,
}

/// Puts the bee in the menu bar. `None` when `AppKit` refuses; the app then runs without it.
pub(super) fn create(clicks: flume::Sender<()>) -> Option<Rc<dyn StatusItem>> {
    let mtm = MainThreadMarker::new()?;

    match AppKitStatusItem::new(mtm, clicks) {
        Ok(item) => Some(Rc::new(item)),
        Err(error) => {
            log::error!("cannot make the menu bar item: {error:#}");
            None
        }
    }
}

impl AppKitStatusItem {
    fn new(mtm: MainThreadMarker, clicks: flume::Sender<()>) -> Result<Self> {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let plain = template_image(&icon::bee_png(false)?)?;
        let with_dot = template_image(&icon::bee_png(true)?)?;
        let target = ClickTarget::new(mtm, clicks);

        let button = item.button(mtm).context("the status item has no button")?;
        button.setImage(Some(&plain));
        button.setImagePosition(NSCellImagePosition::ImageLeft);
        button.setAccessibilityLabel(Some(&NSString::from_str("Swarm")));

        // SAFETY: `clicked:` is defined on the target with the action signature.
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(clicked:)));
        }

        Ok(Self {
            item,
            plain,
            with_dot,
            _target: target,
            mtm,
        })
    }
}

impl StatusItem for AppKitStatusItem {
    fn show(&self, look: &Look) {
        let Some(button) = self.item.button(self.mtm) else {
            return;
        };

        button.setTitle(&NSString::from_str(
            look.title.as_deref().unwrap_or_default(),
        ));
        button.setImage(Some(if look.dot {
            &self.with_dot
        } else {
            &self.plain
        }));
        button.setAccessibilityLabel(Some(&NSString::from_str(&look.label)));
    }

    fn anchor(&self) -> Option<Anchor> {
        let window = self.item.button(self.mtm)?.window()?;
        let screen = window.screen().or_else(|| NSScreen::mainScreen(self.mtm))?;

        Some(Anchor {
            button: frame_of(window.frame()),
            screen: screen_of(&screen)?,
        })
    }
}

impl Drop for AppKitStatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

fn template_image(png: &[u8]) -> Result<Retained<NSImage>> {
    let data = NSData::with_bytes(png);
    let image = NSImage::initWithData(NSImage::alloc(), &data).context("make an image")?;

    image.setSize(NSSize::new(icon::POINTS, icon::POINTS));
    image.setTemplate(true);

    Ok(image)
}

fn frame_of(rect: NSRect) -> Frame {
    Frame::new(
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

fn screen_of(screen: &NSScreen) -> Option<Screen> {
    let description = screen.deviceDescription();
    let number = description.objectForKey(&NSString::from_str("NSScreenNumber"))?;
    let number: &AnyObject = &number;

    // SAFETY: AppKit documents `NSScreenNumber` as an NSNumber with the CGDirectDisplayID.
    let display: u32 = unsafe { msg_send![number, unsignedIntValue] };

    Some(Screen {
        display,
        frame: frame_of(screen.frame()),
        visible: frame_of(screen.visibleFrame()),
    })
}
