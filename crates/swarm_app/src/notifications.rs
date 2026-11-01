//! System notifications. The daemon says when a task needs the user (a `Notice`); this module
//! decides whether to show one, shows it, and routes the click back to the task.
//!
//! It works with no window open: the notices arrive on the daemon connection, which outlives
//! the window. Everything that reaches outside the app sits behind [`Delivery`], so tests
//! record what would have been shown.

#[cfg(target_os = "macos")]
mod appkit;

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    App, Context, Entity, Global, SharedString, SystemNotification, SystemNotificationAction,
    SystemNotificationResponse, Window,
};
use swarm_protocol::{Notice, NoticeKind};
use swarm_settings::{NotificationSettings, ProjectMode};

use crate::lifecycle;
use crate::workspace::{View, Workspace};

/// The Notifications pane of System Settings.
const NOTIFICATIONS_PANE: &str =
    "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

const OPEN_ACTION: &str = "open";
const TEST_TAG: &str = "test";

/// What macOS lets the app do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Permission {
    Allowed,
    NotAllowed,
    /// macOS did not ask the user yet.
    NotAsked,
    /// The app does not run from an app bundle, so macOS has no answer.
    Unavailable,
}

impl Permission {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Allowed => "Allowed",
            Self::NotAllowed => "Not allowed",
            Self::NotAsked => "Not asked",
            Self::Unavailable => "Unavailable",
        }
    }
}

/// What macOS last said about the app's notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub permission: Permission,
    /// The app's own "play sound" setting in System Settings is on.
    pub sound: bool,
}

impl Authorization {
    pub(crate) const UNAVAILABLE: Self = Self {
        permission: Permission::Unavailable,
        sound: false,
    };

    /// macOS shows the notifications and lets them play a sound.
    fn allows_sound(self) -> bool {
        self.permission == Permission::Allowed && self.sound
    }
}

/// Everything a notification needs from outside the app.
pub(crate) trait Delivery {
    fn post(&self, notification: SystemNotification, cx: &App);

    fn play_sound(&self);

    /// Bounces the Dock icon. The caller has checked that the window is not key.
    fn bounce(&self, window: &Window);

    /// Asks macOS for the authorization. The answer may come from any thread.
    fn read_permission(&self, answer: flume::Sender<Authorization>);
}

/// The real thing: GPUI posts, `AppKit` plays and reads.
struct SystemDelivery;

impl Delivery for SystemDelivery {
    fn post(&self, notification: SystemNotification, cx: &App) {
        cx.show_system_notification(notification);
    }

    fn play_sound(&self) {
        #[cfg(target_os = "macos")]
        appkit::play_sound();
    }

    fn bounce(&self, window: &Window) {
        window.request_attention();
    }

    fn read_permission(&self, answer: flume::Sender<Authorization>) {
        #[cfg(target_os = "macos")]
        appkit::read_permission(answer);

        #[cfg(not(target_os = "macos"))]
        if answer.send(Authorization::UNAVAILABLE).is_err() {
            log::debug!("nobody waits for the notification permission");
        }
    }
}

/// A delivery for snapshots: it shows nothing and gives one answer for the permission.
#[cfg(feature = "snapshot")]
pub(crate) struct SampleDelivery(pub Permission);

#[cfg(feature = "snapshot")]
impl Delivery for SampleDelivery {
    fn post(&self, _notification: SystemNotification, _cx: &App) {}

    fn play_sound(&self) {}

    fn bounce(&self, _window: &Window) {}

    fn read_permission(&self, answer: flume::Sender<Authorization>) {
        let authorization = Authorization {
            permission: self.0,
            sound: true,
        };

        if answer.send(authorization).is_err() {
            log::debug!("nobody waits for the notification permission");
        }
    }
}

/// How the app reaches macOS. The system's, unless a test or a snapshot set another.
struct Outside {
    delivery: Rc<dyn Delivery>,
    /// The last answer of macOS, which is none until the first one comes.
    authorization: Option<Authorization>,
}

impl Global for Outside {}

pub(crate) fn use_delivery(delivery: Rc<dyn Delivery>, cx: &mut App) {
    cx.set_global(Outside {
        delivery,
        authorization: None,
    });
}

fn delivery(cx: &App) -> Rc<dyn Delivery> {
    match cx.try_global::<Outside>() {
        Some(outside) => Rc::clone(&outside.delivery),
        None => Rc::new(SystemDelivery),
    }
}

/// macOS shows the notifications and lets them play a sound. An answer that did not come
/// yet counts as no.
fn sound_is_allowed(cx: &App) -> bool {
    cx.try_global::<Outside>()
        .and_then(|outside| outside.authorization)
        .is_some_and(Authorization::allows_sound)
}

/// Keeps the answer of macOS for the sound.
pub(crate) fn remember(authorization: Authorization, cx: &mut App) {
    if cx.has_global::<Outside>() {
        cx.global_mut::<Outside>().authorization = Some(authorization);
    }
}

/// Asks macOS again what the app may do, and keeps the answer. The app does this at
/// start and whenever it becomes active, as the user may change it in System Settings.
pub(crate) fn refresh_authorization(cx: &mut App) {
    if !cx.has_global::<Outside>() {
        return;
    }

    let answers = check_permission(cx);

    cx.spawn(async move |cx| {
        if let Ok(authorization) = answers.recv_async().await {
            cx.update(|cx| remember(authorization, cx));
        }
    })
    .detach();
}

struct Notifier {
    workspace: Entity<Workspace>,
    /// The notice behind the newest notification of each tag, so a click knows its task.
    last: HashMap<SharedString, Notice>,
}

