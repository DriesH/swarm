//! The Report an issue dialog: a filled-in GitHub issue form, opened in the browser.
//!
//! GitHub fills a field of an issue form from the address when the query key is the
//! field's `id`: <https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/creating-an-issue>.
//! The forms are in `.github/ISSUE_TEMPLATE/`, see
//! <https://docs.github.com/en/communities/using-templates-to-encourage-useful-issues-and-pull-requests/syntax-for-issue-forms>.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    Role, SharedString, Subscription, Window, div, px,
};
use swarm_protocol::{Client, Paths, SystemCheck};
use swarm_ui::prelude::*;

use crate::actions::ReportIssue;
use crate::announce::announce;
use crate::lifecycle;
use crate::palette::Overlay;
use crate::workspace::Workspace;

const NEW_ISSUE_URL: &str = "https://github.com/DriesH/swarm/issues/new";
const REPOSITORY: &str = "github.com/DriesH/swarm";

/// GitHub answers `414 URI Too Long` above a server limit it does not publish.
pub(crate) const MAX_URL_LENGTH: usize = 6000;
pub(crate) const LOG_LINES: usize = 200;
/// A log line is far shorter than this, so the tail holds the last lines.
const LOG_TAIL_BYTES: u64 = 256 * 1024;
const CUT_MARK: &str = "\u{2026}";
/// The most an address spends on one character: four UTF-8 bytes, three characters each.
const MOST_ENCODED_PER_CHAR: usize = 12;
const VERSIONS_KEY: &str = "versions";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bug,
    Feature,
    Feedback,
}

impl Kind {
    pub(crate) const ALL: [Self; 3] = [Self::Bug, Self::Feature, Self::Feedback];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Bug => "Bug",
            Self::Feature => "Feature request",
            Self::Feedback => "Feedback",
        }
    }

    /// The issue form in `.github/ISSUE_TEMPLATE/`.
    fn template(self) -> &'static str {
        match self {
            Self::Bug => "bug.yml",
            Self::Feature => "feature.yml",
            Self::Feedback => "feedback.yml",
        }
    }

    pub(crate) fn description_label(self) -> &'static str {
        match self {
            Self::Bug => "What happened",
            Self::Feature => "What you want",
            Self::Feedback => "Your feedback",
        }
    }

    /// The `id` of the form's description field.
    fn description_key(self) -> &'static str {
        match self {
            Self::Bug => "what-happened",
            Self::Feature => "what-you-want",
            Self::Feedback => "feedback",
        }
    }

    pub(crate) fn has_steps(self) -> bool {
        self == Self::Bug
    }
}

/// The versions the report names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Versions {
    pub(crate) swarm: String,
    pub(crate) macos: Option<String>,
    pub(crate) claude: Option<String>,
    pub(crate) codex: Option<String>,
}

impl Versions {
    /// Swarm's own version, until the rest is read.
    pub(crate) fn of_swarm() -> Self {
        Self {
            swarm: env!("CARGO_PKG_VERSION").to_owned(),
            ..Self::default()
        }
    }

    /// Everything the machine says: macOS from `sw_vers`, the tools from the daemon's
    /// system check.
    pub(crate) fn detect(client: Option<&Client>) -> Self {
        let mut versions = Self::of_swarm();
        versions.macos = macos_version();

        if let Some(check) = client.and_then(|client| client.system_check().ok()) {
            versions.add_tools(&check);
        }

        versions
    }

    fn add_tools(&mut self, check: &SystemCheck) {
        self.claude.clone_from(&check.claude.version);
        self.codex.clone_from(&check.codex.version);
    }

    /// "Swarm 1.0.0 · macOS 27.0.1 · Claude Code 2.1.318 · Codex 0.166.0".
    pub(crate) fn text(&self) -> String {
        let named = [
            ("macOS", &self.macos),
            ("Claude Code", &self.claude),
            ("Codex", &self.codex),
        ];
        let mut parts = vec![format!("Swarm {}", self.swarm)];

        parts.extend(
            named
                .into_iter()
                .filter_map(|(name, version)| Some(format!("{name} {}", version.as_ref()?))),
        );

        parts.join(" \u{b7} ")
    }
}

