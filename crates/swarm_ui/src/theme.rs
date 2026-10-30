use std::sync::LazyLock;

use gpui::{App, Global, Hsla, Subscription, Window, WindowAppearance, rgb, rgba};

/// Which theme the user picked. `System` follows macOS light and dark mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    System,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::Light, Self::Dark, Self::System];

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::System => "System",
        }
    }
}

/// Whether a theme is light or dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    Light,
    Dark,
}

impl From<WindowAppearance> for ThemeKind {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
        }
    }
}

impl From<ThemeKind> for WindowAppearance {
    fn from(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Light => Self::Light,
            ThemeKind::Dark => Self::Dark,
        }
    }
}

/// Semantic color tokens. Components read these and never use raw colors.
///
/// Values come from design direction A (Graphite). A few light tokens and the
/// dark `faint` are a little darker or lighter than the mockup so text meets
/// WCAG AA; see the contrast tests.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeColors {
    /// Window background, behind panels.
    pub ground: Hsla,
    /// Main surfaces: panels, tile headers, cards.
    pub panel: Hsla,
    /// Raised controls: selected tab, secondary button, inputs.
    pub raised: Hsla,
    /// Selected rows and pressed or hovered controls.
    pub active: Hsla,
    /// Borders and dividers.
    pub line: Hsla,
    /// Quiet fills: idle pill, progress track, prompt row.
    pub soft: Hsla,
    /// Primary text and icons.
    pub fg: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    /// Tertiary text: meta, counts, section titles.
    pub faint: Hsla,
    /// Working status, links, focus ring.
    pub accent: Hsla,
    pub accent_bg: Hsla,
    /// Selected text in a text field.
    pub selection: Hsla,
    /// Done status and success.
    pub ok: Hsla,
    pub ok_bg: Hsla,
    /// Waiting status and attention ring.
    pub warn: Hsla,
    pub warn_bg: Hsla,
    /// Diff additions and deletions.
    pub add: Hsla,
    pub del: Hsla,
    pub add_bg: Hsla,
    pub del_bg: Hsla,
    /// Terminal background.
    pub term: Hsla,
    /// Faint decoration: graph dots, honeycomb pattern.
    pub dot: Hsla,
    pub hex: Hsla,
    /// Switch track and knob, on and off.
    pub switch_on: Hsla,
    pub switch_off: Hsla,
    pub knob_on: Hsla,
    pub knob_off: Hsla,
    /// Dimmed backdrop behind a dialog.
    pub scrim: Hsla,
    /// Claude Code brand mark. Same in both themes.
    pub claude: Hsla,
    /// Letter on a project tile, which has a saturated per-project color.
    pub on_tile: Hsla,
    /// Terminal colors 0-7 (black to white), then the bright 8-15.
    pub ansi: [Hsla; 16],
}

const ANSI_NAMES: [&str; 16] = [
    "ansi_0", "ansi_1", "ansi_2", "ansi_3", "ansi_4", "ansi_5", "ansi_6", "ansi_7", "ansi_8",
    "ansi_9", "ansi_10", "ansi_11", "ansi_12", "ansi_13", "ansi_14", "ansi_15",
];