impl Global for Notifier {}

/// Starts showing notices and routing clicks. `workspace` is the model the clicks open.
pub(crate) fn install(workspace: Entity<Workspace>, cx: &mut App) {
    use_delivery(Rc::new(SystemDelivery), cx);
    start(workspace, cx);
}

fn start(workspace: Entity<Workspace>, cx: &mut App) {
    cx.set_global(Notifier {
        workspace,
        last: HashMap::new(),
    });
    cx.on_system_notification_response(respond);
    refresh_authorization(cx);
}

/// Where a notice is about, in the words of the user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Subject {
    pub project: String,
    /// "Start" or "Plan" for the Start orchestrator.
    pub task: String,
}

/// A notification to show.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Post {
    pub tag: String,
    pub title: &'static str,
    pub body: String,
    pub sound: bool,
    /// Wanted by the settings and by the kind of notice. Whether the Dock bounces also
    /// depends on the window.
    pub bounce: bool,
}

/// The tag of a task's notification, so a new one replaces the old one. A Start orchestrator
/// has no task: its project holds the tag.
pub(crate) fn tag_of(notice: &Notice) -> String {
    match notice.task {
        Some(task) => format!("task-{}", task.0),
        None => format!("project-{}", notice.project.0),
    }
}

fn title_of(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::NeedsDecision => "Needs a decision",
        NoticeKind::Prompt => "Agent blocked",
        NoticeKind::ReadyForReview => "Ready for review",
        NoticeKind::Broke => "Agent failed",
    }
}

/// The notices that cannot wait.
fn is_urgent(kind: NoticeKind) -> bool {
    matches!(kind, NoticeKind::NeedsDecision | NoticeKind::Prompt)
}

fn body_of(subject: &Subject, text: &str) -> String {
    let place = [subject.project.as_str(), subject.task.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let text = text.trim();

    // "Ready for review" carries the task name as its text.
    if text.is_empty() || text == subject.task {
        place
    } else if place.is_empty() {
        text.to_owned()
    } else {
        format!("{place}: {text}")
    }
}

/// Whether a notice becomes a notification, and which. `viewing` is true when the app is
/// active, the main window is key and its open task is the notice's task.
pub(crate) fn decide(
    notice: &Notice,
    subject: &Subject,
    settings: &NotificationSettings,
    mode: ProjectMode,
    viewing: bool,
) -> Option<Post> {
    let wanted = match mode {
        ProjectMode::All => true,
        ProjectMode::ErrorsOnly => notice.kind == NoticeKind::Broke,
        ProjectMode::Off => false,
    };
    if !wanted || !settings.triggers(notice.kind) {
        return None;
    }

    if viewing && settings.mute_while_viewing {
        return None;
    }

    Some(Post {
        tag: tag_of(notice),
        title: title_of(notice.kind),
        body: body_of(subject, &notice.text),
        sound: settings.sound,
        bounce: settings.dock_bounce && is_urgent(notice.kind),
    })
}

fn open_action() -> SystemNotificationAction {
    SystemNotificationAction {
        id: OPEN_ACTION.into(),
        label: "Open".into(),
    }
}

impl Workspace {
    /// Shows a notification for a notice of the daemon, when the settings and the user's
    /// attention call for one. `window` is the main window, `None` while it is closed.
    pub(crate) fn notice_received(
        &mut self,
        notice: &Notice,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<Notifier>() {
            return;
        }

        let settings = crate::settings::notifications(cx);
        let project = self
            .data
            .project_with_daemon_id(notice.project)
            .and_then(|id| self.data.project(id));
        let mode = project
            .and_then(|project| project.root.as_deref())
            .map_or(ProjectMode::All, |root| settings.mode(root));
        let subject = self.subject_of(notice);

        // A key window means the app is active too.
        let window_is_key = window.as_deref().is_some_and(Window::is_window_active);
        let viewing = window_is_key && self.shows(notice);

        let Some(post) = decide(notice, &subject, &settings, mode, viewing) else {
            return;
        };

        let tag = SharedString::from(post.tag);
        let delivery = delivery(cx);
        delivery.post(
            SystemNotification {
                tag: tag.clone(),
                title: post.title.into(),
                body: post.body.into(),
                actions: vec![open_action()],
            },
            cx,
        );
        cx.global_mut::<Notifier>().last.insert(tag, notice.clone());

        if post.sound && sound_is_allowed(cx) {
            delivery.play_sound();
        }

        if post.bounce
            && let Some(window) = window
            && !window.is_window_active()
        {
            delivery.bounce(window);
        }
    }

    fn subject_of(&self, notice: &Notice) -> Subject {
        let project = self
            .data
            .project_with_daemon_id(notice.project)
            .and_then(|id| self.data.project(id));

        let task = match notice.task {
            Some(id) => self
                .data
                .task_with_daemon_id(id)
                .and_then(|id| self.data.task(id))
                .map(|task| task.name.to_string()),
            None => project
                .and_then(|project| self.data.start_task(project.id))
                .and_then(|id| self.data.task(id))
                .map(|task| self.data.tab_label(task).to_string())
                .or_else(|| Some("Start".to_owned())),
        };

        Subject {
            project: project
                .map(|project| project.name.to_string())
                .unwrap_or_default(),
            task: task.unwrap_or_default(),
        }
    }

    /// True when the open task is the one the notice is about.
    fn shows(&self, notice: &Notice) -> bool {
        let open = self.active_task();

        match notice.task {
            Some(task) => open.daemon.as_ref().is_some_and(|daemon| daemon.id == task),
            None => {
                open.is_start
                    && self
                        .data
                        .project(open.project)
                        .is_some_and(|project| project.daemon == Some(notice.project))
            }
        }
    }

    /// Opens what a notification is about: Review for "ready for review", else the
    /// terminals with the agent that sent it focused.
    fn open_notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        let project = self.data.project_with_daemon_id(notice.project);
        let task = match notice.task {
            Some(task) => self.data.task_with_daemon_id(task),
            None => project.and_then(|project| self.data.start_task(project)),
        };

        match (task, project) {
            (Some(task), _) => self.select_task(task, cx),
            (None, Some(project)) if notice.task.is_none() => self.select_project(project, cx),
            (None, _) => return,
        }

        if notice.kind == NoticeKind::ReadyForReview {
            self.set_view(View::Review, cx);
            return;
        }

        self.set_view(View::Terminals, cx);

        let terminal = self
            .data
            .agent_with_daemon_id(notice.agent)
            .map(|agent| agent.terminal);
        if let Some(terminal) = terminal {
            self.focus_terminal(terminal, cx);
        }
    }
}