fn macos_version() -> Option<String> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;

    let version = text.trim();

    (!version.is_empty()).then(|| version.to_owned())
}

/// What the form holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Draft {
    pub(crate) kind: Kind,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) steps: String,
    /// `None` when the versions are left out.
    pub(crate) versions: Option<String>,
}

/// One query key of the address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) label: &'static str,
    key: &'static str,
    body: String,
    pub(crate) cut: bool,
}

impl Field {
    fn new(label: &'static str, key: &'static str, text: &str) -> Option<Self> {
        let body = text.trim();

        (!body.is_empty()).then(|| Self {
            label,
            key,
            body: body.to_owned(),
            cut: false,
        })
    }

    /// The text the address carries.
    pub(crate) fn text(&self) -> String {
        if self.cut {
            format!("{}{CUT_MARK}", self.body)
        } else {
            self.body.clone()
        }
    }

    /// Drops characters from the end, enough for `excess` bytes at least when they are
    /// plain text. A field that is empty already cannot be cut.
    fn cut_by(&mut self, excess: usize) {
        let drop = (excess / MOST_ENCODED_PER_CHAR).max(1);
        let keep = self.body.chars().count().saturating_sub(drop);

        self.body = self.body.chars().take(keep).collect();
        self.cut = true;
    }
}

/// The address and what it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) url: String,
    pub(crate) fields: Vec<Field>,
}

impl Link {
    /// The text under "Show what is sent".
    pub(crate) fn disclosure(&self, with_log: bool) -> String {
        let mut sections: Vec<String> = self
            .fields
            .iter()
            .map(|field| format!("{}\n{}", field.label, field.text()))
            .collect();

        if with_log {
            sections.push(format!(
                "Daemon log\nThe last {LOG_LINES} lines go to the clipboard, not to the address"
            ));
        }

        let cut: Vec<&str> = self
            .fields
            .iter()
            .filter(|field| field.cut)
            .map(|field| field.label)
            .collect();
        if !cut.is_empty() {
            sections.push(format!("Cut to fit the address\n{}", cut.join(", ")));
        }

        sections.join("\n\n")
    }
}

impl Draft {
    fn fields(&self) -> Vec<Field> {
        [
            Field::new("Title", "title", &self.title),
            Field::new(
                self.kind.description_label(),
                self.kind.description_key(),
                &self.description,
            ),
            self.kind
                .has_steps()
                .then(|| Field::new("Steps to reproduce", "steps", &self.steps))
                .flatten(),
            self.versions
                .as_deref()
                .and_then(|versions| Field::new("Versions", VERSIONS_KEY, versions)),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The address that opens the filled-in form. It stays under [`MAX_URL_LENGTH`]:
    /// the longest field is cut first and ends with "…".
    pub(crate) fn link(&self) -> Link {
        let mut fields = self.fields();

        loop {
            let excess = url_of(self.kind, &fields)
                .len()
                .saturating_sub(MAX_URL_LENGTH);
            if excess == 0 {
                break;
            }

            let longest = fields
                .iter_mut()
                .filter(|field| field.key != VERSIONS_KEY && !field.body.is_empty())
                .max_by_key(|field| percent_encode(&field.text()).len());
            let Some(field) = longest else {
                break;
            };

            field.cut_by(excess);
        }

        Link {
            url: url_of(self.kind, &fields),
            fields,
        }
    }
}

fn url_of(kind: Kind, fields: &[Field]) -> String {
    let mut url = format!("{NEW_ISSUE_URL}?template={}", kind.template());

    for field in fields {
        url.push('&');
        url.push_str(field.key);
        url.push('=');
        url.push_str(&percent_encode(&field.text()));
    }

    url
}

/// Everything but the unreserved characters of RFC 3986 becomes `%XX` of its UTF-8 bytes.
pub(crate) fn percent_encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(text.len());

    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }

    encoded
}

