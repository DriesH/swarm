//! Settings › Prompts: the prompts the agents get and the user's copies of them. The
//! daemon owns the files; this page lists them, asks for a copy, and opens the copy in the
//! editor the system picks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Global, Render, Role, SharedString, WeakEntity,
    Window, div, px,
};
use swarm_protocol::{Client, ClientError, PromptInfo, PromptLayer};
use swarm_ui::prelude::*;

use super::pages::{page_title, plain_row};
use super::{Page, SettingsState, SettingsWindow};

/// Opens a file the way the system does for its type. A test records instead.
trait FileOpener {
    fn open(&self, path: &Path, cx: &mut App);
}

struct SystemOpener;

impl FileOpener for SystemOpener {
    fn open(&self, path: &Path, cx: &mut App) {
        cx.open_with_system(path);
    }
}

struct Opener(Rc<dyn FileOpener>);

impl Global for Opener {}

#[cfg(test)]
fn use_opener(opener: Rc<dyn FileOpener>, cx: &mut App) {
    cx.set_global(Opener(opener));
}

fn opener(cx: &App) -> Rc<dyn FileOpener> {
    cx.try_global::<Opener>().map_or_else(
        || Rc::new(SystemOpener) as Rc<dyn FileOpener>,
        |opener| Rc::clone(&opener.0),
    )
}