impl ThemeColors {
    /// Every token with its name. Destructuring makes this fail to compile
    /// when a token is added but not listed.
    pub fn tokens(&self) -> Vec<(&'static str, Hsla)> {
        let Self {
            ground,
            panel,
            raised,
            active,
            line,
            soft,
            fg,
            muted,
            faint,
            accent,
            accent_bg,
            selection,
            ok,
            ok_bg,
            warn,
            warn_bg,
            add,
            del,
            add_bg,
            del_bg,
            term,
            dot,
            hex,
            switch_on,
            switch_off,
            knob_on,
            knob_off,
            scrim,
            claude,
            on_tile,
            ansi,
        } = self;

        let mut tokens = vec![
            ("ground", *ground),
            ("panel", *panel),
            ("raised", *raised),
            ("active", *active),
            ("line", *line),
            ("soft", *soft),
            ("fg", *fg),
            ("muted", *muted),
            ("faint", *faint),
            ("accent", *accent),
            ("accent_bg", *accent_bg),
            ("selection", *selection),
            ("ok", *ok),
            ("ok_bg", *ok_bg),
            ("warn", *warn),
            ("warn_bg", *warn_bg),
            ("add", *add),
            ("del", *del),
            ("add_bg", *add_bg),
            ("del_bg", *del_bg),
            ("term", *term),
            ("dot", *dot),
            ("hex", *hex),
            ("switch_on", *switch_on),
            ("switch_off", *switch_off),
            ("knob_on", *knob_on),
            ("knob_off", *knob_off),
            ("scrim", *scrim),
            ("claude", *claude),
            ("on_tile", *on_tile),
        ];

        tokens.extend(ANSI_NAMES.into_iter().zip(ansi.iter().copied()));

        tokens
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    name: &'static str,
    kind: ThemeKind,
    colors: ThemeColors,
}

#[expect(
    clippy::unreadable_literal,
    reason = "hex colors stay in #rrggbb form to match the design files"
)]
impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "Graphite Dark",
            kind: ThemeKind::Dark,
            colors: ThemeColors {
                ground: rgb(0x0b0b0c).into(),
                panel: rgb(0x111113).into(),
                raised: rgb(0x18181b).into(),
                active: rgb(0x202024).into(),
                line: rgb(0x26262b).into(),
                soft: rgb(0x1d1d21).into(),
                fg: rgb(0xededef).into(),
                muted: rgb(0xa1a1aa).into(),
                // Mockup: #86868f, 4.50:1 on `active`; nudged up to clear AA.
                faint: rgb(0x878790).into(),
                accent: rgb(0x6e9bff).into(),
                accent_bg: rgba(0x6e9bff21).into(),
                selection: rgba(0x6e9bff59).into(),
                ok: rgb(0x4ac26b).into(),
                ok_bg: rgba(0x4ac26b1f).into(),
                warn: rgb(0xe8a838).into(),
                warn_bg: rgba(0xe8a83824).into(),
                add: rgb(0x4ac26b).into(),
                del: rgb(0xf2716a).into(),
                add_bg: rgba(0x4ac26b1a).into(),
                del_bg: rgba(0xf2716a1a).into(),
                term: rgb(0x0d0d0f).into(),
                dot: rgb(0x1f1f23).into(),
                hex: rgb(0x17171a).into(),
                switch_on: rgb(0xededef).into(),
                switch_off: rgb(0x3a3a40).into(),
                knob_on: rgb(0x111113).into(),
                knob_off: rgb(0xa1a1aa).into(),
                scrim: rgba(0x0000008c).into(),
                claude: rgb(0xd97757).into(),
                on_tile: rgb(0xffffff).into(),
                ansi: [
                    rgb(0x2a2a2e).into(),
                    rgb(0xff6369).into(),
                    rgb(0x4ccd7a).into(),
                    rgb(0xf2c55c).into(),
                    rgb(0x6e9bff).into(),
                    rgb(0xc792ea).into(),
                    rgb(0x5ccfe6).into(),
                    rgb(0xc8c8cf).into(),
                    rgb(0x797984).into(),
                    rgb(0xff8a8f).into(),
                    rgb(0x7ee2a0).into(),
                    rgb(0xffd98a).into(),
                    rgb(0x9bbcff).into(),
                    rgb(0xdcb3f5).into(),
                    rgb(0x8ce3f2).into(),
                    rgb(0xf4f4f6).into(),
                ],
            },
        }
    }

    pub fn light() -> Self {
        Self {
            name: "Graphite Light",
            kind: ThemeKind::Light,
            colors: ThemeColors {
                ground: rgb(0xf3f3f4).into(),
                panel: rgb(0xffffff).into(),
                raised: rgb(0xf7f7f8).into(),
                active: rgb(0xebebee).into(),
                line: rgb(0xe3e3e6).into(),
                soft: rgb(0xeeeef1).into(),
                fg: rgb(0x18181b).into(),
                muted: rgb(0x5f5f68).into(),
                // Mockup values in comments; darkened so text passes 4.5:1 on
                // every surface and on its own tinted pill background.
                faint: rgb(0x696971).into(),  // #6b6b74
                accent: rgb(0x285acb).into(), // #2f6bf0
                accent_bg: rgba(0x2f6bf017).into(),
                selection: rgba(0x285acb40).into(),
                ok: rgb(0x176f30).into(), // #1a7f36
                ok_bg: rgba(0x1a7f361a).into(),
                warn: rgb(0x8c5408).into(), // #a8650a
                warn_bg: rgba(0xc882141f).into(),
                add: rgb(0x176f30).into(), // #1a7f36
                del: rgb(0xb63128).into(), // #c9362c
                add_bg: rgba(0x1a7f3614).into(),
                del_bg: rgba(0xc9362c12).into(),
                term: rgb(0xfbfbfc).into(),
                dot: rgb(0xe4e4e8).into(),
                hex: rgb(0xe9e9ec).into(),
                switch_on: rgb(0x18181b).into(),
                // Mockup: #d4d4d8. A white knob on it was 1.5:1, so the off
                // state was invisible; this gives 3:1.
                switch_off: rgb(0x92929d).into(),
                knob_on: rgb(0xffffff).into(),
                knob_off: rgb(0xffffff).into(),
                scrim: rgba(0x00000066).into(),
                claude: rgb(0xd97757).into(),
                on_tile: rgb(0xffffff).into(),
                ansi: [
                    rgb(0x1f1f23).into(),
                    rgb(0xb63128).into(),
                    rgb(0x176f30).into(),
                    rgb(0x8c5408).into(),
                    rgb(0x285acb).into(),
                    rgb(0x8a3fb8).into(),
                    rgb(0x0f6f80).into(),
                    rgb(0x6b6b73).into(),
                    rgb(0x5c5c66).into(),
                    rgb(0xd43c33).into(),
                    rgb(0x1e853b).into(),
                    rgb(0xa06812).into(),
                    rgb(0x3a6ee0).into(),
                    rgb(0x9f4fd0).into(),
                    rgb(0x137f94).into(),
                    rgb(0x8e8e98).into(),
                ],
            },
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn kind(&self) -> ThemeKind {
        self.kind
    }

    pub fn colors(&self) -> &ThemeColors {
        &self.colors
    }
}

struct GlobalTheme {
    appearance: Appearance,
    system: ThemeKind,
    light: Theme,
    dark: Theme,
}

impl GlobalTheme {
    fn active_kind(&self) -> ThemeKind {
        match self.appearance {
            Appearance::Light => ThemeKind::Light,
            Appearance::Dark => ThemeKind::Dark,
            Appearance::System => self.system,
        }
    }

    fn active(&self) -> &Theme {
        match self.active_kind() {
            ThemeKind::Light => &self.light,
            ThemeKind::Dark => &self.dark,
        }
    }
}

impl Global for GlobalTheme {}

/// Lets components read the active theme with `cx.theme()`.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

// Used only when `init` was not called, so rendering never panics.
static FALLBACK_THEME: LazyLock<Theme> = LazyLock::new(Theme::dark);

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.try_global::<GlobalTheme>()
            .map_or(&*FALLBACK_THEME, GlobalTheme::active)
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(GlobalTheme {
        appearance: Appearance::System,
        system: cx.window_appearance().into(),
        light: Theme::light(),
        dark: Theme::dark(),
    });
}

