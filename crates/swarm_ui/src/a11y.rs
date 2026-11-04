//! Accessibility flags GPUI has no builder for, and the helper that names a focusable element.

use gpui::{
    A11ySubtreeBuilder, App, Bounds, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton, Pixels, Role,
    SharedString, StatefulInteractiveElement, Window, accesskit,
};

pub use gpui::accesskit::Live;

/// Wraps an element and adds AccessKit flags that GPUI has no builder for.
///
/// It forwards every [`Element`] method to the inner element, so GPUI pushes one node.
#[must_use]
#[expect(clippy::struct_excessive_bools, reason = "one bool per AccessKit flag")]
pub struct A11yFlags<E> {
    element: E,
    disabled: bool,
    busy: bool,
    live: Option<Live>,
    modal: bool,
    read_only: bool,
    hidden: bool,
    invalid: bool,
}

impl<E> A11yFlags<E> {
    fn new(element: E) -> Self {
        Self {
            element,
            disabled: false,
            busy: false,
            live: None,
            modal: false,
            read_only: false,
            hidden: false,
            invalid: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    pub fn live(mut self, live: Live) -> Self {
        self.live = Some(live);
        self
    }

    pub fn modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// The element holds a value the user must fix, like `aria-invalid`.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }
}

/// Adds [`A11yFlags`] to any element.
pub trait WithA11yFlags: Element + Sized {
    fn a11y_flags(self) -> A11yFlags<Self> {
        A11yFlags::new(self)
    }
}

impl<E: Element> WithA11yFlags for E {}

impl<E: Element> IntoElement for A11yFlags<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: Element> Element for A11yFlags<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.element.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.element.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.element.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.element
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }

    fn a11y_role(&self) -> Option<Role> {
        self.element.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.element.write_a11y_info(node);

        if self.disabled {
            node.set_disabled();
        }

        if self.busy {
            node.set_busy();
        }

        if let Some(live) = self.live {
            node.set_live(live);
        }

        if self.modal {
            node.set_modal();
        }

        if self.read_only {
            node.set_read_only();
        }

        if self.hidden {
            node.set_hidden();
        }

        if self.invalid {
            node.set_invalid(accesskit::Invalid::True);
        }
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder,
    ) {
        self.element.a11y_synthetic_children(prepaint, builder);
    }
}

/// Gives a focusable element its role and label. With the `test-support` feature (or in
/// this crate's tests) it also records its name when `NameStop` reaches it, so a Tab walk
/// can tell which stop has focus.
pub fn accessible<E: StatefulInteractiveElement>(
    element: E,
    role: Role,
    label: impl Into<SharedString>,
) -> E {
    let label: SharedString = label.into();

    #[cfg(any(test, feature = "test-support"))]
    let element = {
        let name = format!("{role:?} {label}");

        element.on_action(move |_: &crate::test_support::NameStop, _, cx| {
            cx.set_global(crate::test_support::LastStop(Some(name.clone())));
        })
    };

    element.role(role).aria_label(label)
}

/// Tracks `handle` on `element` without making it a Tab stop, for a disabled control that
/// keeps focus. `handle` itself stays as it was, so the control is a Tab stop again when
/// it is enabled.
pub fn track_focus_off_tab<E: InteractiveElement>(element: E, handle: &FocusHandle) -> E {
    let original = handle.tab_stop;

    // GPUI keeps the flag on the handle's shared record, and `track_focus` copies it from there.
    let off_tab = handle.clone().tab_stop(false);
    let element = element.track_focus(&off_tab);
    drop(handle.clone().tab_stop(original));

    element
}

/// Makes a click on a disabled control leave focus where it is, instead of moving it onto
/// the control.
pub fn ignore_press<E: InteractiveElement>(element: E) -> E {
    element.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

#[cfg(test)]
mod tests {
    use gpui::div;

    use super::*;

    fn invalid_of(invalid: bool) -> Option<accesskit::Invalid> {
        let element = div().a11y_flags().invalid(invalid);
        let mut node = accesskit::Node::new(Role::Unknown);

        element.write_a11y_info(&mut node);

        node.invalid()
    }

    #[test]
    fn an_invalid_element_says_so_to_assistive_technology() {
        assert_eq!(invalid_of(true), Some(accesskit::Invalid::True));
        assert_eq!(invalid_of(false), None);
    }
}
