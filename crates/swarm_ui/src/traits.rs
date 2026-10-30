use gpui::{App, ClickEvent, Window};

pub type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub trait Clickable {
    #[must_use]
    fn on_click(self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self;
}

pub trait Disableable {
    #[must_use]
    fn disabled(self, disabled: bool) -> Self;
}

/// On or selected state: a switch that is on, a selected tab or row.
pub trait Toggleable {
    #[must_use]
    fn toggle_state(self, toggled: bool) -> Self;
}