pub fn appearance(cx: &App) -> Appearance {
    cx.try_global::<GlobalTheme>()
        .map_or(Appearance::default(), |theme| theme.appearance)
}

/// Switches the theme and redraws every window.
pub fn set_appearance(appearance: Appearance, cx: &mut App) {
    // Forcing the app appearance also makes the native window chrome match the
    // theme. `None` hands control back to macOS.
    let forced = match appearance {
        Appearance::Light => Some(WindowAppearance::Light),
        Appearance::Dark => Some(WindowAppearance::Dark),
        Appearance::System => None,
    };

    cx.set_window_appearance(forced);
    let system = cx.window_appearance().into();

    if !cx.has_global::<GlobalTheme>() {
        init(cx);
    }

    let theme = cx.global_mut::<GlobalTheme>();
    if theme.appearance == appearance && theme.system == system {
        return;
    }

    theme.appearance = appearance;
    if appearance == Appearance::System {
        theme.system = system;
    }

    cx.refresh_windows();
}

/// Keeps `Appearance::System` in sync with macOS while `window` is open.
/// Keep the returned subscription alive, for example in the root view.
pub fn follow_system_appearance(window: &mut Window, cx: &mut App) -> Subscription {
    sync_system_kind(window.appearance().into(), cx);

    window.observe_window_appearance(|window, cx| {
        sync_system_kind(window.appearance().into(), cx);
    })
}

