use gpui::{
    AnyElement, App, ClickEvent, ElementId, Role, SharedString, StyleRefinement, Styled as _,
    Window, div, px,
};

use crate::component::{Component, example, example_group};
use crate::prelude::*;
use crate::styles::focus_ring;
use crate::traits::ClickHandler;

/// The height of a row.
pub const TREE_ROW_HEIGHT: f32 = 26.;

/// How far each level moves a row to the right. It is also the width of the chevron cell, so
/// an indent guide runs under the chevron of its folder.
pub const TREE_INDENT: f32 = 16.;

const TREE_PADDING: f32 = 6.;
const GUIDE_OFFSET: f32 = 7.;

/// A row of a tree, styled like a project panel: indent guides, a chevron for a folder, a
/// plain icon, a label that truncates and a slot at the end. Stack rows with no gap, so the
/// guides join up.
#[derive(IntoElement)]
#[must_use]
pub struct TreeRow {
    id: ElementId,
    label: SharedString,
    accessible_label: Option<SharedString>,
    level: usize,
    folder: Option<bool>,
    icon: Option<IconName>,
    label_color: Color,
    selected: bool,
    focusable: bool,
    ring: bool,
    end: Option<AnyElement>,
    debug_selector: Option<String>,
    on_click: Option<ClickHandler>,
}

impl TreeRow {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            accessible_label: None,
            level: 0,
            folder: None,
            icon: None,
            label_color: Color::Default,
            selected: false,
            focusable: false,
            ring: false,
            end: None,
            debug_selector: None,
            on_click: None,
        }
    }

    /// Name for `VisualTestContext::debug_bounds`. Only recorded in test builds.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// How deep the row sits, 0 for the top level.
    pub fn level(mut self, level: usize) -> Self {
        self.level = level;
        self
    }

    /// Makes the row a folder with a chevron: down when `expanded`, else right.
    pub fn folder(mut self, expanded: bool) -> Self {
        self.folder = Some(expanded);
        self
    }

    /// Replaces the folder or file icon.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label_color(mut self, color: Color) -> Self {
        self.label_color = color;
        self
    }

    /// What VoiceOver reads when the visible label is not enough, for example
    /// "main.rs, 12 additions, 3 deletions". Defaults to the label.
    pub fn accessible_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessible_label = Some(label.into());
        self
    }

    /// Makes the row its own stop for Tab and shows a focus ring. Leave it off when the
    /// tree is one stop that moves a selection with the arrow keys.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    /// Draws the focus ring on the row. A tree that keeps focus on its container, and
    /// moves a selection with the arrow keys, shows it on the selected row.
    pub fn ring(mut self, ring: bool) -> Self {
        self.ring = ring;
        self
    }

    /// Content after the label, for example counts.
    pub fn end_slot(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }
}

impl Toggleable for TreeRow {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Clickable for TreeRow {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TreeRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (active, raised, accent, line) =
            (colors.active, colors.raised, colors.accent, colors.line);

        #[expect(
            clippy::cast_precision_loss,
            reason = "nesting depth is a handful of levels"
        )]
        let level_offset = |level: usize| TREE_PADDING + TREE_INDENT * level as f32;

        let icon = self.icon.unwrap_or(match self.folder {
            Some(_) => IconName::Folder,
            None => IconName::File,
        });
        let chevron = self.folder.map(|expanded| {
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(IconSize::Small)
            .color(Color::Muted)
        });
        let guide_selector = self.debug_selector.clone();
        let guides = (0..self.level).map(|level| {
            div()
                .when_some(guide_selector.clone(), |guide, selector| {
                    guide.debug_selector(move || format!("{selector}-guide-{level}"))
                })
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(level_offset(level) + GUIDE_OFFSET))
                .w(px(1.))
                .bg(line)
        });

        div()
            .id(self.id)
            .when_some(self.debug_selector, |row, selector| {
                row.debug_selector(move || selector)
            })
            .role(Role::TreeItem)
            .aria_label(self.accessible_label.unwrap_or_else(|| self.label.clone()))
            .aria_level(self.level + 1)
            .when_some(self.folder, StatefulInteractiveElement::aria_expanded)
            .aria_selected(self.selected)
            .when(self.selected && !self.focusable, |row| {
                row.aria_active_descendant()
            })
            .when(self.focusable, |row| {
                row.focusable()
                    .tab_stop(true)
                    .focus_visible(move |style| focus_ring(style, accent))
            })
            .cursor_pointer()
            .relative()
            .flex()
            .items_center()
            .h(px(TREE_ROW_HEIGHT))
            .w_full()
            .pl(px(level_offset(self.level)))
            .pr(px(8.))
            .gap(px(6.))
            .rounded(px(5.))
            .when(self.selected, |row| row.bg(active))
            .when(!self.selected, |row| {
                row.hover(move |style| style.bg(raised))
            })
            .when(self.ring, |mut row| {
                row.style()
                    .refine(&focus_ring(StyleRefinement::default(), accent));

                row
            })
            .children(guides)
            .child(
                div()
                    .w(px(TREE_INDENT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(chevron),
            )
            .child(Icon::new(icon).size(IconSize::Medium).color(Color::Muted))
            .child(
                div().flex_1().min_w_0().flex().child(
                    Label::new(self.label)
                        .size(LabelSize::Small)
                        .color(self.label_color)
                        .truncate(),
                ),
            )
            .when_some(self.end, ParentElement::child)
            .when_some(self.on_click, |row, handler| row.on_click(handler))
    }
}

/// The level, the label, whether a folder is open, and the added and deleted counts.
type DemoRow = (
    usize,
    &'static str,
    Option<bool>,
    Option<(&'static str, &'static str)>,
);

