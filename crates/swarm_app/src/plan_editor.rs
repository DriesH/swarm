//! The plan editor on the Start tab: the user writes a plan, picks the orchestrator and
//! sends it. Only then does the Start orchestrator start. The daemon keeps the plan draft.

use std::convert::Infallible;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, Role, SharedString,
    Subscription, Task, WeakEntity, Window, div, px,
};
use swarm_protocol::{AgentInfo, Client, ClientError, PlanDraft, ProjectId as DaemonProjectId};
use swarm_ui::prelude::*;

use crate::daemon::NOT_RUNNING;
use crate::model::{AgentKind, ProjectId, SAMPLE_PLAN, SAMPLE_PLAN_TEMPLATE};
use crate::new_task::ORCHESTRATORS;
use crate::workspace::{View, Workspace};

#[cfg(test)]
mod tests;

/// How long the draft waits after the last edit before it is saved.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// The widest the editor column grows.
const MAX_WIDTH: f32 = 760.;
/// The corner radius of the text box, as in a bordered text field.
const BOX_RADIUS: f32 = 8.;
/// The space between the box and the text, as in a bordered text field.
const BOX_PADDING: f32 = 10.;

/// What a call to the daemon needs.
struct Link {
    client: Client,
    project: DaemonProjectId,
}

/// A save on its way to the daemon.
struct Save {
    /// What the daemon holds once the call is done.
    kept: Option<String>,
    call: Task<Result<(), String>>,
}

/// A channel that never gets a value. It closes when the call that holds its sender ends.
type Ended = flume::Receiver<Infallible>;

/// Whether the plan draft came from the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    Loading,
    /// The daemon did not give the draft. The text stays locked until a retry works.
    Failed,
    Loaded,
}

/// Whether the plan editor of the open Start tab takes the focus once its draft is there.
#[derive(Debug, Default)]
pub(crate) enum PlanFocus {
    #[default]
    Off,
    /// The tab asked for it. The next draw notes what holds the focus.
    Asked,
    /// Waits for the draft. The user chose another element when the focus is not on the
    /// one noted.
    Waiting(Option<FocusHandle>),
}

