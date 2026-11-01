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

/// The accent color: links, selection and the focus ring. Status colors do not follow it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accent {
    #[default]
    Blue,
    Honey,
}

impl Accent {
    pub const ALL: [Self; 2] = [Self::Blue, Self::Honey];

    pub fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Honey => "Honey",
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
    /// Links and the focus ring. Follows the accent the user picked.
    pub accent: Hsla,
    pub accent_bg: Hsla,
    /// Working status. Always blue, so it never matches `warn` (Blocked).
    pub working: Hsla,
    pub working_bg: Hsla,
    /// Selected text in a text field.
    pub selection: Hsla,
    /// Done status and success.
    pub ok: Hsla,
    pub ok_bg: Hsla,
    /// Blocked status and attention ring.
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
    /// Honey: the cells of the honeycomb in the easter eggs. The line is the outline and
    /// the empty cell is the one not filled yet.
    pub honey: Hsla,
    pub honey_line: Hsla,
    pub honey_empty: Hsla,
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
            working,
            working_bg,
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
            honey,
            honey_line,
            honey_empty,
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
            ("working", *working),
            ("working_bg", *working_bg),
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
            ("honey", *honey),
            ("honey_line", *honey_line),
            ("honey_empty", *honey_empty),
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
                working: rgb(0x6e9bff).into(),
                working_bg: rgba(0x6e9bff21).into(),
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
                honey: rgb(0xe8a838).into(),
                honey_line: rgb(0xf3c56a).into(),
                honey_empty: rgb(0x1f1d18).into(),
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
                working: rgb(0x285acb).into(),
                working_bg: rgba(0x2f6bf017).into(),
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
                honey: rgb(0xeaa93f).into(),
                honey_line: rgb(0x8c5408).into(),
                honey_empty: rgb(0xf6efe2).into(),
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

    /// The theme with the accent's tokens. Blue is the base; Honey is amber, dark enough
    /// in light mode for text to pass WCAG AA on every surface and on its own tint.
    /// Status tokens (`working`, `warn`, `ok`, `del`) stay as they are.
    #[must_use]
    pub fn with_accent(mut self, accent: Accent) -> Self {
        if accent == Accent::Blue {
            return self;
        }

        match self.kind {
            ThemeKind::Dark => {
                self.name = "Graphite Dark Honey";
                self.colors.accent = rgb(0xe8a838).into();
                self.colors.accent_bg = rgba(0xe8a83824).into();
                self.colors.selection = rgba(0xe8a83859).into();
            }
            ThemeKind::Light => {
                self.name = "Graphite Light Honey";
                self.colors.accent = rgb(0x905500).into();
                self.colors.accent_bg = rgba(0xd99a1f1f).into();
                self.colors.selection = rgba(0xe0a82a59).into();
            }
        }

        self
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
    accent: Accent,
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
        accent: Accent::default(),
        system: cx.window_appearance().into(),
        light: Theme::light(),
        dark: Theme::dark(),
    });
}

pub fn accent(cx: &App) -> Accent {
    cx.try_global::<GlobalTheme>()
        .map_or(Accent::default(), |theme| theme.accent)
}

