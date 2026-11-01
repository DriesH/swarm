//! A live terminal in a `TileFrame`. It lives here because `swarm_ui` cannot depend on
//! the terminal crates.

use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div, px, size};
use swarm_term_view::{DEFAULT_FONT_SIZE, TerminalView};
use swarm_term_vt::{DEFAULT_SCROLLBACK_BYTES, LocalTerminal, SpawnOptions};
use swarm_ui::component::{Component, example_group};
use swarm_ui::prelude::*;

const TILE_WIDTH: f32 = 760.;
const TILE_HEIGHT: f32 = 420.;
// The tile's header, border and body padding.
const CHROME_WIDTH: f32 = 26.;
const CHROME_HEIGHT: f32 = 56.;

const SCRIPT: &str = r#"
printf '\033[1mbold\033[0m \033[3mitalic\033[0m \033[2mfaint\033[0m \033[4munderline\033[0m \033[9mstrikethrough\033[0m \033[7minverse\033[0m\n'
printf 'fg  '; for n in 0 1 2 3 4 5 6 7; do printf '\033[3%dm██' $n; done; printf ' '; for n in 0 1 2 3 4 5 6 7; do printf '\033[9%dm██' $n; done; printf '\033[0m\n'
printf 'bg  '; for n in 0 1 2 3 4 5 6 7; do printf '\033[4%dm  ' $n; done; printf '\033[0m '; for n in 0 1 2 3 4 5 6 7; do printf '\033[10%dm  ' $n; done; printf '\033[0m\n'
printf '256 '; for n in $(seq 16 4 231); do printf '\033[48;5;%dm ' $n; done; printf '\033[0m\n'
printf 'rgb '; for n in $(seq 0 4 255); do printf '\033[48;2;%d;%d;%dm ' $n $((255 - n)) 128; done; printf '\033[0m\n'
printf '┌───┬───┐  █▓▒░▀▄▌▐\n'
printf '│ a │ b │\n'
printf '├───┼───┤\n'
printf '│ c │ d │\n'
printf '└───┴───┘\n'
printf '日本語 🐝\n'
exec "$SHELL" -l
"#;

enum PreviewState {
    Running {
        view: Entity<TerminalView>,
        // Dropping it kills the process.
        _terminal: LocalTerminal,
    },
    Failed(SharedString),
}

impl PreviewState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        match Self::start(window, cx) {
            Ok(state) => state,
            Err(error) => {
                log::error!("cannot start the terminal preview: {error:#}");
                Self::Failed(format!("{error:#}").into())
            }
        }
    }

    fn start(window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<Self> {
        let body = size(
            px(TILE_WIDTH - CHROME_WIDTH),
            px(TILE_HEIGHT - CHROME_HEIGHT),
        );
        let grid = TerminalView::size_for(body, DEFAULT_FONT_SIZE, window, cx);

        let (terminal_channels, emulator_channels) = swarm_term::channels();
        let options = SpawnOptions {
            command: Some(vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()]),
            cwd: None,
            env: Vec::new(),
            size: grid,
            palette: swarm_term_view::palette_for(cx.theme()),
            scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
        };

        let terminal = LocalTerminal::spawn(options, emulator_channels)?;
        let view = cx
            .new(|cx| TerminalView::new(terminal_channels, "Terminal preview".into(), window, cx));

        Ok(Self::Running {
            view,
            _terminal: terminal,
        })
    }
}

pub(crate) struct TerminalPreview;

impl Component for TerminalPreview {
    fn name() -> &'static str {
        "Terminal"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("terminal-preview", cx, PreviewState::new);

        let body: AnyElement = match state.read(cx) {
            PreviewState::Running { view, .. } => {
                div().size_full().child(view.clone()).into_any_element()
            }
            PreviewState::Failed(message) => Label::new(message.clone())
                .mono()
                .color(Color::Muted)
                .into_any_element(),
        };

        let shell = swarm_term_vt::login_shell()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "shell".to_owned());

        let tile = div().w(px(TILE_WIDTH)).h(px(TILE_HEIGHT)).child(
            TileFrame::new("terminal-tile", IconName::Terminal, "terminal")
                .meta(shell)
                .child(body),
        );

        example_group("Live", [tile.into_any_element()])
    }
}