/// The plan of one project, from a blank template to the moment it is sent.
pub(crate) struct PlanEditor {
    workspace: WeakEntity<Workspace>,
    project: ProjectId,
    /// False for the fake sample data: there is no daemon to load from, save to or send to.
    live: bool,
    text: Entity<TextArea>,
    orchestrator: usize,
    template: String,
    load: Load,
    sending: bool,
    /// Why the draft did not load or the plan did not go out.
    error: Option<SharedString>,
    /// Why the last save failed.
    save_error: Option<SharedString>,
    /// The plan draft the daemon holds, as far as the app knows.
    saved: Option<String>,
    /// Waits for the edits to stop. Each edit replaces it.
    save_timer: Option<Task<()>>,
    /// The last task that works on the daemon's answer. The next one waits for it, so they
    /// never swap places.
    work: Option<Task<()>>,
    /// Closes when the last save call to the daemon ends. The next save, and the save on
    /// quit, wait for it. The quit cannot wait for `work`: that needs the main thread.
    saving: Option<Ended>,
    send_focus: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl PlanEditor {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        project: ProjectId,
        live: bool,
        default_kind: AgentKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Plan", cx);
            area.set_mono(true, cx);
            area.set_bordered(false, cx);
            area.set_fill(true, cx);
            area.set_highlighter(Some(markdown_highlights), cx);
            area
        });

        let on_text = cx.subscribe(&text, |this, _, event, cx| match event {
            TextInputEvent::Changed(_) => this.edited(cx),
            TextInputEvent::Submitted(_) => this.send(cx),
        });
        let on_quit = cx.on_app_quit(Self::save_on_quit);

        let mut editor = Self {
            workspace,
            project,
            live,
            text,
            orchestrator: ORCHESTRATORS
                .iter()
                .position(|(_, kind)| *kind == default_kind)
                .unwrap_or(0),
            template: if live {
                String::new()
            } else {
                SAMPLE_PLAN_TEMPLATE.to_owned()
            },
            load: if live { Load::Loading } else { Load::Loaded },
            sending: false,
            error: None,
            save_error: None,
            saved: None,
            save_timer: None,
            work: None,
            saving: None,
            send_focus: cx.focus_handle().tab_stop(true),
            _subscriptions: [on_text, on_quit],
        };

        if live {
            editor
                .text
                .update(cx, |area, cx| area.set_disabled(true, cx));
            editor.load(cx);
        } else {
            editor
                .text
                .update(cx, |area, cx| area.set_text_at_start(SAMPLE_PLAN, cx));
        }

        editor
    }

    /// True until the daemon has answered with the draft. A disabled text takes no focus.
    pub(crate) fn is_loading(&self) -> bool {
        self.load == Load::Loading
    }

    pub(crate) fn text_focus(&self, cx: &App) -> FocusHandle {
        self.text.focus_handle(cx)
    }

    fn link(&self, cx: &App) -> Option<Link> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let client = workspace.daemon.clone().filter(Client::is_connected)?;
        let project = workspace.data.project(self.project)?.daemon?;

        Some(Link { client, project })
    }

    fn is_being_removed(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).removing_projects.contains(&self.project))
    }

    /// True while the project's Start orchestrator runs: the daemon owns the plan by then.
    fn start_runs(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).data.start_runs(self.project))
    }

    /// What the daemon should keep: the text, unless it is blank or still the template.
    fn draft(&self, cx: &App) -> Option<String> {
        let text = self.text.read(cx).text();
        let trimmed = text.trim();

        (!trimmed.is_empty() && trimmed != self.template.trim()).then(|| text.to_string())
    }

    /// The plan that Send plan would send now.
    fn sendable(&self, cx: &App) -> Option<String> {
        if !self.live || self.load != Load::Loaded || self.sending {
            return None;
        }

        self.draft(cx)
    }

    /// Asks the daemon for the draft.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.work = Some(cx.spawn(async move |this, cx| {
            let link = this.update(cx, |editor, cx| editor.link(cx));

            let loaded = match link {
                Ok(Some(link)) => {
                    let loading = cx
                        .background_executor()
                        .spawn(async move { link.client.plan_draft(link.project) });

                    loading.await.map_err(|error| error.to_string())
                }
                Ok(None) => Err(NOT_RUNNING.to_owned()),
                Err(error) => {
                    log::debug!("the plan editor is gone before the draft came: {error}");
                    return;
                }
            };

            if let Err(error) = this.update(cx, |editor, cx| editor.loaded(loaded, cx)) {
                log::debug!("the plan editor is gone before the draft came: {error}");
            }
        }));
    }

    /// A daemon link is back: asks again when the draft did not load.
    fn retry_load(&mut self, cx: &mut Context<Self>) {
        if self.load != Load::Failed || self.sending {
            return;
        }

        self.load = Load::Loading;
        self.text.update(cx, |area, cx| area.set_disabled(true, cx));
        self.load(cx);

        cx.notify();
    }

    /// A failed load keeps the text locked, so nothing typed can replace a draft that
    /// did not show.
    fn loaded(&mut self, loaded: Result<PlanDraft, String>, cx: &mut Context<Self>) {
        match loaded {
            Ok(draft) => {
                self.load = Load::Loaded;
                self.error = None;
                self.template = draft.template;

                match draft.text.filter(|text| !text.trim().is_empty()) {
                    Some(text) => {
                        self.text
                            .update(cx, |area, cx| area.set_text(text.clone(), cx));
                        self.saved = Some(text);
                    }
                    None => self.show_template(cx),
                }

                self.text
                    .update(cx, |area, cx| area.set_disabled(false, cx));
            }
            Err(message) => {
                log::warn!("cannot load the plan draft: {message}");
                self.load = Load::Failed;
                self.error = Some(message.into());
            }
        }

        cx.notify();
    }

    /// What the error line shows: the load or send problem, else the last save problem.
    fn shown_error(&self) -> Option<&SharedString> {
        self.error.as_ref().or(self.save_error.as_ref())
    }

    /// Whether the text may go to the daemon as a draft.
    fn may_save(&self, cx: &App) -> bool {
        // The daemon clears the draft when the plan starts. A save now would bring it back.
        self.live && self.load == Load::Loaded && !self.sending && !self.start_runs(cx)
    }

    /// Shows the template with the cursor where the user starts to write.
    fn show_template(&mut self, cx: &mut Context<Self>) {
        let template = self.template.clone();
        let cursor = after_first_heading(&template);

        self.text.update(cx, |area, cx| {
            area.set_text(template, cx);
            area.set_cursor(cursor, cx);
        });
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.schedule_save(cx);

        cx.notify();
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if !self.live || self.load != Load::Loaded {
            return;
        }

        self.save_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;

            if let Err(error) = this.update(cx, Self::save) {
                log::debug!("the plan editor is gone before its draft was saved: {error}");
            }
        }));
    }

    /// Starts the call that stores the text, when the daemon does not hold it yet. The call
    /// waits for the save before it, so they never swap places.
    fn start_save(&mut self, cx: &mut Context<Self>) -> Option<Save> {
        if !self.may_save(cx) {
            return None;
        }

        let draft = self.draft(cx);
        if draft == self.saved {
            return None;
        }
        let Some(link) = self.link(cx) else {
            log::debug!("the daemon is not connected, the plan draft stays unsaved");
            return None;
        };

        let kept = draft.clone();
        let previous = self.saving.take();
        let (closing, ended) = flume::bounded::<Infallible>(0);
        self.saving = Some(ended);

        let call = cx.background_executor().spawn(async move {
            // Dropped when the call ends: that tells the next save.
            let _closing = closing;

            if let Some(previous) = previous {
                while previous.recv_async().await.is_ok() {}
            }

            link.client
                .save_plan_draft(link.project, draft)
                .map_err(|error| error.to_string())
        });

        Some(Save { kept, call })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(Save { kept, call }) = self.start_save(cx) else {
            return;
        };
        let previous = self.work.take();

        self.work = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }

            let outcome = call.await;

            if let Err(error) = this.update(cx, |editor, cx| editor.saved(kept, outcome, cx)) {
                log::debug!("the plan editor is gone after it saved: {error}");
            }
        }));
    }

    fn saved(&mut self, kept: Option<String>, outcome: Result<(), String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(()) => {
                self.saved = kept;
                self.save_error = None;
            }
            Err(message) => {
                log::warn!("cannot save the plan draft: {message}");
                self.save_error = Some(message.into());
            }
        }

        cx.notify();
    }

    /// Saves a change that waits for its timer. The quit waits for the save. It cannot wait
    /// for tasks of the main thread: that thread waits for the quit.
    fn save_on_quit(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.save_timer = None;

        let Some(Save { call, .. }) = self.start_save(cx) else {
            return Task::ready(());
        };

        cx.background_spawn(async move {
            if let Err(message) = call.await {
                log::warn!("cannot save the plan draft on quit: {message}");
            }
        })
    }

    /// Sends the plan: the Start orchestrator starts with it as its first message.
    pub(crate) fn send(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.sendable(cx) else {
            return;
        };
        if self.is_being_removed(cx) {
            return;
        }
        let Some(link) = self.link(cx) else {
            self.error = Some(NOT_RUNNING.into());
            cx.notify();
            return;
        };

        let kind = ORCHESTRATORS
            .get(self.orchestrator)
            .map_or(AgentKind::ClaudeCode, |(_, kind)| *kind);
        let workspace = self.workspace.clone();
        let previous = self.work.take();

        // A save that is still to come would bring the plan back after the daemon cleared it.
        self.save_timer = None;
        self.sending = true;
        self.error = None;
        self.save_error = None;
        self.text.update(cx, |area, cx| area.set_disabled(true, cx));

        self.work = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }

            let starting = cx.background_executor().spawn(async move {
                link.client
                    .start_orchestrator(link.project, Some(kind.into()), Some(plan))
            });

            let outcome = match starting.await {
                Ok(agent) => {
                    let shown =
                        Workspace::update_with_window(&workspace, cx, |workspace, window, cx| {
                            workspace.plan_sent(&agent, window, cx);
                        });

                    if let Err(error) = shown {
                        log::debug!("the workspace is gone before the plan was shown: {error}");
                    }

                    Ok(())
                }
                Err(error) => Err(error),
            };

            if let Err(error) = this.update(cx, |editor, cx| editor.sent(outcome, cx)) {
                log::debug!("the plan editor is gone after it sent the plan: {error}");
            }
        }));

        cx.notify();
    }

    fn sent(&mut self, outcome: Result<(), ClientError>, cx: &mut Context<Self>) {
        self.sending = false;

        match outcome {
            Ok(()) => self.forget(cx),
            Err(error) => {
                log::warn!("cannot send the plan: {error}");
                self.error = Some(error.to_string().into());
                self.text
                    .update(cx, |area, cx| area.set_disabled(false, cx));

                // After a refusal the plan is still only a draft. After a timeout or a lost
                // link the daemon may have started: a save would bring the plan back.
                if matches!(error, ClientError::Daemon(_)) {
                    self.schedule_save(cx);
                }
            }
        }

        cx.notify();
    }

    /// The daemon started the orchestrator for this plan. The workspace drops the editor: the
    /// next one loads the draft again. That is the template after a good start, and the plan
    /// when the start failed after the daemon answered.
    fn forget(&self, cx: &mut Context<Self>) {
        let project = self.project;
        let forgotten = self.workspace.update(cx, |workspace, _| {
            workspace.plan_editors.remove(&project);
        });

        if let Err(error) = forgotten {
            log::debug!("the workspace is gone before the plan editor was dropped: {error}");
        }
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.shown_error().map(SharedString::as_str)
    }

    #[cfg(test)]
    pub(crate) fn choose_orchestrator(&mut self, kind: AgentKind) {
        if let Some(index) = ORCHESTRATORS.iter().position(|(_, other)| *other == kind) {
            self.orchestrator = index;
        }
    }

    #[cfg(test)]
    pub(crate) fn orchestrator_kind(&self) -> AgentKind {
        ORCHESTRATORS[self.orchestrator].1
    }

    #[cfg(test)]
    pub(crate) fn is_sending(&self) -> bool {
        self.sending
    }

    #[cfg(test)]
    pub(crate) fn can_send(&self, cx: &App) -> bool {
        self.sendable(cx).is_some()
    }

    #[cfg(test)]
    pub(crate) fn text_area(&self) -> &Entity<TextArea> {
        &self.text
    }

    /// Shows the Send plan button as it looks while the plan goes out.
    #[cfg(feature = "snapshot")]
    pub(crate) fn show_sending(&mut self, cx: &mut Context<Self>) {
        self.sending = true;
        self.text.update(cx, |area, cx| area.set_disabled(true, cx));
        cx.notify();
    }

    /// Shows the error line as it looks when the daemon refuses.
    #[cfg(feature = "snapshot")]
    pub(crate) fn show_error(&mut self, message: &str, cx: &mut Context<Self>) {
        self.error = Some(message.to_owned().into());
        cx.notify();
    }
}

