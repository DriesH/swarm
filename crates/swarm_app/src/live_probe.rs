//! Test helper: reads the accessibility node of a rendered element. The test platform builds
//! no accessibility tree, so tests read the node the way `swarm_ui`'s own tests do.

use gpui::accesskit::{Live, Node, Role};
use gpui::{App, Div, IntoElement, RenderOnce, Stateful, VisualTestContext, Window};

/// What a screen reader would see on one node.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub role: Option<Role>,
    pub label: Option<String>,
    pub value: Option<String>,
    pub live: Option<Live>,
    pub heading_level: Option<usize>,
    pub read_only: bool,
}

fn read(element: &impl gpui::Element) -> Seen {
    let role = element.a11y_role();
    let mut node = Node::new(role.unwrap_or(Role::Unknown));
    element.write_a11y_info(&mut node);

    Seen {
        role,
        label: node.label().map(str::to_owned),
        value: node.value().map(str::to_owned),
        live: node.live(),
        heading_level: node.level(),
        read_only: node.is_read_only(),
    }
}

/// Renders the component and reads the node it puts in the tree.
pub(crate) fn see<C: RenderOnce>(component: C, window: &mut Window, cx: &mut App) -> Seen {
    read(&component.render(window, cx).into_element())
}

/// [`see`] in a test window.
pub(crate) fn see_in<C: RenderOnce>(component: C, cx: &mut VisualTestContext) -> Seen {
    cx.update(|window, cx| see(component, window, cx))
}

/// [`see_in`] for a component that boxes its node, such as an accessible `Label`.
pub(crate) fn see_label_in<C: RenderOnce>(component: C, cx: &mut VisualTestContext) -> Seen {
    cx.update(|window, cx| {
        let mut element = component.render(window, cx).into_any_element();

        match element.downcast_mut::<Stateful<Div>>() {
            Some(node) => read(&*node),
            None => read(&element),
        }
    })
}

/// What a Shift-Tab walk visits when a Tab walk visited `forward`: the same stops, from the
/// same start, the other way round.
pub(crate) fn reversed(forward: &[String]) -> Vec<String> {
    forward
        .iter()
        .take(1)
        .chain(forward.iter().skip(1).rev())
        .cloned()
        .collect()
}

/// Panics when a walk reached a stop with no name, no focus or no drawn element.
pub(crate) fn assert_named(walk: &[String]) {
    for stop in walk {
        assert!(
            !matches!(stop.as_str(), "unnamed" | "nothing" | "not drawn"),
            "{stop} in {walk:?}"
        );
    }
}

/// A region that says nothing: a live region with an empty value.
pub(crate) fn is_quiet(seen: &Seen) -> bool {
    seen.value.as_deref().is_none_or(str::is_empty)
}