/// The prompts of the agents, for a window with no daemon behind it.
fn sample_prompts() -> Vec<PromptInfo> {
    let folder = Path::new("/Users/sam/.config/swarm/prompts");

    ["orchestrator", "worker", "review", "pr", "commit"]
        .into_iter()
        .map(|name| PromptInfo {
            name: name.to_owned(),
            layer: if name == "worker" {
                PromptLayer::User
            } else {
                PromptLayer::BuiltIn
            },
            user_path: folder.join(format!("{name}.md")),
            project_path: None,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// No daemon answered: the list is made up and the buttons do nothing.
    Sample,
    Daemon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Control {
    Customize,
    Open,
    Reset,
}

/// Where keyboard focus goes at the next frame.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Wanted {
    Cancel,
    Control(String, Control),
}

pub(super) struct PromptsPage {
    state: Entity<SettingsState>,
    prompts: Vec<PromptInfo>,
    source: Source,
    /// Goes up with every listing asked for, so an old answer never replaces a newer one.
    revision: u64,
    list_error: Option<SharedString>,
    action_error: Option<SharedString>,
    /// The prompt whose copy the user was asked about.
    asking: Option<String>,
    /// An action is on its way to the daemon.
    busy: bool,
    handles: HashMap<(String, Control), FocusHandle>,
    cancel: FocusHandle,
    confirm: FocusHandle,
    wanted: Option<Wanted>,
    /// Takes focus once the next listing arrives, when the control it names is there.
    wanted_after_listing: Option<Wanted>,
}

impl PromptsPage {
    pub(super) fn new(state: Entity<SettingsState>, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            prompts: sample_prompts(),
            source: Source::Sample,
            revision: 0,
            list_error: None,
            action_error: None,
            asking: None,
            busy: false,
            handles: HashMap::new(),
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
            wanted: None,
            wanted_after_listing: None,
        }
    }

    /// The daemon the workspace is connected to, if any.
    fn client(&self, cx: &App) -> Option<Client> {
        self.state
            .read(cx)
            .workspace
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|workspace| workspace.read(cx).daemon.clone())
            .filter(Client::is_connected)
    }

    /// Reads the list again. Without a daemon the page shows the sample list.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx) else {
            self.use_sample(cx);

            return;
        };

        self.revision += 1;
        let revision = self.revision;

        cx.spawn(async move |this, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { client.prompts(None) })
                .await;

            let shown = this.update(cx, |page, cx| page.listed(revision, listing, cx));

            if let Err(error) = shown {
                log::debug!("the settings closed before the prompts came: {error}");
            }
        })
        .detach();
    }

    fn use_sample(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;

        if self.source != Source::Sample {
            self.prompts = sample_prompts();
            self.source = Source::Sample;
        }

        self.list_error = None;
        self.asking = None;
        cx.notify();
    }

    fn listed(
        &mut self,
        revision: u64,
        listing: Result<Vec<PromptInfo>, ClientError>,
        cx: &mut Context<Self>,
    ) {
        if revision != self.revision {
            return;
        }

        match listing {
            Ok(prompts) => {
                self.prompts = prompts;
                self.source = Source::Daemon;
                self.list_error = None;
                self.wanted = self.wanted_after_listing.take();
            }
            Err(error) => {
                self.list_error = Some(format!("Could not read the prompts: {error}").into());
                self.wanted_after_listing = None;
            }
        }

        cx.notify();
    }

    fn can_act(&self, cx: &App) -> bool {
        !self.busy && self.source == Source::Daemon && self.client(cx).is_some()
    }

    /// Makes the user's copy of a built-in prompt and opens it.
    fn customize(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.can_act(cx) {
            return;
        }
        let Some(client) = self.client(cx) else {
            return;
        };

        self.begin_action(cx);

        let asked = name.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.customize_prompt(asked) })
                .await;

            let shown = this.update(cx, |page, cx| page.customized(name, result, cx));

            if let Err(error) = shown {
                log::debug!("the settings closed before the copy was made: {error}");
            }
        })
        .detach();
    }

    fn customized(
        &mut self,
        name: String,
        result: Result<PathBuf, ClientError>,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;

        match result {
            Ok(path) => {
                opener(cx).open(&path, cx);
                self.wanted_after_listing = Some(Wanted::Control(name, Control::Open));
            }
            Err(error) => {
                self.action_error = Some(format!("Could not customize {name}.md: {error}").into());
                self.wanted_after_listing = Some(Wanted::Control(name, Control::Customize));
            }
        }

        self.refresh(cx);
    }

    /// Opens the user's copy.
    fn open(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.can_act(cx) {
            return;
        }

        let path = self
            .prompts
            .iter()
            .find(|prompt| prompt.name == name && prompt.layer == PromptLayer::User)
            .map(|prompt| prompt.user_path.clone());

        if let Some(path) = path {
            self.action_error = None;
            opener(cx).open(&path, cx);
            cx.notify();
        }
    }

    fn ask_to_reset(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.can_act(cx) {
            return;
        }

        self.asking = Some(name);
        self.action_error = None;
        self.wanted = Some(Wanted::Cancel);
        cx.notify();
    }

    fn cancel_reset(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }

        if let Some(name) = self.asking.take() {
            self.wanted = Some(Wanted::Control(name, Control::Reset));
        }

        cx.notify();
    }

    /// Removes the user's copy, so the built-in prompt applies again.
    fn reset(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.asking.clone() else {
            return;
        };
        if !self.can_act(cx) {
            return;
        }
        let Some(client) = self.client(cx) else {
            return;
        };

        self.begin_action(cx);

        let asked = name.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.reset_prompt(asked) })
                .await;

            let shown = this.update(cx, |page, cx| page.was_reset(name, result, cx));

            if let Err(error) = shown {
                log::debug!("the settings closed before the copy was removed: {error}");
            }
        })
        .detach();
    }

    fn was_reset(&mut self, name: String, result: Result<(), ClientError>, cx: &mut Context<Self>) {
        self.busy = false;
        self.asking = None;

        let control = match result {
            Ok(()) => Control::Customize,
            Err(error) => {
                self.action_error = Some(format!("Could not reset {name}.md: {error}").into());

                Control::Reset
            }
        };
        self.wanted_after_listing = Some(Wanted::Control(name, control));

        self.refresh(cx);
    }

    /// Asks about the copy of `worker`, as a click on its Reset would with a daemon.
    #[cfg(feature = "snapshot")]
    pub(super) fn ask_for_snapshot(&mut self, cx: &mut Context<Self>) {
        self.asking = Some("worker".to_owned());
        cx.notify();
    }

    fn begin_action(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.action_error = None;
        cx.notify();
    }

    fn handle(&mut self, name: &str, control: Control, cx: &mut Context<Self>) -> FocusHandle {
        self.handles
            .entry((name.to_owned(), control))
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    fn apply_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.wanted.take() {
            Some(Wanted::Cancel) => Some(self.cancel.clone()),
            Some(Wanted::Control(name, control)) => self.handles.get(&(name, control)).cloned(),
            None => None,
        };

        if let Some(handle) = handle {
            window.focus(&handle, cx);
        }
    }

    /// What sits at the end of a prompt's row: its buttons, or the question about its copy.
    fn controls(&mut self, prompt: &PromptInfo, cx: &mut Context<Self>) -> AnyElement {
        let name = prompt.name.clone();

        if self.asking.as_ref() == Some(&name) {
            let selector = format!("reset-{name}");

            let confirm = ConfirmRow::new(
                format!("reset-confirm-{name}"),
                format!("Reset {name}.md?"),
                "Reset",
            )
            .destructive(true)
            .busy(self.busy)
            .track_focus(&self.cancel, &self.confirm)
            .debug_selector(move || selector)
            .on_cancel(cx.listener(|page, _, _, cx| page.cancel_reset(cx)))
            .on_confirm(cx.listener(|page, _, _, cx| page.reset(cx)));

            return div().flex_1().min_w_0().child(confirm).into_any_element();
        }

        let button =
            |page: &mut Self, control: Control, label: &'static str, cx: &mut Context<Self>| {
                let handle = page.handle(&name, control, cx);
                let (for_click, selector) =
                    (name.clone(), format!("{label}-{name}").to_lowercase());

                Button::new(selector.clone(), label)
                    .accessible_label(format!("{label} {name}"))
                    .track_focus(&handle)
                    .debug_selector(move || selector)
                    .on_click(cx.listener(move |page, _, _, cx| match control {
                        Control::Customize => page.customize(for_click.clone(), cx),
                        Control::Open => page.open(&for_click, cx),
                        Control::Reset => page.ask_to_reset(for_click.clone(), cx),
                    }))
            };

        let buttons = match prompt.layer {
            PromptLayer::BuiltIn => vec![button(self, Control::Customize, "Customize", cx)],
            PromptLayer::User => vec![
                button(self, Control::Open, "Open", cx),
                button(self, Control::Reset, "Reset", cx),
            ],
            PromptLayer::Project => Vec::new(),
        };

        h_flex().gap(px(8.)).children(buttons).into_any_element()
    }
}