/// The offset where the line after the first heading line starts: where the user begins to
/// write. A template without a heading line starts at its top.
fn after_first_heading(template: &str) -> usize {
    let mut offset = 0;

    for line in template.split_inclusive('\n') {
        offset += line.len();

        if line.starts_with('#') {
            return offset;
        }
    }

    0
}

impl Render for PlanEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        let colors = cx.theme().colors();
        let (line, accent) = (colors.line, colors.accent);

        let orchestrator = Dropdown::new(
            "plan-orchestrator",
            "Orchestrator",
            ORCHESTRATORS
                .iter()
                .map(|(label, _)| SharedString::new_static(label))
                .collect(),
            self.orchestrator,
        )
        .debug_selector(|| "plan-orchestrator".to_owned())
        .disabled(self.sending)
        .on_change(move |index, _, cx| {
            editor.update(cx, |editor, cx| {
                editor.orchestrator = index;
                cx.notify();
            });
        });

        let send_label = if self.sending {
            "Sending\u{2026}"
        } else {
            "Send plan"
        };
        let send = Button::new("plan-send", send_label)
            .variant(ButtonVariant::Primary)
            .track_focus(&self.send_focus)
            .loading(self.sending)
            .disabled(!self.sending && self.sendable(cx).is_none())
            .debug_selector(|| "plan-send".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.send(cx)));

        let focused = self.text.focus_handle(cx).contains_focused(window, cx);
        let text_box = div()
            .flex_1()
            .min_h_0()
            .px(px(BOX_PADDING))
            .rounded(px(BOX_RADIUS))
            .border_1()
            .border_color(if focused { accent } else { line })
            .debug_selector(|| "plan-text".to_owned())
            .child(self.text.clone());

        let error = self.shown_error().cloned().map(|message| {
            div()
                .id("plan-error")
                .debug_selector(|| "plan-error".to_owned())
                .role(Role::Alert)
                .aria_label(message.clone())
                .text_size(px(12.))
                .text_color(Color::Del.hsla(cx))
                .child(message)
        });

        let footer = h_flex()
            .justify_end()
            .items_center()
            .gap(px(8.))
            .child(orchestrator)
            .child(send);

        v_flex()
            .id("plan-editor")
            .debug_selector(|| "plan-editor".to_owned())
            .size_full()
            .items_center()
            .px(px(24.))
            .py(px(20.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(MAX_WIDTH))
                    .flex_1()
                    .min_h_0()
                    .gap(px(12.))
                    .child(text_box)
                    .children(error)
                    .child(footer),
            )
    }
}