impl Component for TreeRow {
    fn name() -> &'static str {
        "TreeRow"
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        let state = window.use_keyed_state("tree-row-demo", cx, |_, _| 3_usize);
        let selected = *state.read(cx);

        let rows: [DemoRow; 9] = [
            (0, "crates/swarm_term/src", Some(true), None),
            (1, "render", Some(true), None),
            (2, "element.rs", None, Some(("+48", "\u{2212}12"))),
            (2, "grid.rs", None, Some(("+22", "\u{2212}9"))),
            (1, "font.rs", None, Some(("+18", "\u{2212}0"))),
            (0, "docs", Some(false), None),
            (0, "benches", Some(true), None),
            (
                1,
                "a_file_with_a_very_long_name_that_has_to_be_cut.rs",
                None,
                None,
            ),
            (0, "Cargo.lock", None, Some(("+1,690", "\u{2212}312"))),
        ];

        let tree = div()
            .id("tree-demo")
            .role(Role::Tree)
            .aria_label("Changed files")
            .w(px(272.))
            .flex()
            .flex_col()
            .children(rows.into_iter().enumerate().map(
                |(index, (level, label, folder, counts))| {
                    let state = state.clone();
                    let row = TreeRow::new(("tree-demo-row", index), label)
                        .level(level)
                        .toggle_state(index == selected)
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| {
                                *state = index;
                                cx.notify();
                            });
                        });
                    let row = match folder {
                        Some(expanded) => row.folder(expanded),
                        None => row,
                    };

                    match counts {
                        Some((added, deleted)) => row.end_slot(
                            h_flex()
                                .flex_none()
                                .gap(px(4.))
                                .child(
                                    Label::new(added)
                                        .mono()
                                        .size(LabelSize::XSmall)
                                        .color(Color::Add),
                                )
                                .child(
                                    Label::new(deleted)
                                        .mono()
                                        .size(LabelSize::XSmall)
                                        .color(Color::Del),
                                ),
                        ),
                        None => row,
                    }
                },
            ));

        v_flex()
            .gap_6()
            .child(example_group(
                "Folders and files",
                [example("Selectable, with indent guides", tree)],
            ))
            .child(example_group(
                "Focusable row",
                [example(
                    "Own Tab stop",
                    div()
                        .id("tree-demo-focusable")
                        .role(Role::Tree)
                        .aria_label("Files")
                        .w(px(272.))
                        .child(
                            TreeRow::new("tree-demo-focus", "main.rs")
                                .focusable(true)
                                .label_color(Color::Accent),
                        ),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};

    use super::*;

    struct Rows;

    impl Render for Rows {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .w(px(240.))
                .child(
                    TreeRow::new("folder", "crates")
                        .folder(true)
                        .debug_selector(|| "folder".to_owned()),
                )
                .child(
                    TreeRow::new("file", "main.rs")
                        .level(1)
                        .debug_selector(|| "file".to_owned()),
                )
                .child(
                    TreeRow::new("deep", "lib.rs")
                        .level(2)
                        .debug_selector(|| "deep".to_owned()),
                )
        }
    }

    fn rows(app: &mut TestAppContext) -> &mut gpui::VisualTestContext {
        app.update(crate::theme::init);
        let (_, cx) = app.add_window_view(|_, _| Rows);
        cx.run_until_parked();

        cx
    }

    #[test]
    fn rows_are_24_to_26_pixels_high_and_stack_without_a_gap() {
        let mut app = TestAppContext::single();
        let cx = rows(&mut app);

        let folder = cx.debug_bounds("folder");
        let file = cx.debug_bounds("file");

        assert!((24.0..=26.0).contains(&TREE_ROW_HEIGHT));
        assert_eq!(
            folder.map(|bounds| bounds.size.height),
            Some(px(TREE_ROW_HEIGHT))
        );
        assert_eq!(
            file.map(|bounds| bounds.size.height),
            Some(px(TREE_ROW_HEIGHT))
        );
        assert_eq!(
            folder
                .zip(file)
                .map(|(above, below)| below.top() - above.bottom()),
            Some(px(0.))
        );
    }

    #[test]
    fn guides_are_one_pixel_lines_one_indent_apart() {
        let mut app = TestAppContext::single();
        let cx = rows(&mut app);

        assert!((14.0..=16.0).contains(&TREE_INDENT));
        for (name, guides) in [("folder", 0), ("file", 1), ("deep", 2)] {
            let row = cx.debug_bounds(name);
            let drawn: Vec<_> = (0..3)
                .filter_map(|level| {
                    cx.debug_bounds(Box::leak(format!("{name}-guide-{level}").into_boxed_str()))
                })
                .collect();

            assert_eq!(drawn.len(), guides, "{name}");
            for (level, guide) in drawn.iter().enumerate() {
                let row = row.unwrap_or_default();

                assert_eq!(guide.size.width, px(1.), "{name} guide {level} is not 1px");
                assert_eq!(guide.size.height, row.size.height, "{name} guide {level}");
                assert_eq!(guide.top(), row.top());
            }
        }

        let first = cx.debug_bounds("deep-guide-0").map(|guide| guide.left());
        let second = cx.debug_bounds("deep-guide-1").map(|guide| guide.left());
        assert_eq!(
            first.zip(second).map(|(first, second)| second - first),
            Some(px(TREE_INDENT))
        );
    }

    #[test]
    fn a_guide_runs_under_the_chevron_of_its_folder() {
        let mut app = TestAppContext::single();
        let cx = rows(&mut app);

        let row = cx.debug_bounds("file").unwrap_or_default();
        let guide = cx.debug_bounds("file-guide-0").unwrap_or_default();
        let chevron_center = row.left() + px(TREE_PADDING) + px(TREE_INDENT / 2.);

        assert!((guide.center().x - chevron_center).abs() <= px(1.));
    }
}