fn layer_tag(name: &str, layer: PromptLayer) -> impl IntoElement + use<> {
    let (label, tone) = match layer {
        PromptLayer::BuiltIn => ("Built in", TagTone::Neutral),
        PromptLayer::User => ("Yours", TagTone::Accent),
        PromptLayer::Project => ("Project", TagTone::Ok),
    };
    let selector = format!("layer-{name}-{}", label.to_lowercase().replace(' ', "-"));

    div()
        .id(format!("layer-{name}"))
        .debug_selector(move || selector)
        .role(Role::Label)
        .aria_label(label)
        .child(Tag::new(label).tone(tone))
}

impl Render for PromptsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompts = self.prompts.clone();

        let mut panel = Panel::new().accessible("prompts", "Prompts");
        for prompt in &prompts {
            let controls = self.controls(prompt, cx);

            let row = plain_row()
                .child(
                    div().flex_1().min_w_0().child(
                        h_flex()
                            .gap(px(8.))
                            .child(Label::new(format!("{}.md", prompt.name)).mono().truncate())
                            .child(layer_tag(&prompt.name, prompt.layer)),
                    ),
                )
                .child(controls);

            panel = panel.row(row);
        }

        self.apply_focus(window, cx);

        let error = self
            .action_error
            .clone()
            .or_else(|| self.list_error.clone())
            .map(|message| {
                div()
                    .id("prompts-error")
                    .debug_selector(|| "prompts-error".to_owned())
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .text_size(px(12.))
                    .text_color(Color::Del.hsla(cx))
                    .child(message)
            });

        v_flex()
            .gap(px(26.))
            .child(page_title("Prompts"))
            .children(error)
            .child(panel)
    }
}