impl Workspace {
    /// The project's plan editor, made when it is first needed.
    pub(crate) fn plan_editor(
        &mut self,
        project: ProjectId,
        cx: &mut Context<Self>,
    ) -> Entity<PlanEditor> {
        if let Some(editor) = self.plan_editors.get(&project) {
            return editor.clone();
        }

        let live = self
            .data
            .project(project)
            .is_some_and(|entry| entry.daemon.is_some());
        let default_kind = self.default_orchestrator();
        let workspace = cx.weak_entity();
        let editor = cx.new(|cx| PlanEditor::new(workspace, project, live, default_kind, cx));

        self.plan_editors.insert(project, editor.clone());

        editor
    }

    /// The editor the open tab shows: a Start or Plan tab whose orchestrator does not run.
    pub(crate) fn shown_plan_editor(&self) -> Option<Entity<PlanEditor>> {
        let task = self.active_task();

        if !task.is_start || self.data.start_runs(task.project) {
            return None;
        }

        self.plan_editors.get(&task.project).cloned()
    }

    /// Makes the editor of the open Start tab before it draws, and moves the focus into it
    /// once the tab asked for it and the draft is there. Focus the user moved elsewhere
    /// meanwhile stays where it is.
    pub(crate) fn sync_plan_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.active_task();

