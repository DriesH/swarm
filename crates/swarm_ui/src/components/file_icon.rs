use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, DevicePixels, Global, ImageSource, Pixels, RenderImage, Size, SvgSize, Window,
    div, img, px, size,
};

use crate::component::{Component, example, example_group};
use crate::file_icons::FileIconName;
use crate::prelude::*;
use crate::theme::ThemeKind;

/// Icons by asset and size on screen, in device pixels. A failure is kept too, so it is
/// logged once and not on every frame.
#[derive(Default)]
struct DrawnIcons(HashMap<(&'static str, i32), Option<Arc<RenderImage>>>);

impl Global for DrawnIcons {}

/// Draws the SVG at exactly `device` pixels. GPUI's `img` draws an SVG at a fixed 2x its own
/// size, which a screen then scales again.
fn draw(asset: &str, svg: &str, device: Size<DevicePixels>, cx: &App) -> Option<Arc<RenderImage>> {
    let renderer = cx.svg_renderer();
    let image = renderer
        .parse_svg(svg.as_bytes())
        .and_then(|parsed| renderer.render_parsed(&parsed, SvgSize::ExactSize(device)));

    match image {
        Ok(image) => Some(image),
        Err(error) => {
            log::error!("could not draw the file icon {asset}: {error}");

            None
        }
    }
}

/// The icon as an image for a square of `side` pixels on a screen with `scale` device pixels
/// to a pixel. `None` when the SVG does not draw.
pub(crate) fn drawn_icon(
    icon: FileIconName,
    kind: ThemeKind,
    side: Pixels,
    scale: f32,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let device = size(side, side).to_device_pixels(scale);
    let asset = icon.asset_for(kind);
    let key = (asset, device.width.0);

    if let Some(image) = cx.default_global::<DrawnIcons>().0.get(&key) {
        return image.clone();
    }

    let image = draw(asset, icon.svg(kind), device, cx);
    cx.default_global::<DrawnIcons>()
        .0
        .insert(key, image.clone());

    image
}

/// A file or folder icon in full color, from vscode-icons (CC BY-SA 4.0). It is
/// decorative: it has no label, so the row that holds it names the file.
#[derive(IntoElement)]
#[must_use]
pub struct FileIcon {
    name: FileIconName,
    size: IconSize,
    debug_selector: Option<String>,
}

impl FileIcon {
    pub fn new(name: FileIconName) -> Self {
        Self {
            name,
            size: IconSize::default(),
            debug_selector: None,
        }
    }

    /// The icon for a file name.
    pub fn file(name: &str) -> Self {
        Self::new(FileIconName::for_file(name))
    }

    /// The icon for a folder name.
    pub fn folder(name: &str, open: bool) -> Self {
        Self::new(FileIconName::for_folder(name, open))
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    /// Name for `VisualTestContext::debug_bounds`, on the image. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }
}

impl RenderOnce for FileIcon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.size.pixels();
        let kind = cx.theme().kind();
        let image = drawn_icon(self.name, kind, side, window.scale_factor(), cx);

        div()
            .size(side)
            .flex_none()
            .when_some(image, |slot, image| {
                slot.child(
                    img(ImageSource::Render(image))
                        .size(side)
                        .when_some(self.debug_selector, |image, selector| {
                            image.debug_selector(move || selector)
                        }),
                )
            })
    }
}

impl Component for FileIcon {
    fn name() -> &'static str {
        "FileIcon"
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        let named_files = [
            "Cargo.toml",
            "package.json",
            "README.md",
            "LICENSE",
            ".env",
            "ci.YML",
            "photo.PNG",
            "backup.tar.gz",
            "main.rs",
            "Cargo.lock",
        ];
        let named_folders = [
            ("src", true),
            ("tests", false),
            ("docs", true),
            ("crates", false),
        ];

        v_flex()
            .gap_6()
            .child(example_group(
                "vscode-icons, CC BY-SA 4.0",
                FileIconName::ALL.iter().map(|name| {
                    example(
                        format!("{name:?}"),
                        FileIcon::new(*name)
                            .size(IconSize::Large)
                            .debug_selector(|| format!("file-icon-{name:?}")),
                    )
                }),
            ))
            .child(example_group(
                "By file name",
                named_files
                    .into_iter()
                    .map(|name| example(name, FileIcon::file(name).size(IconSize::Large))),
            ))
            .child(example_group(
                "By folder name",
                named_folders.into_iter().map(|(name, open)| {
                    example(
                        format!("{name}{}", if open { " (open)" } else { "" }),
                        FileIcon::folder(name, open).size(IconSize::Large),
                    )
                }),
            ))
            .child(example_group(
                "Sizes",
                [
                    ("11", IconSize::XSmall),
                    ("12", IconSize::Small),
                    ("14", IconSize::Medium),
                    ("16", IconSize::Large),
                    ("24", IconSize::Custom(px(24.))),
                    ("32", IconSize::Custom(px(32.))),
                ]
                .into_iter()
                .map(|(label, size)| example(label, FileIcon::new(FileIconName::Json).size(size))),
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};

    use super::*;

    struct Icons;

    impl Render for Icons {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .gap(px(8.))
                .child(
                    FileIcon::file("Cargo.toml")
                        .size(IconSize::Large)
                        .debug_selector(|| "toml".to_owned()),
                )
                .child(
                    FileIcon::folder("src", true)
                        .debug_selector(|| "src".to_owned())
                        .size(IconSize::Medium),
                )
        }
    }

    struct Gallery;

    impl Render for Gallery {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            FileIcon::preview(window, cx)
        }
    }

    fn has_pixels(image: &RenderImage) -> bool {
        image
            .as_bytes(0)
            .is_some_and(|bytes| bytes.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0))
    }

    #[test]
    fn an_icon_is_an_image_of_the_size_asked_for() {
        let mut app = TestAppContext::single();
        app.update(crate::theme::init);
        let (_, cx) = app.add_window_view(|_, _| Icons);
        cx.run_until_parked();

        let toml = cx.debug_bounds("toml");
        let src = cx.debug_bounds("src");

        assert_eq!(toml.map(|bounds| bounds.size), Some(size(px(16.), px(16.))));
        assert_eq!(src.map(|bounds| bounds.size), Some(size(px(14.), px(14.))));
    }

    #[test]
    fn every_icon_draws_in_both_themes() {
        let app = TestAppContext::single();
        app.update(|cx| {
            for kind in [ThemeKind::Dark, ThemeKind::Light] {
                for icon in FileIconName::ALL {
                    let image = drawn_icon(*icon, kind, px(16.), 2., cx);

                    assert!(
                        image.is_some_and(|image| has_pixels(&image)),
                        "{icon:?} draws nothing in {kind:?}"
                    );
                }
            }
        });
    }

    #[test]
    fn an_icon_is_drawn_at_the_size_of_the_screen_pixels() {
        let app = TestAppContext::single();
        app.update(|cx| {
            for (side, scale, expected) in
                [(14., 1., 14), (14., 2., 28), (16., 2., 32), (16., 3., 48)]
            {
                let image = drawn_icon(FileIconName::Json, ThemeKind::Dark, px(side), scale, cx);

                assert_eq!(
                    image.map(|image| image.size(0)),
                    Some(size(DevicePixels(expected), DevicePixels(expected))),
                    "{side}px at {scale}x"
                );
            }
        });
    }

    #[test]
    fn a_drawn_icon_is_kept_for_the_next_frame() {
        let app = TestAppContext::single();
        app.update(|cx| {
            let first = drawn_icon(FileIconName::Json, ThemeKind::Dark, px(16.), 2., cx);
            let again = drawn_icon(FileIconName::Json, ThemeKind::Dark, px(16.), 2., cx);
            let other_size = drawn_icon(FileIconName::Json, ThemeKind::Dark, px(14.), 2., cx);

            assert!(matches!((&first, &again), (Some(a), Some(b)) if Arc::ptr_eq(a, b)));
            assert!(matches!((&first, &other_size), (Some(a), Some(b)) if !Arc::ptr_eq(a, b)));
        });
    }

    #[test]
    fn a_light_theme_draws_the_light_file() {
        let app = TestAppContext::single();
        app.update(|cx| {
            let dark = drawn_icon(FileIconName::Json, ThemeKind::Dark, px(16.), 2., cx);
            let light = drawn_icon(FileIconName::Json, ThemeKind::Light, px(16.), 2., cx);
            let same = |a: &Option<Arc<RenderImage>>, b: &Option<Arc<RenderImage>>| {
                a.as_ref().and_then(|a| a.as_bytes(0)) == b.as_ref().and_then(|b| b.as_bytes(0))
            };

            assert!(!same(&dark, &light));

            let dark = drawn_icon(FileIconName::Markdown, ThemeKind::Dark, px(16.), 2., cx);
            let light = drawn_icon(FileIconName::Markdown, ThemeKind::Light, px(16.), 2., cx);

            assert!(same(&dark, &light));
        });
    }

    #[test]
    fn an_svg_that_does_not_draw_gives_no_image() {
        let app = TestAppContext::single();
        let device = size(DevicePixels(16), DevicePixels(16));

        app.update(|cx| {
            assert!(draw("broken", "not an svg", device, cx).is_none());
            assert!(draw("empty", "", device, cx).is_none());
        });
    }

    #[test]
    fn the_gallery_shows_every_icon() {
        let mut app = TestAppContext::single();
        app.update(crate::theme::init);
        let (_, cx) = app.add_window_view(|_, _| Gallery);
        cx.run_until_parked();

        for icon in FileIconName::ALL {
            let selector: &'static str = Box::leak(format!("file-icon-{icon:?}").into_boxed_str());

            assert!(cx.debug_bounds(selector).is_some(), "{icon:?} is not shown");
        }
    }
}