impl SettingsWindow {
    /// Reads the prompts again, while the Prompts page shows.
    pub(super) fn refresh_prompts(&self, cx: &mut Context<Self>) {
        if self.page == Page::Prompts {
            self.prompts.update(cx, PromptsPage::refresh);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle, px, size};
    use swarm_protocol::{Event, Request};

    use super::*;
    use crate::actions::OpenSettings;
    use crate::daemon::tests::{fake_daemon_replying, redraw, setup, wait_until};

    const FOLDER: &str = "/home/sam/.config/swarm/prompts";

    fn prompt(name: &str, layer: PromptLayer) -> PromptInfo {
        PromptInfo {
            name: name.to_owned(),
            layer,
            user_path: Path::new(FOLDER).join(format!("{name}.md")),
            project_path: None,
        }
    }

    /// What the fake daemon knows and has been asked.
    #[derive(Default)]
    struct Daemon {
        prompts: Vec<PromptInfo>,
        requests: Vec<Request>,
        refuse_customize: bool,
    }

    type Shared = Arc<Mutex<Daemon>>;

    fn shared(prompts: Vec<PromptInfo>) -> Shared {
        Arc::new(Mutex::new(Daemon {
            prompts,
            ..Daemon::default()
        }))
    }

    fn asked(shared: &Shared) -> Vec<Request> {
        shared
            .lock()
            .expect("lock the fake daemon")
            .requests
            .clone()
    }

    fn count(shared: &Shared, wanted: impl Fn(&Request) -> bool) -> usize {
        asked(shared)
            .iter()
            .filter(|request| wanted(request))
            .count()
    }

    /// A daemon that lists prompts, makes copies and removes them, like the real one.
    fn fake_daemon(shared: &Shared) -> PathBuf {
        let shared = Arc::clone(shared);

        fake_daemon_replying(move |request| {
            let mut daemon = shared.lock().expect("lock the fake daemon");
            daemon.requests.push(request.clone());

            match request {
                Request::Prompts { call, .. } => Some(Event::Prompts {
                    call,
                    prompts: daemon.prompts.clone(),
                }),
                Request::CustomizePrompt { call, name } => {
                    if daemon.refuse_customize {
                        return Some(Event::Error {
                            call: Some(call),
                            id: None,
                            message: "the prompts folder is read-only".into(),
                        });
                    }

                    let path = Path::new(FOLDER).join(format!("{name}.md"));
                    if let Some(prompt) = daemon.prompts.iter_mut().find(|p| p.name == name) {
                        prompt.layer = PromptLayer::User;
                    }

                    Some(Event::PromptFile { call, path })
                }
                Request::ResetPrompt { call, name } => {
                    if let Some(prompt) = daemon.prompts.iter_mut().find(|p| p.name == name) {
                        prompt.layer = PromptLayer::BuiltIn;
                    }

                    Some(Event::Done { call })
                }
                _ => None,
            }
        })
    }

    #[derive(Default)]
    struct Opened(RefCell<Vec<PathBuf>>);

    impl FileOpener for Opened {
        fn open(&self, path: &Path, _cx: &mut App) {
            self.0.borrow_mut().push(path.to_owned());
        }
    }

    struct Fixture {
        settings: VisualTestContext,
        window: WindowHandle<SettingsWindow>,
        opened: Rc<Opened>,
    }

    impl Fixture {
        fn opened(&self) -> Vec<PathBuf> {
            self.opened.0.borrow().clone()
        }

        fn shown(&mut self, selector: &'static str) -> bool {
            redraw(&mut self.settings);

            self.settings.debug_bounds(selector).is_some()
        }

        /// Waits until the page draws `selector`, as the answer of the daemon arrives on
        /// another thread.
        fn wait_for(&mut self, selector: &'static str) {
            let seen = wait_until(&mut self.settings, |cx| {
                redraw(cx);

                cx.debug_bounds(selector).is_some()
            });

            assert!(seen, "{selector} never showed");
        }

        fn click(&mut self, selector: &'static str) {
            self.wait_for(selector);
            let bounds = self
                .settings
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not on screen"));

            self.settings
                .simulate_click(bounds.center(), Modifiers::none());
            self.settings.run_until_parked();
        }

        fn come_back(&mut self) {
            self.settings.deactivate_window();
            self.settings.update(|window, _| window.activate_window());
            self.settings.run_until_parked();
        }
    }

    /// The Prompts page of a settings window opened from a workspace. With `daemon` the
    /// workspace is connected to a fake daemon that knows these prompts.
    fn open_page(cx: &mut TestAppContext, daemon: Option<&Shared>) -> Fixture {
        let opened = Rc::new(Opened::default());
        let recorder: Rc<dyn FileOpener> = Rc::clone(&opened) as Rc<dyn FileOpener>;
        cx.update(|cx| use_opener(recorder, cx));

        let (workspace, main) = setup(cx);
        if let Some(daemon) = daemon {
            main.executor().allow_parking();
            let client = Client::connect(&fake_daemon(daemon)).expect("connect to the fake daemon");
            workspace.update(main, |workspace, _| workspace.daemon = Some(client));
        }

        main.dispatch_action(OpenSettings);
        let window = main
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .expect("the settings window opened");
        window
            .update(main, |settings, _, cx| {
                settings.select_page(Page::Prompts, cx);
            })
            .expect("the window is open");

        let settings = VisualTestContext::from_window(window.into(), main);
        settings.simulate_resize(size(px(1280.), px(1000.)));
        settings.executor().allow_parking();
        settings.run_until_parked();

        Fixture {
            settings,
            window,
            opened,
        }
    }

    fn three() -> Shared {
        shared(vec![
            prompt("orchestrator", PromptLayer::BuiltIn),
            prompt("worker", PromptLayer::User),
            prompt("review", PromptLayer::BuiltIn),
        ])
    }

    #[gpui::test]
    fn every_prompt_shows_with_its_layer_and_its_actions(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));