        if self.view != View::Terminals || !task.is_start || self.data.start_runs(task.project) {
            self.plan_focus = PlanFocus::Off;
            return;
        }

        let editor = self.plan_editor(task.project, cx);
        let focused = window.focused(cx);

        let first = match std::mem::take(&mut self.plan_focus) {
            PlanFocus::Off => return,
            PlanFocus::Asked => focused.clone(),
            PlanFocus::Waiting(first) => first,
        };

        // A dialog keeps the request until it closes.
        if self.overlay.is_some() {
            self.plan_focus = PlanFocus::Asked;
            return;
        }
        if first != focused {
            return;
        }
        // A disabled text cannot take the focus.
        if editor.read(cx).is_loading() {
            self.plan_focus = PlanFocus::Waiting(first);
            return;
        }

        let text = editor.read(cx).text_focus(cx);

        window.focus(&text, cx);
    }

    /// A daemon link is back: the editors whose draft did not load ask again.
    pub(crate) fn retry_plan_loads(&mut self, cx: &mut Context<Self>) {
        for editor in self.plan_editors.values() {
            editor.update(cx, PlanEditor::retry_load);
        }
    }

    /// The daemon started the Start orchestrator for a plan: its terminal replaces the editor
    /// and takes the focus.
    pub(crate) fn plan_sent(
        &mut self,
        agent: &AgentInfo,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.agent_changed(agent, window, cx);

        let terminal = self
            .data
            .agent_with_daemon_id(agent.id)
            .filter(|known| known.task == self.active_task)
            .map(|known| known.terminal);

        if let Some(terminal) = terminal {
            self.focus_terminal(terminal, cx);
        }
    }
}
