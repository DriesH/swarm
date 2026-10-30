//! The usage popover and a stand-in main window.

use chrono::Utc;
use gpui::{
    App, Context, Entity, FocusHandle, Hsla, IntoElement, ParentElement as _, Render, Role,
    SharedString, Styled as _, Window, WindowAppearance, actions, div, prelude::*, px, relative,
    rgb,
};

use crate::app::{self, Model};
use crate::usage::{SourceState, short_duration};

actions!(menubar_spike, [Dismiss]);

#[derive(Clone, Copy)]
struct Theme {
    bg: Hsla,
    fg: Hsla,
    muted: Hsla,
    track: Hsla,
    fill: Hsla,
    warn: Hsla,
    hover: Hsla,
    border: Hsla,
}

impl Theme {
    fn for_window(window: &Window) -> Self {
        match window.appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                bg: rgb(0x1c1c1e).into(),
                fg: rgb(0xf2f2f7).into(),
                muted: rgb(0xa1a1aa).into(),
                track: rgb(0x3a3a3c).into(),
                fill: rgb(0x0a84ff).into(),
                warn: rgb(0xff9f0a).into(),
                hover: rgb(0x2c2c2e).into(),
                border: rgb(0x38383a).into(),
            },
            _ => Self {
                bg: rgb(0xffffff).into(),
                fg: rgb(0x1c1c1e).into(),
                muted: rgb(0x5f5f66).into(),
                track: rgb(0xe5e5ea).into(),
                fill: rgb(0x0060df).into(),
                warn: rgb(0xb25000).into(),
                hover: rgb(0xf2f2f7).into(),
                border: rgb(0xd1d1d6).into(),
            },
        }
    }
}

pub struct Popover {
    model: Entity<Model>,
    focus: FocusHandle,
    /// The snapshot copy lives in a normal window and must not close itself.
    close_on_blur: bool,
}

impl Popover {
    pub fn new(
        model: Entity<Model>,
        close_on_blur: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        if close_on_blur {
            cx.observe_window_activation(window, |this, window, _| {
                if this.close_on_blur && !window.is_window_active() {
                    window.remove_window();
                }
            })
            .detach();
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            model,
            focus,
            close_on_blur,
        }
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_window(window);
        let m = self.model.read(cx);
        let agents = match m.agents {
            1 => "1 agent running".to_string(),
            n => format!("{n} agents running"),
        };

        div()
            .id("popover")
            .key_context("Popover")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Dismiss, window, _| {
                if this.close_on_blur {
                    window.remove_window();
                }
            }))
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .text_sm()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .pt_4()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Swarm"))
                    .child(
                        div()
                            .text_color(if m.needs_you { t.warn } else { t.muted })
                            .child(if m.needs_you { "Needs you" } else { "" }),
                    ),
            )
            .child(section("Claude", &m.claude, t))
            .child(section("Codex", &m.codex, t))
            .child(div().h(px(1.)).bg(t.border))
            .child(div().text_color(t.muted).child(agents))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(menu_item("open-swarm", "Open Swarm", t, app::open_main))
                    .child(menu_item("stop-all", "Stop all", t, app::stop_all))
                    .child(menu_item("quit", "Quit Swarm", t, |cx| cx.quit())),
            )
    }
}

fn section(name: &'static str, state: &SourceState, t: Theme) -> impl IntoElement {
    let now = Utc::now();
    let plan = state
        .usage
        .as_ref()
        .and_then(|u| u.plan.clone())
        .map(|p| format!(" · {p}"))
        .unwrap_or_default();
    let mut col = div().flex().flex_col().gap_1p5().child(
        div()
            .flex()
            .justify_between()
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(format!("{name}{plan}")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if state.problem.is_some() {
                        t.warn
                    } else {
                        t.muted
                    })
                    .child(
                        state
                            .problem
                            .as_ref()
                            .map(|p| p.label(now))
                            .unwrap_or_default(),
                    ),
            ),
    );
    match &state.usage {
        None if state.problem.is_none() => {
            col = col.child(div().text_xs().text_color(t.muted).child("Loading…"))
        }
        None => {}
        Some(u) => {
            for w in &u.windows {
                let resets = w
                    .resets_at
                    .map(|r| format!("resets in {}", short_duration(r - now)))
                    .unwrap_or_default();
                let label: SharedString =
                    format!("{name} {}, {:.0}% used, {resets}", w.label, w.used_percent).into();
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_xs()
                                .child(format!("{} · {:.0}%", w.label, w.used_percent))
                                .child(div().text_color(t.muted).child(resets)),
                        )
                        .child(
                            div()
                                .id(label.clone())
                                .role(Role::ProgressIndicator)
                                .aria_label(label)
                                .h(px(6.))
                                .w_full()
                                .rounded_full()
                                .bg(t.track)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(if w.used_percent >= 90. {
                                            t.warn
                                        } else {
                                            t.fill
                                        })
                                        .w(relative(w.used_percent / 100.)),
                                ),
                        ),
                );
            }
        }
    }
    col
}

fn menu_item(
    id: &'static str,
    label: &'static str,
    t: Theme,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .px_2()
        .py_1()
        .rounded_md()
        .hover(move |s| s.bg(t.hover))
        .on_click(move |_, _, cx| on_click(cx))
        .child(label)
}

pub struct MainView {
    model: Entity<Model>,
}

impl MainView {
    pub fn new(model: Entity<Model>, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self { model }
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_window(window);
        let m = self.model.read(cx);
        div()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child("Main window stand-in")
            .child(div().text_sm().text_color(t.muted).child(format!(
                "{} agents · close me, the menu bar icon stays",
                m.agents
            )))
    }
}