/// The last `count` lines of the text.
pub(crate) fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.len().saturating_sub(count);

    lines[first..].join("\n")
}

/// The last [`LOG_LINES`] lines of the daemon log. It reads only the end of the file.
pub(crate) fn read_log_tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(LOG_TAIL_BYTES);

    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;

    let text = String::from_utf8_lossy(&bytes);
    // The tail starts in the middle of a line.
    let whole = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };

    Ok(last_lines(whole, LOG_LINES))
}

/// What a failure screen fills in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefill {
    pub(crate) kind: Kind,
    pub(crate) title: String,
    pub(crate) description: String,
}

impl Prefill {
    /// A bug report with the error as its description.
    pub(crate) fn bug(title: &str, error: &str) -> Self {
        Self {
            kind: Kind::Bug,
            title: title.to_owned(),
            description: error.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportIssueEvent {
    Dismissed,
}

/// The open dialog. It reports when it is done and leaves closing to its owner.
pub(crate) struct ReportIssueDialog {
    kind: Kind,
    title: Entity<TextInput>,
    description: Entity<TextArea>,
    steps: Entity<TextArea>,
    include_versions: bool,
    include_log: bool,
    details_open: bool,
    versions: Versions,
    log_path: Option<PathBuf>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    pub(crate) opener: Option<FocusHandle>,
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<ReportIssueEvent> for ReportIssueDialog {}

impl gpui::Focusable for ReportIssueDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReportIssueDialog {
    pub(crate) fn new(
        prefill: Option<Prefill>,
        log_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let opener = window.focused(cx);
        let kind = prefill.as_ref().map_or(Kind::Bug, |prefill| prefill.kind);

        let title = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_label("Title", cx);
            input
        });
        let description = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label(kind.description_label(), cx);
            area.set_rows(4, cx);
            area
        });
        let steps = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_label("Steps to reproduce", cx);
            area.set_rows(3, cx);
            area
        });

        if let Some(prefill) = prefill {
            title.update(cx, |input, cx| input.set_text(prefill.title, cx));
            description.update(cx, |area, cx| area.set_text(prefill.description, cx));
        }
        window.focus(&title.focus_handle(cx), cx);

        let on_title = cx.subscribe(&title, Self::field_event);
        let on_description = cx.subscribe(&description, Self::field_event);
        let on_steps = cx.subscribe(&steps, Self::field_event);

        Self {
            kind,
            title,
            description,
            steps,
            include_versions: true,
            include_log: false,
            details_open: false,
            versions: Versions::of_swarm(),
            log_path,
            error: None,
            focus_handle: cx.focus_handle(),
            opener,
            _subscriptions: [on_title, on_description, on_steps],
        }
    }

    fn field_event<T: 'static>(
        &mut self,
        _field: Entity<T>,
        event: &TextInputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Changed(_) => cx.notify(),
            TextInputEvent::Submitted(_) => self.open(cx),
        }
    }

    pub(crate) fn set_versions(&mut self, versions: Versions, cx: &mut Context<Self>) {
        self.versions = versions;
        cx.notify();
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.description
            .update(cx, |area, cx| area.set_label(kind.description_label(), cx));
        cx.notify();
    }

    pub(crate) fn draft(&self, cx: &App) -> Draft {
        Draft {
            kind: self.kind,
            title: self.title.read(cx).text().to_string(),
            description: self.description.read(cx).text().to_string(),
            steps: self.steps.read(cx).text().to_string(),
            versions: self.include_versions.then(|| self.versions.text()),
        }
    }

    /// Copies the log when asked, opens the filled-in form and closes the dialog. When
    /// the log cannot be read nothing opens, and the dialog says why.
    pub(crate) fn open(&mut self, cx: &mut Context<Self>) {
        if self.include_log && !self.copy_log(cx) {
            return;
        }

        let link = self.draft(cx).link();
        cx.open_url(&link.url);
        cx.emit(ReportIssueEvent::Dismissed);
    }

    fn copy_log(&mut self, cx: &mut Context<Self>) -> bool {
        let read = match &self.log_path {
            Some(path) => read_log_tail(path),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no log file")),
        };

        match read {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.error = None;

                true
            }
            Err(error) => {
                log::warn!("cannot read the daemon log: {error}");
                self.error = Some("Cannot read the daemon log".into());
                cx.notify();

                false
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn choose(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.set_kind(kind, cx);
    }

    #[cfg(test)]
    pub(crate) fn include_log(&mut self, include: bool) {
        self.include_log = include;
    }

    #[cfg(test)]
    pub(crate) fn include_versions(&mut self, include: bool) {
        self.include_versions = include;
    }

    #[cfg(test)]
    pub(crate) fn fill(&self, title: &str, description: &str, cx: &mut Context<Self>) {
        self.title
            .update(cx, |input, cx| input.set_text(title.to_owned(), cx));
        self.description
            .update(cx, |area, cx| area.set_text(description.to_owned(), cx));
    }

    #[cfg(test)]
    pub(crate) fn details_are_open(&self) -> bool {
        self.details_open
    }
}

fn field(label: &'static str, control: impl IntoElement) -> gpui::Div {
    v_flex()
        .gap(px(6.))
        .min_w_0()
        .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
        .child(control)
}

impl ReportIssueDialog {
    /// The group under "Include": what goes along, and the disclosure of what is sent.
    fn include_group(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let line = cx.theme().colors().line;
        let this = cx.entity();
        let (on_versions, on_log) = (this.clone(), this);

        let versions = h_flex()
            .gap(px(10.))
            .child(
                Checkbox::new("report-versions", "Versions")
                    .toggle_state(self.include_versions)
                    .on_toggle(move |checked, _, cx| {
                        on_versions.update(cx, |this, cx| {
                            this.include_versions = checked;
                            cx.notify();
                        });
                    }),
            )
            .child(
                Label::new(self.versions.text())
                    .mono()
                    .size(LabelSize::XSmall)
                    .color(Color::Faint)
                    .truncate(),
            );
        let log = Checkbox::new("report-log", "Last 200 lines of the daemon log")
            .toggle_state(self.include_log)
            .on_toggle(move |checked, _, cx| {
                on_log.update(cx, |this, cx| {
                    this.include_log = checked;
                    this.error = None;
                    cx.notify();
                });
            });
        let details = Button::new("report-details", "Show what is sent")
            .variant(ButtonVariant::Ghost)
            .expanded(self.details_open)
            .debug_selector(|| "report-details".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.details_open = !this.details_open;
                cx.notify();
            }));

        let sent = self.details_open.then(|| {
            let text = self.draft(cx).link().disclosure(self.include_log);

            div()
                .id("report-sent")
                .role(Role::Group)
                .aria_label("What is sent")
                .debug_selector(|| "report-sent".to_owned())
                .max_h(px(160.))
                .overflow_y_scroll()
                .p(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(line)
                .child(Label::new(text).mono().size(LabelSize::XSmall))
        });

        v_flex()
            .id("report-include")
            .role(Role::Group)
            .aria_label("Include")
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(line)
            .child(
                Label::new("Include")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(versions)
            .child(log)
            .child(div().flex().child(details))
            .children(sent)
    }

    fn footer(cx: &mut Context<Self>) -> impl IntoElement + use<> {
        h_flex()
            .gap(px(8.))
            .items_center()
            .child(
                Label::new(REPOSITORY)
                    .size(LabelSize::Small)
                    .color(Color::Faint),
            )
            .child(
                Button::new("report-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .debug_selector(|| "report-cancel".to_owned())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ReportIssueEvent::Dismissed))),
            )
            .child(
                Button::new("report-open", "Open on GitHub")
                    .variant(ButtonVariant::Primary)
                    .end_slot(Icon::new(IconName::ExternalLink).color(Color::Inverse))
                    .debug_selector(|| "report-open".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.open(cx))),
            )
    }
}

impl Render for ReportIssueDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let (on_kind, on_dismiss) = (this.clone(), this);

        let selected = Kind::ALL
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or(0);
        let kinds = SegmentedControl::new("report-kind", "Kind", Kind::ALL.map(Kind::label))
            .selected(selected)
            .debug_selector(|| "report-kind".to_owned())
            .on_select(move |index, _, cx| {
                let Some(kind) = Kind::ALL.get(index).copied() else {
                    return;
                };

                on_kind.update(cx, |this, cx| this.set_kind(kind, cx));
            });
        let include = self.include_group(cx);
        let footer = Self::footer(cx);

        Dialog::new("report-issue-dialog", "Report an issue", &self.focus_handle)
            .width(px(700.))
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, |_, cx| cx.emit(ReportIssueEvent::Dismissed));
            })
            .child(kinds)
            .child(field("Title", self.title.clone()))
            .child(field(
                self.kind.description_label(),
                self.description.clone(),
            ))
            .children(self.kind.has_steps().then(|| {
                field("Steps to reproduce", self.steps.clone())
                    .debug_selector(|| "report-steps".to_owned())
            }))
            .child(include)
            .children(
                self.error
                    .clone()
                    .map(|message| Label::new(message).size(LabelSize::Small).color(Color::Del)),
            )
            .child(announce(
                "report-alert",
                LiveRegion::alert(
                    "report-alert-region",
                    self.error.clone().unwrap_or_default(),
                ),
            ))
            .footer(footer)
    }
}