/// Switches the accent color and redraws every window.
pub fn set_accent(accent: Accent, cx: &mut App) {
    if !cx.has_global::<GlobalTheme>() {
        init(cx);
    }

    let theme = cx.global_mut::<GlobalTheme>();
    if theme.accent == accent {
        return;
    }

    theme.accent = accent;
    theme.light = Theme::light().with_accent(accent);
    theme.dark = Theme::dark().with_accent(accent);

    cx.refresh_windows();
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

    /// Every look: light and dark, each with the blue and the honey accent.
    fn themes() -> [Theme; 4] {
        [
            Theme::light(),
            Theme::dark(),
            Theme::light().with_accent(Accent::Honey),
            Theme::dark().with_accent(Accent::Honey),
        ]
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
        let [light, dark, ..] = themes();
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
                ("working", colors.working),
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
                (
                    ("working", colors.working),
                    ("working_bg", colors.working_bg),
                ),
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

            // Destructive button label.
            check(
                &mut failures,
                &theme,
                TEXT_AA,
                ("ground", colors.ground),
                &[("del", colors.del)],
            );

            // Diff lines: code, line numbers and signs sit on the add and delete tints.
            for tint in [("add_bg", colors.add_bg), ("del_bg", colors.del_bg)] {
                let tinted: Vec<(&str, Hsla)> = [("panel", colors.panel), ("term", colors.term)]
                    .into_iter()
                    .map(|(name, surface)| (name, composite(surface, tint.1)))
                    .collect();

                for text in [
                    ("fg", colors.fg),
                    ("muted", colors.muted),
                    ("faint", colors.faint),
                ] {
                    check(&mut failures, &theme, TEXT_AA, text, &tinted);
                }
            }
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn button_labels_meet_wcag_aa_at_rest_and_on_hover() {
        use crate::components::ButtonVariant;

        let mut failures = Vec::new();

        for theme in themes() {
            let colors = theme.colors();
            let surfaces = [
                ("ground", colors.ground),
                ("panel", colors.panel),
                ("raised", colors.raised),
                ("active", colors.active),
                ("soft", colors.soft),
            ];

            for variant in [
                ButtonVariant::Primary,
                ButtonVariant::Secondary,
                ButtonVariant::Ghost,
                ButtonVariant::Destructive,
            ] {
                let button = variant.colors_in(colors);

                for (state, background, text) in [
                    ("rest", button.background, button.text),
                    ("hover", button.hover_background, button.hover_text),
                ] {
                    // A translucent fill depends on what is behind the button.
                    let behind: Vec<(&str, Hsla)> = surfaces
                        .into_iter()
                        .map(|(name, surface)| (name, composite(surface, background)))
                        .collect();

                    check(
                        &mut failures,
                        &theme,
                        TEXT_AA,
                        (&format!("{variant:?} {state} label"), text),
                        &behind,
                    );
                }
            }
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
                ("working", colors.working),
                ("ok", colors.ok),
                ("warn", colors.warn),
            ] {
                check(&mut failures, &theme, GRAPHIC_AA, glyph, &surfaces);
            }

            // The check mark in a checked box: the ground on the accent.
            check(
                &mut failures,
                &theme,
                GRAPHIC_AA,
                ("ground", colors.ground),
                &[("accent", colors.accent)],
            );

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
    fn the_honey_accent_is_amber_and_passes_contrast_in_light_and_dark() {
        let mut failures = Vec::new();

        for (blue, honey) in [
            (Theme::light(), Theme::light().with_accent(Accent::Honey)),
            (Theme::dark(), Theme::dark().with_accent(Accent::Honey)),
        ] {
            let colors = honey.colors();
            let surfaces = [
                ("ground", colors.ground),
                ("panel", colors.panel),
                ("raised", colors.raised),
                ("active", colors.active),
                ("soft", colors.soft),
                ("term", colors.term),
            ];

            assert_ne!(colors.accent, blue.colors().accent, "{}", honey.name());
            assert!(
                (0.07..0.14).contains(&colors.accent.h),
                "{}: accent hue {} is not amber",
                honey.name(),
                colors.accent.h
            );

            check(
                &mut failures,
                &honey,
                TEXT_AA,
                ("accent", colors.accent),
                &surfaces,
            );
            check(
                &mut failures,
                &honey,
                GRAPHIC_AA,
                ("ground", colors.ground),
                &[("accent", colors.accent)],
            );

            let tinted: Vec<(&str, Hsla)> = surfaces
                .into_iter()
                .map(|(name, surface)| (name, composite(surface, colors.accent_bg)))
                .collect();
            check(
                &mut failures,
                &honey,
                TEXT_AA,
                ("accent", colors.accent),
                &tinted,
            );

            let selected: Vec<(&str, Hsla)> = surfaces
                .into_iter()
                .map(|(name, surface)| (name, composite(surface, colors.selection)))
                .collect();
            check(&mut failures, &honey, TEXT_AA, ("fg", colors.fg), &selected);
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn appearance_picks_the_theme() {
        let mut global = GlobalTheme {
            appearance: Appearance::System,
            accent: Accent::Blue,
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