/// A click on a notification or on its Open button. Both open the task, also with no
/// window: the main window comes back first.
fn respond(response: SystemNotificationResponse, cx: &mut App) {
    let Some(notifier) = cx.try_global::<Notifier>() else {
        return;
    };
    let notice = notifier.last.get(&response.tag).cloned();
    let workspace = notifier.workspace.clone();

    if let Some(notice) = notice {
        workspace.update(cx, |workspace, cx| workspace.open_notice(&notice, cx));
    }

    lifecycle::show_main_window(cx);
}

/// **Send test notification**: shows one at once, in the same way as a real one.
pub(crate) fn send_test(cx: &mut App) {
    let delivery = delivery(cx);

    delivery.post(
        SystemNotification {
            tag: TEST_TAG.into(),
            title: "Test notification".into(),
            body: "Swarm".into(),
            actions: vec![open_action()],
        },
        cx,
    );

    if crate::settings::notifications(cx).sound && sound_is_allowed(cx) {
        delivery.play_sound();
    }
}

/// Asks macOS what the app may do. The answer reaches `answer` later, and the receiver
/// is read on the GPUI thread.
pub(crate) fn check_permission(cx: &App) -> flume::Receiver<Authorization> {
    let (answer, answers) = flume::bounded(1);

    delivery(cx).read_permission(answer);

    answers
}

pub(crate) fn open_system_settings(cx: &App) {
    cx.open_url(NOTIFICATIONS_PANE);
}

#[cfg(test)]
pub(crate) mod harness {
    use std::cell::{Cell, RefCell};

    use gpui::VisualTestContext;

    use super::*;

    /// A delivery that records what was asked of it.
    pub(crate) struct Recorder {
        pub posted: RefCell<Vec<SystemNotification>>,
        pub sounds: Cell<usize>,
        pub bounces: Cell<usize>,
        pub permission: Cell<Permission>,
        /// The app's "play sound" setting in System Settings.
        pub sound_enabled: Cell<bool>,
        /// False while macOS has not answered: the question stays open.
        pub answers: Cell<bool>,
    }

    impl Recorder {
        pub(crate) fn new(permission: Permission) -> Rc<Self> {
            Rc::new(Self {
                posted: RefCell::new(Vec::new()),
                sounds: Cell::new(0),
                bounces: Cell::new(0),
                permission: Cell::new(permission),
                sound_enabled: Cell::new(true),
                answers: Cell::new(true),
            })
        }

        pub(crate) fn posted(&self) -> Vec<SystemNotification> {
            self.posted.borrow().clone()
        }

        pub(crate) fn tags(&self) -> Vec<String> {
            self.posted
                .borrow()
                .iter()
                .map(|notification| notification.tag.to_string())
                .collect()
        }
    }

    impl Delivery for Recorder {
        fn post(&self, notification: SystemNotification, _cx: &App) {
            self.posted.borrow_mut().push(notification);
        }

        fn play_sound(&self) {
            self.sounds.set(self.sounds.get() + 1);
        }

        fn bounce(&self, _window: &Window) {
            self.bounces.set(self.bounces.get() + 1);
        }

        fn read_permission(&self, answer: flume::Sender<Authorization>) {
            if !self.answers.get() {
                return;
            }

            let authorization = Authorization {
                permission: self.permission.get(),
                sound: self.sound_enabled.get(),
            };

            assert!(answer.send(authorization).is_ok());
        }
    }

    /// Starts the notifications on a workspace whose lifecycle runs already, with a recorder
    /// in place of macOS.
    pub(crate) fn install(
        workspace: &Entity<Workspace>,
        recorder: &Rc<Recorder>,
        cx: &mut VisualTestContext,
    ) {
        let delivery: Rc<dyn Delivery> = Rc::clone(recorder) as Rc<dyn Delivery>;

        cx.update(|_, cx| {
            use_delivery(delivery, cx);
            start(workspace.clone(), cx);
        });
        cx.run_until_parked();
    }

    /// What macOS last said, as far as the app knows.
    pub(crate) fn known_authorization(cx: &VisualTestContext) -> Option<Authorization> {
        cx.read_global(|outside: &Outside, _| outside.authorization)
    }