impl Workspace {
    /// Opens the dialog, with the fields a failure screen filled in. The versions come
    /// from the machine and the daemon a moment later.
    pub(crate) fn open_report_issue(
        &mut self,
        prefill: Option<Prefill>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_report_with(prefill, daemon_log_path(), window, cx);
    }

    fn open_report_with(
        &mut self,
        prefill: Option<Prefill>,
        log_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Some(Overlay::ReportIssue { .. })) {
            return;
        }

        let view = cx.new(|cx| ReportIssueDialog::new(prefill, log_path, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let ReportIssueEvent::Dismissed = event;
            this.close_overlay(window, cx);
        });

        self.overlay = Some(Overlay::ReportIssue {
            view: view.clone(),
            _subscription: subscription,
        });
        cx.notify();

        let client = self.daemon.clone();
        let detecting = cx.background_spawn(async move { Versions::detect(client.as_ref()) });

        cx.spawn(async move |_, cx| {
            let versions = detecting.await;

            view.update(cx, |dialog, cx| dialog.set_versions(versions, cx));
        })
        .detach();
    }
}

/// The log of the daemon. Tests never read the real one.
fn daemon_log_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }

    Paths::new().ok().map(|paths| paths.log)
}

/// Starts the dialog from the Help menu and from the sidebar, in the main window.
pub(crate) fn register(cx: &mut App) {
    cx.on_action(|_: &ReportIssue, cx| cx.defer(open_in_main_window));
}