        page.wait_for("layer-review-built-in");

        for shown in [
            "layer-orchestrator-built-in",
            "layer-worker-yours",
            "layer-review-built-in",
            "customize-orchestrator",
            "customize-review",
            "open-worker",
            "reset-worker",
        ] {
            assert!(page.shown(shown), "{shown} is missing");
        }
        for hidden in [
            "customize-worker",
            "open-orchestrator",
            "reset-orchestrator",
            "layer-worker-built-in",
        ] {
            assert!(!page.shown(hidden), "{hidden} should not show");
        }
        assert_eq!(
            count(&daemon, |r| matches!(
                r,
                Request::Prompts { project: None, .. }
            )),
            1
        );
    }

    #[gpui::test]
    fn customize_asks_for_a_copy_opens_the_returned_file_and_reads_the_list_again(
        cx: &mut TestAppContext,
    ) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));

        page.click("customize-orchestrator");
        page.wait_for("open-orchestrator");

        let customized = count(
            &daemon,
            |request| matches!(request, Request::CustomizePrompt { name, .. } if name == "orchestrator"),
        );
        assert_eq!(customized, 1);
        assert_eq!(page.opened(), [Path::new(FOLDER).join("orchestrator.md")]);
        assert!(page.shown("layer-orchestrator-yours"));
        assert!(!page.shown("customize-orchestrator"));
        assert_eq!(count(&daemon, |r| matches!(r, Request::Prompts { .. })), 2);
    }

    #[gpui::test]
    fn open_opens_the_copy_and_asks_the_daemon_nothing(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));

        page.click("open-worker");

        assert_eq!(page.opened(), [Path::new(FOLDER).join("worker.md")]);
        assert_eq!(asked(&daemon).len(), 1);
    }

    #[gpui::test]
    fn reset_asks_in_place_and_calls_the_daemon_once_after_the_answer(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));

        page.click("reset-worker");
        let asking = page.shown("reset-worker-confirm");
        let asked_before = count(&daemon, |r| matches!(r, Request::ResetPrompt { .. }));
        let buttons_gone = !page.shown("open-worker");

        page.click("reset-worker-confirm");
        page.wait_for("customize-worker");

        let resets = count(
            &daemon,
            |request| matches!(request, Request::ResetPrompt { name, .. } if name == "worker"),
        );
        assert!(asking, "no question was asked");
        assert!(buttons_gone, "the buttons stayed beside the question");
        assert_eq!(asked_before, 0);
        assert_eq!(resets, 1);
        assert!(!page.shown("reset-worker-confirm"));
        assert!(!page.shown("open-worker"));
        assert!(page.opened().is_empty());
    }

    #[gpui::test]
    fn cancelling_the_question_keeps_the_copy(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));

        page.click("reset-worker");
        page.click("reset-worker-cancel");

        assert!(!page.shown("reset-worker-confirm"));
        assert!(page.shown("open-worker"));
        assert_eq!(
            count(&daemon, |r| matches!(r, Request::ResetPrompt { .. })),
            0
        );
    }

    #[gpui::test]
    fn the_list_is_read_again_when_the_window_gets_focus(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));
        page.wait_for("open-worker");

        // The user deletes their copy in an editor.
        daemon
            .lock()
            .expect("lock the fake daemon")
            .prompts
            .iter_mut()
            .for_each(|prompt| prompt.layer = PromptLayer::BuiltIn);
        page.come_back();

        page.wait_for("customize-worker");
        assert!(!page.shown("open-worker"));
        assert_eq!(count(&daemon, |r| matches!(r, Request::Prompts { .. })), 2);
    }

    #[gpui::test]
    fn the_list_is_read_again_each_time_the_page_is_selected(cx: &mut TestAppContext) {
        let daemon = three();
        let mut page = open_page(cx, Some(&daemon));
        page.wait_for("open-worker");

        page.window
            .update(&mut page.settings, |settings, _, cx| {
                settings.select_page(Page::Agents, cx);
                settings.select_page(Page::Prompts, cx);
            })
            .expect("the window is open");
        page.settings.run_until_parked();

        let wanted = wait_until(&mut page.settings, |_| {
            count(&daemon, |r| matches!(r, Request::Prompts { .. })) == 2
        });
        assert!(wanted);
    }

    #[gpui::test]
    fn a_refused_copy_shows_the_error_inline_and_opens_nothing(cx: &mut TestAppContext) {
        let daemon = three();
        daemon
            .lock()
            .expect("lock the fake daemon")
            .refuse_customize = true;
        let mut page = open_page(cx, Some(&daemon));

        page.click("customize-orchestrator");
        page.wait_for("prompts-error");

        assert!(page.opened().is_empty());
        assert!(page.shown("customize-orchestrator"));
    }

    #[gpui::test]
    fn without_a_daemon_the_sample_list_shows_and_clicks_do_nothing(cx: &mut TestAppContext) {
        let mut page = open_page(cx, None);

        page.wait_for("customize-orchestrator");
        assert!(page.shown("layer-worker-yours"));
        page.click("customize-orchestrator");
        page.click("open-worker");
        page.click("reset-worker");

        assert!(page.opened().is_empty());
        assert!(!page.shown("reset-worker-confirm"));
        assert!(page.shown("customize-orchestrator"));
    }
}