    /// The notice a click on `tag` would open.
    pub(crate) fn remembered(tag: &str, cx: &VisualTestContext) -> Option<Notice> {
        cx.read_global(|notifier: &Notifier, _| notifier.last.get(tag).cloned())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use swarm_protocol::{
        AgentId, AgentInfo, AgentRole, AgentStatus as DaemonAgentStatus, Client, ProjectId, TaskId,
    };

    use super::harness::{Recorder, install, known_authorization, remembered};
    use super::*;
    use crate::daemon::tests::{
        agent_info, fake_daemon_sending, project_info, send_notice, setup, start_agent, task_info,
        use_projects, wait_until,
    };
    use crate::menu_bar::harness as menu_bar;
    use crate::model::TaskId as ModelTaskId;

    fn notice(kind: NoticeKind, task: Option<u64>) -> Notice {
        Notice {
            kind,
            project: ProjectId(5),
            task: task.map(TaskId),
            agent: AgentId(1),
            text: "Allow cargo test?".to_owned(),
            at: 0,
        }
    }

    fn subject() -> Subject {
        Subject {
            project: "demo".to_owned(),
            task: "login".to_owned(),
        }
    }

    fn decided(notice: &Notice, settings: &NotificationSettings) -> Option<Post> {
        decide(notice, &subject(), settings, ProjectMode::All, false)
    }

    const KINDS: [NoticeKind; 4] = [
        NoticeKind::NeedsDecision,
        NoticeKind::Prompt,
        NoticeKind::ReadyForReview,
        NoticeKind::Broke,
    ];

    #[test]
    fn every_kind_posts_with_the_default_settings() {
        for kind in KINDS {
            let post = decided(&notice(kind, Some(7)), &NotificationSettings::default());

            assert!(post.is_some(), "{kind:?}");
        }
    }

    fn with_trigger_off(kind: NoticeKind) -> NotificationSettings {
        let mut settings = NotificationSettings::default();

        match kind {
            NoticeKind::NeedsDecision => settings.needs_decision = false,
            NoticeKind::Prompt => settings.prompt = false,
            NoticeKind::ReadyForReview => settings.ready_for_review = false,
            NoticeKind::Broke => settings.broke = false,
        }

        settings
    }

    #[test]
    fn each_trigger_switch_silences_only_its_kind() {
        for off in KINDS {
            let settings = with_trigger_off(off);

            for kind in KINDS {
                let posted = decided(&notice(kind, Some(7)), &settings).is_some();

                assert_eq!(posted, kind != off, "{kind:?} with {off:?} off");
            }
        }
    }

    #[test]
    fn a_project_on_off_posts_nothing() {
        for kind in KINDS {
            let post = decide(
                &notice(kind, Some(7)),
                &subject(),
                &NotificationSettings::default(),
                ProjectMode::Off,
                false,
            );

            assert_eq!(post, None, "{kind:?}");
        }
    }

    #[test]
    fn a_project_on_errors_only_posts_only_what_broke() {
        for kind in KINDS {
            let post = decide(
                &notice(kind, Some(7)),
                &subject(),
                &NotificationSettings::default(),
                ProjectMode::ErrorsOnly,
                false,
            );

            assert_eq!(post.is_some(), kind == NoticeKind::Broke, "{kind:?}");
        }
    }

    #[test]
    fn errors_only_does_not_override_a_trigger_that_is_off() {
        let settings = NotificationSettings {
            broke: false,
            ..NotificationSettings::default()
        };

        let post = decide(
            &notice(NoticeKind::Broke, Some(7)),
            &subject(),
            &settings,
            ProjectMode::ErrorsOnly,
            false,
        );

        assert_eq!(post, None);
    }

    #[test]
    fn viewing_the_task_mutes_it_when_muting_is_on() {
        for kind in KINDS {
            let post = decide(
                &notice(kind, Some(7)),
                &subject(),
                &NotificationSettings::default(),
                ProjectMode::All,
                true,
            );

            assert_eq!(post, None, "{kind:?}");
        }
    }

    #[test]
    fn viewing_the_task_posts_when_muting_is_off() {
        let settings = NotificationSettings {
            mute_while_viewing: false,
            ..NotificationSettings::default()
        };

        let post = decide(
            &notice(NoticeKind::Prompt, Some(7)),
            &subject(),
            &settings,
            ProjectMode::All,
            true,
        );

        assert!(post.is_some());
    }

    #[test]
    fn a_task_notice_is_tagged_by_its_task() {
        let post = decided(
            &notice(NoticeKind::Prompt, Some(7)),
            &NotificationSettings::default(),
        );

        assert_eq!(post.map(|post| post.tag), Some("task-7".to_owned()));
    }

    #[test]
    fn a_start_orchestrator_without_a_task_is_tagged_by_its_project() {
        let post = decide(
            &notice(NoticeKind::NeedsDecision, None),
            &Subject {
                project: "demo".to_owned(),
                task: "Start".to_owned(),
            },
            &NotificationSettings::default(),
            ProjectMode::All,
            false,
        );

        let post = post.expect("a notification");
        assert_eq!(post.tag, "project-5");
        assert_eq!(post.body, "demo \u{b7} Start: Allow cargo test?");
    }

    #[test]
    fn a_newer_notice_of_the_same_task_gets_the_same_tag() {
        let settings = NotificationSettings::default();

        let first = decided(&notice(NoticeKind::Prompt, Some(7)), &settings);
        let second = decided(&notice(NoticeKind::Broke, Some(7)), &settings);

        assert_eq!(first.map(|post| post.tag), second.map(|post| post.tag));
    }

    #[test]
    fn the_text_names_the_kind_the_task_and_the_notice() {
        let titles: Vec<&str> = KINDS
            .into_iter()
            .filter_map(|kind| decided(&notice(kind, Some(7)), &NotificationSettings::default()))
            .map(|post| post.title)
            .collect();
        let post = decided(
            &notice(NoticeKind::Prompt, Some(7)),
            &NotificationSettings::default(),
        )
        .expect("a notification");

        assert_eq!(
            titles,
            [
                "Needs a decision",
                "Agent blocked",
                "Ready for review",
                "Agent failed"
            ]
        );
        assert_eq!(post.body, "demo \u{b7} login: Allow cargo test?");
    }

    #[test]
    fn the_task_name_is_not_said_twice() {
        let mut ready = notice(NoticeKind::ReadyForReview, Some(7));
        ready.text = "login".to_owned();

        let post = decided(&ready, &NotificationSettings::default()).expect("a notification");

        assert_eq!(post.body, "demo \u{b7} login");
    }

    #[test]
    fn a_notice_of_an_unknown_task_still_has_its_text() {
        let post = decide(
            &notice(NoticeKind::Prompt, Some(7)),
            &Subject::default(),
            &NotificationSettings::default(),
            ProjectMode::All,
            false,
        )
        .expect("a notification");

        assert_eq!(post.body, "Allow cargo test?");
    }

    #[test]
    fn sound_follows_its_setting() {
        let mut settings = NotificationSettings::default();
        let off = decided(&notice(NoticeKind::Prompt, Some(7)), &settings);
        settings.sound = true;
        let on = decided(&notice(NoticeKind::Prompt, Some(7)), &settings);

        assert_eq!(off.map(|post| post.sound), Some(false));
        assert_eq!(on.map(|post| post.sound), Some(true));
    }

    #[test]
    fn only_urgent_notices_ask_for_a_bounce() {
        for kind in KINDS {
            let post = decided(&notice(kind, Some(7)), &NotificationSettings::default())
                .expect("a notification");

            let urgent = matches!(kind, NoticeKind::NeedsDecision | NoticeKind::Prompt);
            assert_eq!(post.bounce, urgent, "{kind:?}");
        }
    }

    #[test]
    fn no_bounce_is_asked_for_when_the_setting_is_off() {
        let settings = NotificationSettings {
            dock_bounce: false,
            ..NotificationSettings::default()
        };

        let post = decided(&notice(NoticeKind::Prompt, Some(7)), &settings);

        assert_eq!(post.map(|post| post.bounce), Some(false));
    }

    #[test]
    fn the_permission_has_four_labels() {
        assert_eq!(Permission::Allowed.label(), "Allowed");
        assert_eq!(Permission::NotAllowed.label(), "Not allowed");
        assert_eq!(Permission::NotAsked.label(), "Not asked");
        assert_eq!(Permission::Unavailable.label(), "Unavailable");
    }

    // The tests below run a fake daemon that sends the notices, a workspace with two tasks,
    // a recorder in place of macOS, and the user away from the window unless a test says
    // otherwise.

    fn sent(kind: NoticeKind, task: Option<u64>, agent: u64, text: &str) -> Notice {
        Notice {
            kind,
            project: ProjectId(5),
            task: task.map(TaskId),
            agent: AgentId(agent),
            text: text.to_owned(),
            at: 0,
        }
    }

    struct Rig {
        workspace: Entity<Workspace>,
        recorder: Rc<Recorder>,
        client: Client,
    }

    /// Task 7 "login" has the orchestrator 1 and the worker 2. Task 8 "signup" has the
    /// orchestrator 3. The user has task 7 open.
    fn rig(notices: Vec<Notice>, cx: &mut TestAppContext) -> (Rig, &mut VisualTestContext) {
        rig_with(notices, Recorder::new(Permission::Allowed), cx)
    }

    /// Like [`rig`], with the recorder that plays macOS.
    fn rig_with(
        notices: Vec<Notice>,
        recorder: Rc<Recorder>,
        cx: &mut TestAppContext,
    ) -> (Rig, &mut VisualTestContext) {
        let socket = fake_daemon_sending(notices);
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        use_projects(
            &workspace,
            &[project_info(
                5,
                "demo",
                vec![task_info(7, 5, "login"), task_info(8, 5, "signup")],
            )],
            cx,
        );
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        menu_bar::install(&workspace, cx);
        install(&workspace, &recorder, cx);

        let worker = AgentInfo {
            role: AgentRole::Worker,
            name: "worker".into(),
            ..agent_info(2, 7, DaemonAgentStatus::Idle)
        };
        for info in [
            agent_info(1, 7, DaemonAgentStatus::Idle),
            worker,
            agent_info(3, 8, DaemonAgentStatus::Idle),
        ] {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.agent_changed(&info, Some(window), cx);
            });
        }
        cx.deactivate_window();