fn sync_system_kind(kind: ThemeKind, cx: &mut App) {
    if !cx.has_global::<GlobalTheme>() {
        init(cx);
    }

    let theme = cx.global_mut::<GlobalTheme>();
    // While the user forces light or dark, the window reports that forced value,
    // not the system one.
    if theme.appearance != Appearance::System || theme.system == kind {
        return;
    }

    theme.system = kind;
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contrast::{GRAPHIC_AA, TEXT_AA, composite, contrast_ratio};

    fn themes() -> [Theme; 2] {
        [Theme::light(), Theme::dark()]
    }

    /// Checks `foreground` on each background and collects every failure,
    /// so one run reports all of them.
    fn check(
        failures: &mut Vec<String>,
        theme: &Theme,
        minimum: f32,
        foreground: (&str, Hsla),
        backgrounds: &[(&str, Hsla)],
    ) {
        for (background_name, background) in backgrounds {
            let ratio = contrast_ratio(foreground.1, *background);

            if ratio < minimum {
                failures.push(format!(
                    "{}: {} on {background_name} is {ratio:.2}, needs {minimum}",
                    theme.name(),
                    foreground.0,
                ));
            }
        }
    }

    #[test]
    fn light_and_dark_define_the_same_tokens() {
        let [light, dark] = themes();
        let light_names: Vec<_> = light
            .colors()
            .tokens()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let dark_names: Vec<_> = dark
            .colors()
            .tokens()
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert_eq!(light_names, dark_names);
        assert_eq!(light.kind(), ThemeKind::Light);
        assert_eq!(dark.kind(), ThemeKind::Dark);
    }

    #[test]
    fn only_tint_tokens_are_translucent() {
        for theme in themes() {
            for (name, color) in theme.colors().tokens() {
                let tint = name.ends_with("_bg") || name == "scrim" || name == "selection";
                assert_eq!(
                    color.a < 1.0,
                    tint,
                    "{}: {name} alpha {}",
                    theme.name(),
                    color.a
                );
            }
        }
    }

    #[test]
    fn text_meets_wcag_aa() {
        let mut failures = Vec::new();

        for theme in themes() {
            let colors = theme.colors();
            let surfaces = [
                ("ground", colors.ground),
                ("panel", colors.panel),
                ("raised", colors.raised),
                ("active", colors.active),
                ("soft", colors.soft),
                ("term", colors.term),
            ];

            for text in [
                ("fg", colors.fg),
                ("muted", colors.muted),
                ("faint", colors.faint),
                ("accent", colors.accent),
                ("ok", colors.ok),
                ("warn", colors.warn),
                ("add", colors.add),
                ("del", colors.del),
            ] {
                check(&mut failures, &theme, TEXT_AA, text, &surfaces);
            }

            // Status pills and diff counts sit on a tint over a surface.
            for (text, tint) in [
                (("accent", colors.accent), ("accent_bg", colors.accent_bg)),
                (("warn", colors.warn), ("warn_bg", colors.warn_bg)),
                (("ok", colors.ok), ("ok_bg", colors.ok_bg)),
                (("add", colors.add), ("add_bg", colors.add_bg)),
                (("del", colors.del), ("del_bg", colors.del_bg)),
            ] {
                let tinted: Vec<(&str, Hsla)> = surfaces
                    .into_iter()
                    .map(|(name, surface)| (name, composite(surface, tint.1)))
                    .collect();

                check(&mut failures, &theme, TEXT_AA, text, &tinted);
            }

            // Primary button label.
            check(
                &mut failures,
                &theme,
                TEXT_AA,
                ("ground", colors.ground),
                &[("fg", colors.fg)],
            );
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn ansi_text_colors_meet_wcag_aa_on_the_terminal_background() {
        let mut failures = Vec::new();

        for theme in themes() {
            let colors = theme.colors();
            let background = [("term", colors.term)];

            // Dark ansi_0 and light ansi_15 are background-like colors, used for
            // fills and selections rather than text, so they are exempt.
            let range = match theme.kind() {
                ThemeKind::Dark => 1..16,
                ThemeKind::Light => 1..15,
            };

            for index in range {
                check(
                    &mut failures,
                    &theme,
                    TEXT_AA,
                    (ANSI_NAMES[index], colors.ansi[index]),
                    &background,
                );
            }
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn glyphs_and_rings_meet_wcag_aa() {
        let mut failures = Vec::new();

        for theme in themes() {
            let colors = theme.colors();
            let surfaces = [
                ("ground", colors.ground),
                ("panel", colors.panel),
                ("raised", colors.raised),
                ("active", colors.active),
                ("term", colors.term),
            ];

            // Icons, status marks, the focus ring (accent) and the attention ring (warn).
            // The Claude Code mark is left out: logos are exempt (WCAG 1.4.11).
            for glyph in [
                ("fg", colors.fg),
                ("muted", colors.muted),
                ("faint", colors.faint),
                ("accent", colors.accent),
                ("ok", colors.ok),
                ("warn", colors.warn),
            ] {
                check(&mut failures, &theme, GRAPHIC_AA, glyph, &surfaces);
            }

            check(
                &mut failures,
                &theme,
                GRAPHIC_AA,
                ("knob_on", colors.knob_on),
                &[("switch_on", colors.switch_on)],
            );
            check(
                &mut failures,
                &theme,
                GRAPHIC_AA,
                ("knob_off", colors.knob_off),
                &[("switch_off", colors.switch_off)],
            );
            check(
                &mut failures,
                &theme,
                GRAPHIC_AA,
                ("switch_on", colors.switch_on),
                &[("panel", colors.panel)],
            );

            // An off switch must still be visible: its track or its knob stands out.
            let off_visible = contrast_ratio(colors.switch_off, colors.panel)
                .max(contrast_ratio(colors.knob_off, colors.panel));

            if off_visible < GRAPHIC_AA {
                failures.push(format!(
                    "{}: off switch is {off_visible:.2} on panel",
                    theme.name()
                ));
            }
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn focus_ring_reaches_3_to_1_on_its_surroundings() {
        let mut failures = Vec::new();

        for theme in themes() {
            let colors = theme.colors();

            check(
                &mut failures,
                &theme,
                GRAPHIC_AA,
                ("accent", colors.accent),
                &[
                    ("ground", colors.ground),
                    ("panel", colors.panel),
                    ("raised", colors.raised),
                    ("active", colors.active),
                ],
            );
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn appearance_picks_the_theme() {
        let mut global = GlobalTheme {
            appearance: Appearance::System,
            system: ThemeKind::Dark,
            light: Theme::light(),
            dark: Theme::dark(),
        };
        assert_eq!(global.active().kind(), ThemeKind::Dark);

        global.system = ThemeKind::Light;
        assert_eq!(global.active().kind(), ThemeKind::Light);

        global.appearance = Appearance::Dark;
        assert_eq!(global.active().kind(), ThemeKind::Dark);

        global.appearance = Appearance::Light;
        global.system = ThemeKind::Dark;
        assert_eq!(global.active().kind(), ThemeKind::Light);
    }
}
