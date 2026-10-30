use std::borrow::Cow;

use gpui::App;

pub const UI_FONT: &str = "Geist";
pub const MONO_FONT: &str = "Geist Mono";

// Geist and Geist Mono 1.9.0 (SIL OFL 1.1), from github.com/vercel/geist-font.
const FONTS: [&[u8]; 6] = [
    include_bytes!("../../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-SemiBold.ttf"),
];

/// Registers the bundled fonts with GPUI's text system.
/// On failure it logs the error and the UI falls back to system fonts.
pub fn load(cx: &App) {
    let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();

    if let Err(error) = cx.text_system().add_fonts(fonts) {
        log::error!("could not load the Geist fonts: {error:#}; falling back to system fonts");
    }
}