fn open_in_main_window(cx: &mut App) {
    let Some(workspace) = lifecycle::workspace(cx) else {
        return;
    };
    let Some(window) = lifecycle::reveal_main_window(cx) else {
        return;
    };

    let opened = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_report_issue(None, window, cx);
        });
    });

    if let Err(error) = opened {
        log::debug!("the main window closed before the report opened: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(kind: Kind) -> Draft {
        Draft {
            kind,
            title: "The button stays".to_owned(),
            description: "It stays after a commit".to_owned(),
            steps: "1. Approve\n2. Commit".to_owned(),
            versions: Some("Swarm 1.0.0 \u{b7} macOS 27.0.1".to_owned()),
        }
    }

    #[test]
    fn a_bug_link_names_its_form_and_fills_title_description_steps_and_versions() {
        let link = draft(Kind::Bug).link();

        assert_eq!(
            link.url,
            "https://github.com/DriesH/swarm/issues/new?template=bug.yml\
             &title=The%20button%20stays\
             &what-happened=It%20stays%20after%20a%20commit\
             &steps=1.%20Approve%0A2.%20Commit\
             &versions=Swarm%201.0.0%20%C2%B7%20macOS%2027.0.1"
        );
    }

    #[test]
    fn a_feature_request_uses_its_own_form_and_field_and_has_no_steps() {
        let url = draft(Kind::Feature).link().url;

        assert!(url.contains("template=feature.yml"));
        assert!(url.contains("&what-you-want="));
        assert!(!url.contains("steps="));
    }

    #[test]
    fn feedback_uses_its_own_form_and_field() {
        let url = draft(Kind::Feedback).link().url;

        assert!(url.contains("template=feedback.yml"));
        assert!(url.contains("&feedback="));
    }

    #[test]
    fn left_out_versions_and_empty_fields_are_not_in_the_address() {
        let mut left_out = draft(Kind::Bug);
        left_out.versions = None;
        left_out.steps = "  ".to_owned();

        let url = left_out.link().url;

        assert!(!url.contains("versions="));
        assert!(!url.contains("steps="));
    }

    #[test]
    fn encoding_keeps_unreserved_characters_and_escapes_the_rest_as_utf8() {
        assert_eq!(
            percent_encode("a-b_c.d~e f&g=h\u{e9}\n"),
            "a-b_c.d~e%20f%26g%3Dh%C3%A9%0A"
        );
    }

    #[test]
    fn a_long_description_is_cut_with_an_ellipsis_to_keep_the_address_short() {
        let mut long = draft(Kind::Bug);
        long.description = "word ".repeat(5000);

        let link = long.link();

        assert!(link.url.len() <= MAX_URL_LENGTH, "{}", link.url.len());
        assert!(link.fields[1].cut);
        assert!(link.fields[1].text().ends_with('\u{2026}'));
    }

    #[test]
    fn cutting_text_with_multi_byte_characters_still_fits() {
        let mut long = draft(Kind::Feedback);
        long.description = "\u{1f41d}".repeat(3000);

        assert!(long.link().url.len() <= MAX_URL_LENGTH);
    }

    #[test]
    fn only_the_longest_field_is_cut() {
        let mut long = draft(Kind::Bug);
        long.description = "x".repeat(8000);

        let link = long.link();

        let cut: Vec<&str> = link
            .fields
            .iter()
            .filter(|field| field.cut)
            .map(|field| field.label)
            .collect();
        assert_eq!(cut, ["What happened"]);
        assert!(
            link.fields
                .iter()
                .any(|field| field.label == "Versions" && !field.cut)
        );
    }

    #[test]
    fn the_disclosure_lists_the_fields_the_cut_and_the_log() {
        let mut long = draft(Kind::Bug);
        long.description = "x".repeat(8000);

        let text = long.link().disclosure(true);

        assert!(text.starts_with("Title\nThe button stays"));
        assert!(text.contains("Versions\nSwarm 1.0.0"));
        assert!(text.contains("Daemon log\nThe last 200 lines go to the clipboard"));
        assert!(text.ends_with("Cut to fit the address\nWhat happened"));
    }

    #[test]
    fn the_disclosure_says_nothing_of_the_log_when_it_stays_out() {
        let text = draft(Kind::Bug).link().disclosure(false);

        assert!(!text.contains("Daemon log"));
        assert!(!text.contains("Cut to fit"));
    }

    #[test]
    fn the_versions_text_skips_what_is_unknown() {
        let versions = Versions {
            swarm: "1.0.0".to_owned(),
            macos: Some("27.0.1".to_owned()),
            claude: None,
            codex: Some("0.166.0".to_owned()),
        };

        assert_eq!(
            versions.text(),
            "Swarm 1.0.0 \u{b7} macOS 27.0.1 \u{b7} Codex 0.166.0"
        );
    }

    #[test]
    fn the_last_lines_keep_the_end_of_the_text() {
        assert_eq!(last_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
    }

    #[test]
    fn the_log_tail_holds_the_last_200_lines_of_a_big_file() {
        let folder = std::env::temp_dir().join(format!("swarm-report-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");
        let path = folder.join("daemon.log");
        let lines: Vec<String> = (0..20_000).map(|number| format!("line {number}")).collect();
        std::fs::write(&path, lines.join("\n")).expect("log");

        let tail = read_log_tail(&path).expect("tail");

        let kept: Vec<&str> = tail.lines().collect();
        assert_eq!(kept.len(), LOG_LINES);
        assert_eq!(kept.last().copied(), Some("line 19999"));
        assert_eq!(kept.first().copied(), Some("line 19800"));
    }
}

#[cfg(test)]
mod dialog_tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::daemon::tests::{redraw, setup};

    fn open(
        prefill: Option<Prefill>,
        log: Option<PathBuf>,
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<ReportIssueDialog> {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_report_with(prefill, log, window, cx);
        });
        redraw(cx);

        workspace.read_with(cx, |workspace, _| match &workspace.overlay {
            Some(Overlay::ReportIssue { view, .. }) => view.clone(),
            _ => panic!("the report is not open"),
        })
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(selector);
        assert!(bounds.is_some(), "{selector} is not on screen");
        cx.simulate_click(
            bounds.map_or_else(Default::default, |bounds| bounds.center()),
            gpui::Modifiers::none(),
        );
    }

    fn log_file(name: &str, lines: usize) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("swarm-report-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");
        let path = folder.join("daemon.log");
        let text: Vec<String> = (0..lines).map(|number| format!("log {number}")).collect();
        std::fs::write(&path, text.join("\n")).expect("log");

        path
    }

    #[gpui::test]
    fn the_dialog_opens_on_a_bug_with_focus_in_the_title(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let view = open(None, None, &workspace, cx);

        let (kind, title) = view.read_with(cx, |dialog, cx| {
            (dialog.kind, dialog.title.focus_handle(cx))
        });
        assert_eq!(kind, Kind::Bug);
        assert!(cx.update(|window, _| title.is_focused(window)));
        assert!(cx.debug_bounds("report-steps").is_some());
    }

    #[gpui::test]
    fn only_a_bug_asks_for_steps_to_reproduce(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open(None, None, &workspace, cx);

        view.update(cx, |dialog, cx| dialog.choose(Kind::Feature, cx));
        redraw(cx);
        let feature = cx.debug_bounds("report-steps").is_some();
        view.update(cx, |dialog, cx| dialog.choose(Kind::Feedback, cx));
        redraw(cx);
        let feedback = cx.debug_bounds("report-steps").is_some();

        assert!(!feature);
        assert!(!feedback);
    }

    #[gpui::test]
    fn a_failure_screen_fills_in_the_title_and_the_error(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        let view = open(
            Some(Prefill::bug("Cleanup stopped", "git: busy")),
            None,
            &workspace,
            cx,
        );

        let draft = view.read_with(cx, ReportIssueDialog::draft);
        assert_eq!(draft.title, "Cleanup stopped");
        assert_eq!(draft.description, "git: busy");
        assert_eq!(draft.kind, Kind::Bug);
    }

    #[gpui::test]
    fn open_on_github_opens_the_filled_in_form_and_closes_the_dialog(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open(None, None, &workspace, cx);
        view.update(cx, |dialog, cx| {
            dialog.fill("Tabs jump", "They jump on click", cx);
        });
        redraw(cx);

        click("report-open", cx);
        redraw(cx);

        let url = cx.opened_url().expect("a page opened");
        assert!(url.starts_with("https://github.com/DriesH/swarm/issues/new?template=bug.yml"));
        assert!(url.contains("&title=Tabs%20jump"));
        assert!(url.contains("&what-happened=They%20jump%20on%20click"));
        assert!(url.contains("&versions=Swarm%20"));
        assert!(workspace.read_with(cx, |workspace, _| workspace.overlay.is_none()));
    }

    #[gpui::test]
    fn cancel_closes_without_opening_anything(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open(None, None, &workspace, cx);

        click("report-cancel", cx);
        redraw(cx);

        assert_eq!(cx.opened_url(), None);
        assert!(workspace.read_with(cx, |workspace, _| workspace.overlay.is_none()));
    }

    #[gpui::test]
    fn leaving_the_versions_out_keeps_them_out_of_the_address(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open(None, None, &workspace, cx);
        view.update(cx, |dialog, cx| {
            dialog.fill("Title", "Text", cx);
            dialog.include_versions(false);
        });

        click("report-open", cx);

        let url = cx.opened_url().expect("a page opened");
        assert!(!url.contains("versions="));
    }

    #[gpui::test]
    fn the_daemon_log_goes_to_the_clipboard_and_not_into_the_address(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let path = log_file("clipboard", 300);
        let view = open(None, Some(path), &workspace, cx);
        view.update(cx, |dialog, cx| {
            dialog.fill("Title", "Text", cx);
            dialog.include_log(true);
        });

        click("report-open", cx);

        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        let lines: Vec<&str> = clipboard.lines().collect();
        assert_eq!(lines.len(), LOG_LINES);
        assert_eq!(lines.first().copied(), Some("log 100"));
        assert_eq!(lines.last().copied(), Some("log 299"));
        assert!(
            !cx.opened_url()
                .expect("a page opened")
                .contains("log%20299")
        );
    }

    #[gpui::test]
    fn without_the_log_option_the_clipboard_stays_alone(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("mine".to_owned()));
        let path = log_file("untouched", 10);
        open(None, Some(path), &workspace, cx);

        click("report-open", cx);

        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("mine"));
    }

    #[gpui::test]
    fn a_log_that_cannot_be_read_opens_nothing_and_says_so(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let missing = std::env::temp_dir().join("swarm-report-missing/daemon.log");
        let view = open(None, Some(missing), &workspace, cx);
        view.update(cx, |dialog, _| dialog.include_log(true));

        click("report-open", cx);
        redraw(cx);

        assert_eq!(cx.opened_url(), None);
        assert_eq!(
            view.read_with(cx, |dialog, _| dialog.error_text().map(str::to_owned)),
            Some("Cannot read the daemon log".to_owned())
        );
        assert!(workspace.read_with(cx, |workspace, _| workspace.overlay.is_some()));
    }

    #[gpui::test]
    fn show_what_is_sent_lists_the_exact_text(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let view = open(None, None, &workspace, cx);
        view.update(cx, |dialog, cx| dialog.fill("Tabs jump", "On click", cx));
        redraw(cx);
        let closed = cx.debug_bounds("report-sent").is_none();

        click("report-details", cx);
        redraw(cx);

        assert!(closed);
        assert!(view.read_with(cx, |dialog, _| dialog.details_are_open()));
        assert!(cx.debug_bounds("report-sent").is_some());
    }

    #[gpui::test]
    fn the_palette_has_a_report_an_issue_command(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("report an issue");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(workspace.read_with(cx, |workspace, _| matches!(
            workspace.overlay,
            Some(Overlay::ReportIssue { .. })
        )));
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;

    fn form(kind: Kind) -> &'static str {
        match kind {
            Kind::Bug => include_str!("../../../.github/ISSUE_TEMPLATE/bug.yml"),
            Kind::Feature => include_str!("../../../.github/ISSUE_TEMPLATE/feature.yml"),
            Kind::Feedback => include_str!("../../../.github/ISSUE_TEMPLATE/feedback.yml"),
        }
    }

    #[test]
    fn every_field_the_app_fills_is_a_field_id_of_its_form() {
        for kind in Kind::ALL {
            let draft = Draft {
                kind,
                title: "t".to_owned(),
                description: "d".to_owned(),
                steps: "s".to_owned(),
                versions: Some("v".to_owned()),
            };

            for field in draft
                .link()
                .fields
                .iter()
                .filter(|field| field.key != "title")
            {
                assert!(
                    form(kind).contains(&format!("id: {}\n", field.key)),
                    "{} has no field {}",
                    kind.template(),
                    field.key
                );
            }
        }
    }

    #[test]
    fn each_form_sets_its_own_label() {
        assert!(form(Kind::Bug).contains("labels: [\"bug\"]"));
        assert!(form(Kind::Feature).contains("labels: [\"enhancement\"]"));
        assert!(form(Kind::Feedback).contains("labels: [\"feedback\"]"));
    }
}