        let rig = Rig {
            workspace,
            recorder,
            client,
        };

        (rig, cx)
    }

    fn look_at_the_window(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
    }

    fn close_window(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    /// Sends the notices with these numbers, in order, and waits until the recorder holds
    /// `posts` notifications.
    fn deliver(rig: &Rig, numbers: &[u64], posts: usize, cx: &mut VisualTestContext) -> bool {
        for number in numbers {
            send_notice(&rig.client, *number);
        }

        wait_until(cx, |_| rig.recorder.posted().len() == posts)
    }

    fn click(tag: &str, action: Option<&str>, cx: &mut VisualTestContext) {
        cx.simulate_system_notification_response(SystemNotificationResponse {
            tag: SharedString::from(tag.to_owned()),
            action_id: action.map(|action| SharedString::from(action.to_owned())),
        });
        cx.run_until_parked();
    }

    fn open_task(rig: &Rig, cx: &VisualTestContext) -> Option<TaskId> {
        rig.workspace.read_with(cx, |workspace, _| {
            workspace
                .active_task()
                .daemon
                .as_ref()
                .map(|daemon| daemon.id)
        })
    }

    fn view_of(rig: &Rig, cx: &VisualTestContext) -> View {
        rig.workspace.read_with(cx, |workspace, _| workspace.view)
    }

    #[gpui::test]
    fn a_notice_from_the_daemon_is_posted_with_its_tag_and_text(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted, "the notice never became a notification");
        let shown = rig.recorder.posted();
        assert_eq!(shown[0].tag, "task-7");
        assert_eq!(shown[0].title, "Agent blocked");
        assert_eq!(shown[0].body, "demo \u{b7} login: Allow cargo test?");
        assert_eq!(
            shown[0].actions,
            [SystemNotificationAction {
                id: "open".into(),
                label: "Open".into(),
            }]
        );
    }

    #[gpui::test]
    fn a_second_notice_for_the_same_task_uses_the_same_tag(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?"),
            sent(NoticeKind::NeedsDecision, Some(7), 1, "Merge now?"),
            sent(NoticeKind::NeedsDecision, Some(8), 3, "Which API?"),
        ];
        let (rig, cx) = rig(notices, cx);

        let posted = deliver(&rig, &[0, 1, 2], 3, cx);

        assert!(posted);
        assert_eq!(rig.recorder.tags(), ["task-7", "task-7", "task-8"]);
        assert_eq!(
            rig.recorder.posted()[1].body,
            "demo \u{b7} login: Merge now?"
        );
    }

    #[gpui::test]
    fn a_start_orchestrator_notice_is_tagged_by_its_project(cx: &mut TestAppContext) {
        let notices = vec![sent(
            NoticeKind::NeedsDecision,
            None,
            100,
            "Split it in two?",
        )];
        let (rig, cx) = rig(notices, cx);
        let start = start_agent(100, 5, DaemonAgentStatus::Idle);
        rig.workspace.update_in(cx, |workspace, window, cx| {
            workspace.agent_changed(&start, Some(window), cx);
        });

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted);
        let shown = rig.recorder.posted();
        assert_eq!(shown[0].tag, "project-5");
        assert_eq!(shown[0].body, "demo \u{b7} Plan: Split it in two?");
    }

    #[gpui::test]
    fn a_notice_of_the_open_task_is_muted_while_the_user_looks_at_it(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?"),
            sent(NoticeKind::Prompt, Some(8), 3, "Allow cargo fmt?"),
        ];
        let (rig, cx) = rig(notices, cx);
        look_at_the_window(cx);

        let posted = deliver(&rig, &[0, 1], 1, cx);

        assert_eq!(open_task(&rig, cx), Some(TaskId(7)));
        assert!(posted);
        assert_eq!(rig.recorder.tags(), ["task-8"]);
    }

    #[gpui::test]
    fn a_notice_of_the_open_task_is_posted_when_the_window_is_not_key(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert_eq!(open_task(&rig, cx), Some(TaskId(7)));
        assert!(posted);
    }

    #[gpui::test]
    fn muting_off_posts_a_notice_of_the_task_the_user_looks_at(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        cx.update(|_, cx| {
            crate::settings::change_notifications(|n| n.mute_while_viewing = false, cx);
        });
        look_at_the_window(cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted);
    }

    #[gpui::test]
    fn a_project_mode_from_the_settings_decides_by_the_project_root(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?"),
            sent(NoticeKind::ReadyForReview, Some(8), 3, "signup"),
            sent(NoticeKind::Broke, Some(8), 3, "Exited with code 1"),
        ];
        let (rig, cx) = rig(notices, cx);
        cx.update(|_, cx| {
            crate::settings::change_notifications(
                |n| n.set_mode(std::path::Path::new("/tmp/demo"), ProjectMode::ErrorsOnly),
                cx,
            );
        });

        let posted = deliver(&rig, &[0, 1, 2], 1, cx);

        assert!(posted);
        let shown = rig.recorder.posted();
        assert_eq!(shown[0].title, "Agent failed");
    }

    #[gpui::test]
    fn a_project_that_is_off_posts_nothing_while_another_project_still_posts(
        cx: &mut TestAppContext,
    ) {
        let notices = vec![
            sent(NoticeKind::Broke, Some(8), 3, "Exited with code 1"),
            Notice {
                project: ProjectId(6),
                ..sent(NoticeKind::Broke, Some(9), 4, "Exited with code 2")
            },
        ];
        let (rig, cx) = rig(notices, cx);
        use_projects(
            &rig.workspace,
            &[
                project_info(
                    5,
                    "demo",
                    vec![task_info(7, 5, "login"), task_info(8, 5, "signup")],
                ),
                project_info(6, "other", vec![task_info(9, 6, "billing")]),
            ],
            cx,
        );
        cx.update(|_, cx| {
            crate::settings::change_notifications(
                |n| n.set_mode(std::path::Path::new("/tmp/demo"), ProjectMode::Off),
                cx,
            );
        });

        let posted = deliver(&rig, &[0, 1], 1, cx);

        assert!(posted, "the notice of the other project never arrived");
        assert_eq!(rig.recorder.tags(), ["task-9"]);
    }

    #[gpui::test]
    fn a_notice_is_posted_with_no_window_open(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::NeedsDecision, Some(7), 1, "Merge now?")];
        let (rig, cx) = rig(notices, cx);
        close_window(cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted, "the notice was lost while no window was open");
        assert!(cx.windows().is_empty());
        assert_eq!(rig.recorder.tags(), ["task-7"]);
    }

    #[gpui::test]
    fn a_notification_plays_a_sound_only_when_the_setting_is_on(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(7), 2, "first"),
            sent(NoticeKind::Prompt, Some(7), 2, "second"),
        ];
        let (rig, cx) = rig(notices, cx);

        deliver(&rig, &[0], 1, cx);
        let quiet = rig.recorder.sounds.get();
        cx.update(|_, cx| crate::settings::change_notifications(|n| n.sound = true, cx));
        deliver(&rig, &[1], 2, cx);

        assert_eq!(quiet, 0);
        assert_eq!(rig.recorder.sounds.get(), 1);
    }

    #[gpui::test]
    fn no_sound_plays_before_macos_answered(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(8), 3, "first"),
            sent(NoticeKind::Prompt, Some(8), 3, "second"),
        ];
        let recorder = Recorder::new(Permission::Allowed);
        recorder.answers.set(false);
        let (rig, cx) = rig_with(notices, recorder, cx);
        cx.update(|_, cx| crate::settings::change_notifications(|n| n.sound = true, cx));

        deliver(&rig, &[0], 1, cx);
        let unknown = (known_authorization(cx), rig.recorder.sounds.get());
        rig.recorder.answers.set(true);
        look_at_the_window(cx);
        deliver(&rig, &[1], 2, cx);

        assert_eq!(unknown, (None, 0));
        assert_eq!(rig.recorder.sounds.get(), 1);
    }

    #[gpui::test]
    fn no_sound_plays_when_macos_shows_nothing_or_has_the_sound_off(cx: &mut TestAppContext) {
        let notices = (0..6)
            .map(|index| sent(NoticeKind::Prompt, Some(8), 3, &format!("notice {index}")))
            .collect();
        let (rig, cx) = rig(notices, cx);
        cx.update(|_, cx| crate::settings::change_notifications(|n| n.sound = true, cx));
        let answers = [
            (Permission::Allowed, true, 1),
            (Permission::NotAllowed, true, 1),
            (Permission::NotAsked, true, 1),
            (Permission::Unavailable, true, 1),
            (Permission::Allowed, false, 1),
            (Permission::Allowed, true, 2),
        ];

        for (number, (permission, sound, sounds)) in (0..).zip(answers) {
            rig.recorder.permission.set(permission);
            rig.recorder.sound_enabled.set(sound);
            cx.deactivate_window();
            look_at_the_window(cx);

            deliver(
                &rig,
                &[number],
                usize::try_from(number).unwrap_or_default() + 1,
                cx,
            );

            assert_eq!(
                rig.recorder.sounds.get(),
                sounds,
                "{permission:?} with the sound {sound}"
            );
        }
    }

    #[gpui::test]
    fn the_test_notification_plays_no_sound_when_macos_shows_nothing(cx: &mut TestAppContext) {
        let (rig, cx) = rig(Vec::new(), cx);
        cx.update(|_, cx| crate::settings::change_notifications(|n| n.sound = true, cx));
        rig.recorder.permission.set(Permission::NotAllowed);
        cx.deactivate_window();
        look_at_the_window(cx);

        cx.update(|_, cx| send_test(cx));
        let denied = rig.recorder.sounds.get();
        rig.recorder.permission.set(Permission::Allowed);
        cx.deactivate_window();
        look_at_the_window(cx);
        cx.update(|_, cx| send_test(cx));

        assert_eq!(denied, 0);
        assert_eq!(rig.recorder.sounds.get(), 1);
    }

    #[gpui::test]
    fn the_dock_bounces_only_for_urgent_notices(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::NeedsDecision, Some(7), 1, "Merge now?"),
            sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?"),
            sent(NoticeKind::ReadyForReview, Some(7), 1, "login"),
            sent(NoticeKind::Broke, Some(7), 2, "Exited with code 1"),
        ];
        let (rig, cx) = rig(notices, cx);

        let posted = deliver(&rig, &[0], 1, cx);
        let after_decision = rig.recorder.bounces.get();
        deliver(&rig, &[1], 2, cx);
        let after_prompt = rig.recorder.bounces.get();
        deliver(&rig, &[2, 3], 4, cx);

        assert!(posted);
        assert_eq!(after_decision, 1);
        assert_eq!(after_prompt, 2);
        assert_eq!(rig.recorder.bounces.get(), 2);
    }

    #[gpui::test]
    fn the_dock_does_not_bounce_when_the_setting_is_off(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        cx.update(|_, cx| crate::settings::change_notifications(|n| n.dock_bounce = false, cx));

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted);
        assert_eq!(rig.recorder.bounces.get(), 0);
    }

    #[gpui::test]
    fn the_dock_does_not_bounce_while_the_window_is_key(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        cx.update(|_, cx| {
            crate::settings::change_notifications(|n| n.mute_while_viewing = false, cx);
        });
        look_at_the_window(cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted);
        assert_eq!(rig.recorder.bounces.get(), 0);
    }

    #[gpui::test]
    fn the_dock_does_not_bounce_with_no_main_window(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        close_window(cx);

        let posted = deliver(&rig, &[0], 1, cx);

        assert!(posted);
        assert_eq!(rig.recorder.bounces.get(), 0);
    }

    #[gpui::test]
    fn the_last_notice_of_a_tag_is_remembered_for_the_click(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::Prompt, Some(7), 2, "first"),
            sent(NoticeKind::ReadyForReview, Some(7), 1, "login"),
        ];
        let (rig, cx) = rig(notices, cx);

        deliver(&rig, &[0, 1], 2, cx);

        let kept = remembered("task-7", cx).map(|notice| notice.kind);
        assert_eq!(kept, Some(NoticeKind::ReadyForReview));
        assert_eq!(remembered("task-8", cx), None);
    }

    #[gpui::test]
    fn a_click_on_ready_for_review_opens_review(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::ReadyForReview, Some(8), 3, "signup")];
        let (rig, cx) = rig(notices, cx);
        deliver(&rig, &[0], 1, cx);
        let before = (open_task(&rig, cx), view_of(&rig, cx));

        click("task-8", Some("open"), cx);

        assert_eq!(before, (Some(TaskId(7)), View::Terminals));
        assert_eq!(open_task(&rig, cx), Some(TaskId(8)));
        assert_eq!(view_of(&rig, cx), View::Review);
    }

    #[gpui::test]
    fn a_click_on_a_prompt_opens_the_terminals_with_that_agent_focused(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        rig.workspace.update(cx, |workspace, cx| {
            workspace.select_task(ModelTaskId(1), cx);
            workspace.set_view(View::Review, cx);
        });
        deliver(&rig, &[0], 1, cx);
        let worker = rig.workspace.read_with(cx, |workspace, _| {
            workspace
                .data
                .agent_with_daemon_id(AgentId(2))
                .map(|agent| agent.terminal)
        });
        let away = (open_task(&rig, cx), view_of(&rig, cx));

        click("task-7", None, cx);

        let focused = rig
            .workspace
            .read_with(cx, |workspace, _| workspace.focused_terminal);
        assert_ne!(away.0, Some(TaskId(7)), "the click had nothing to do");
        assert_eq!(away.1, View::Review);
        assert!(worker.is_some());
        assert_eq!(open_task(&rig, cx), Some(TaskId(7)));
        assert_eq!(view_of(&rig, cx), View::Terminals);
        assert_eq!(focused, worker);
    }

    #[gpui::test]
    fn a_click_with_the_window_closed_brings_it_back_on_the_task(cx: &mut TestAppContext) {
        let notices = vec![
            sent(NoticeKind::ReadyForReview, Some(8), 3, "signup"),
            sent(NoticeKind::Prompt, Some(7), 2, "Allow cargo test?"),
        ];
        let (rig, cx) = rig(notices, cx);
        close_window(cx);
        deliver(&rig, &[0, 1], 2, cx);

        click("task-8", Some("open"), cx);

        let review = (cx.windows().len(), open_task(&rig, cx), view_of(&rig, cx));
        let mut second = VisualTestContext::from_window(cx.windows()[0], cx);
        close_window(&mut second);
        click("task-7", None, cx);
        let focused = rig.workspace.read_with(cx, |workspace, _| {
            let worker = workspace
                .data
                .agent_with_daemon_id(AgentId(2))
                .map(|agent| agent.terminal);

            (workspace.focused_terminal, worker)
        });
        assert_eq!(review, (1, Some(TaskId(8)), View::Review));
        assert_eq!(cx.windows().len(), 1);
        assert_eq!(open_task(&rig, cx), Some(TaskId(7)));
        assert_eq!(view_of(&rig, cx), View::Terminals);
        assert_eq!(focused.0, focused.1);
        assert!(focused.1.is_some());
    }

    #[gpui::test]
    fn a_click_on_a_start_notice_opens_the_start_tab(cx: &mut TestAppContext) {
        let notices = vec![sent(
            NoticeKind::NeedsDecision,
            None,
            100,
            "Split it in two?",
        )];
        let (rig, cx) = rig(notices, cx);
        let start = start_agent(100, 5, DaemonAgentStatus::Idle);
        rig.workspace.update_in(cx, |workspace, window, cx| {
            workspace.agent_changed(&start, Some(window), cx);
        });
        deliver(&rig, &[0], 1, cx);

        click("project-5", Some("open"), cx);

        let (is_start, focused, terminal) = rig.workspace.read_with(cx, |workspace, _| {
            (
                workspace.active_task().is_start,
                workspace.focused_terminal,
                workspace
                    .data
                    .agent_with_daemon_id(AgentId(100))
                    .map(|agent| agent.terminal),
            )
        });
        assert!(is_start);
        assert_eq!(focused, terminal);
        assert!(terminal.is_some());
    }

    #[gpui::test]
    fn a_click_for_a_task_the_app_no_longer_knows_only_shows_the_window(cx: &mut TestAppContext) {
        let notices = vec![sent(NoticeKind::Prompt, Some(99), 2, "Allow cargo test?")];
        let (rig, cx) = rig(notices, cx);
        let signup = rig.workspace.read_with(cx, |workspace, _| {
            workspace.data.task_with_daemon_id(TaskId(8))
        });
        rig.workspace.update(cx, |workspace, cx| {
            workspace.select_task(signup.expect("task 8"), cx);
            workspace.set_view(View::Review, cx);
        });
        deliver(&rig, &[0], 1, cx);
        close_window(cx);

        click("task-99", Some("open"), cx);

        assert_eq!(cx.windows().len(), 1);
        assert_eq!(open_task(&rig, cx), Some(TaskId(8)));
        assert_eq!(view_of(&rig, cx), View::Review);
    }

    #[gpui::test]
    fn a_click_on_a_notification_the_app_does_not_know_only_shows_the_window(
        cx: &mut TestAppContext,
    ) {
        let (rig, cx) = rig(Vec::new(), cx);
        close_window(cx);

        click("test", Some("open"), cx);

        assert_eq!(cx.windows().len(), 1);
        assert_eq!(open_task(&rig, cx), Some(TaskId(7)));
    }

    #[gpui::test]
    fn the_test_notification_is_posted_at_once(cx: &mut TestAppContext) {
        let (rig, cx) = rig(Vec::new(), cx);

        cx.update(|_, cx| send_test(cx));

        let shown = rig.recorder.posted();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].tag, "test");
        assert_eq!(shown[0].title, "Test notification");
        assert_eq!(rig.recorder.sounds.get(), 0);
    }
}
