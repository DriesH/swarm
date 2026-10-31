//! The pull request panel: a new pull request, or the status of the one that exists.

use gpui::{AnyElement, Context, FontWeight, Role, SharedString, div, px};
use swarm_protocol::{
    CheckState, GithubCheck, GithubCheckState, GithubStatus, LocalCheck, MergeMethod, PrState,
    PullRequest, ReviewState,
};
use swarm_ui::TagTone;
use swarm_ui::prelude::*;

use crate::model::Task;
use crate::panels::{header, mono, pill};
use crate::pull_request::{
    ActionKind, Blocker, CreateState, CreateStatus, DraftWait, GithubSetup, PanelAction, PrForm,
    exit_value, format_duration, parse_logins, run_is_over,
};
use crate::workspace::{RightPanel, Workspace};

const METHODS: [(MergeMethod, &str); 3] = [
    (MergeMethod::Squash, "Squash"),
    (MergeMethod::Merge, "Merge"),
    (MergeMethod::Rebase, "Rebase"),
];

pub(crate) fn render(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let task = workspace.active_task();

    let Some(form) = workspace.pull_requests.form(task.id) else {
        return div().into_any_element();
    };

    let panel = if workspace.shows_new_pull_request(task.id) {
        new_panel(workspace, task, form, cx)
    } else {
        status_panel(workspace, task, form, cx)
    };

    panel
        .id("pull-request-panel")
        .role(Role::Group)
        .aria_label("Pull request")
        .when_some(workspace.pull_requests.panel_focus(), |panel, handle| {
            panel.track_focus(handle)
        })
        .size_full()
        .into_any_element()
}

fn panel_header(title: AnyElement, action: impl IntoElement, cx: &App) -> gpui::Div {
    header(cx)
        .h(px(46.))
        .child(
            Icon::new(IconName::PullRequest)
                .size(IconSize::Medium)
                .color(Color::Default),
        )
        .child(title)
        .child(div().flex_1())
        .child(action)
}

fn close_button(cx: &mut Context<Workspace>) -> IconButton {
    IconButton::new(
        "close-pull-request",
        IconName::Close,
        "Close pull request panel",
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.toggle_panel(RightPanel::PullRequest, cx);
    }))
}

fn footer(cx: &App) -> gpui::Div {
    let line = cx.theme().colors().line;

    v_flex()
        .flex_none()
        .gap(px(10.))
        .px(px(14.))
        .py(px(12.))
        .border_t_1()
        .border_color(line)
}

fn section_heading(text: &'static str) -> Label {
    Label::new(text)
        .size(LabelSize::Small)
        .weight(FontWeight::MEDIUM)
        .color(Color::Muted)
}

fn field_label(text: &'static str) -> Label {
    Label::new(text).size(LabelSize::Small).color(Color::Muted)
}

fn body() -> gpui::Stateful<gpui::Div> {
    v_flex()
        .id("pull-request-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(14.))
        .gap(px(14.))
}

fn branches_row(
    base: SharedString,
    branch: SharedString,
    extra: Option<String>,
    cx: &App,
) -> gpui::Div {
    h_flex()
        .gap(px(8.))
        .child(div().min_w_0().flex().child(pill(base, cx)))
        .child(mono("\u{2190}").color(Color::Muted))
        .child(div().min_w_0().flex().child(pill(branch, cx)))
        .child(div().flex_1())
        .children(extra.map(|text| mono(text).color(Color::Muted)))
}

/// The branch the pull request merges into.
pub(crate) fn base_of(workspace: &Workspace, task: &Task) -> SharedString {
    task.daemon
        .as_ref()
        .map(|daemon| daemon.pr_base.clone())
        .or_else(|| {
            workspace
                .data
                .project(task.project)
                .map(|project| project.base_branch.clone())
        })
        .unwrap_or_default()
}

fn status_label(text: impl Into<SharedString>, color: Color) -> gpui::Stateful<gpui::Div> {
    let text = text.into();

    div()
        .id("pull-request-status")
        .role(Role::Status)
        .aria_label(text.clone())
        .min_w_0()
        .child(
            Label::new(text)
                .size(LabelSize::Small)
                .color(color)
                .truncate(),
        )
}

// A new pull request

fn new_panel(
    workspace: &Workspace,
    task: &Task,
    form: &PrForm,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let creating = form.create == CreateStatus::Creating;
    let base = base_of(workspace, task);
    let files = workspace
        .data
        .task_diff(task.id, swarm_protocol::ReviewSource::Merged)
        .map(|diff| crate::review_model::plural(diff.stat.files, "file", "files"));

    let github = workspace.data.github_of(task.id);
    let setup = workspace.pull_requests.setups.get(&task.project);
    let state = workspace.create_state(task.id, cx);

    let github_section = github_section(github, setup, workspace.daemon.is_some());

    v_flex()
        .child(panel_header(
            Label::new("New pull request")
                .weight(FontWeight::MEDIUM)
                .into_any_element(),
            close_button(cx),
            cx,
        ))
        .child(
            body()
                .children(github_section)
                .child(branches_row(base, task.branch.clone(), files, cx))
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(field_label("Title"))
                        .child(form.title.clone()),
                )
                .child(description_field(form, cx))
                .children(local_checks(workspace, task, form, cx))
                .child(options(form, creating, cx)),
        )
        .child(new_footer(form, state, creating, cx))
}

fn github_section(
    github: Option<&GithubStatus>,
    setup: Option<&GithubSetup>,
    connected: bool,
) -> Option<AnyElement> {
    let set_up = || {
        Button::new("set-up-github", "Set up GitHub")
            .variant(ButtonVariant::Secondary)
            .debug_selector(|| "set-up-github".to_owned())
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(crate::actions::SetUpGithub), cx);
            })
    };

    let row = |content: AnyElement, action: Option<Button>| {
        h_flex()
            .id("github-setup")
            .role(Role::Group)
            .aria_label("GitHub")
            .gap(px(8.))
            .min_h(px(30.))
            .child(div().flex_1().min_w_0().child(content))
            .children(action)
            .into_any_element()
    };

    if let Some(setup) = setup {
        return Some(match setup {
            GithubSetup::Running(step) => row(
                h_flex()
                    .gap(px(8.))
                    .child(StatusRing::new(AgentStatus::Working).size(px(9.)))
                    .child(status_label(step.running(), Color::Default))
                    .into_any_element(),
                None,
            ),
            GithubSetup::Failed { step, reason, .. } => row(
                status_label(format!("{} \u{b7} {reason}", step.failed()), Color::Del)
                    .into_any_element(),
                Some(
                    Button::new("try-again", "Try again")
                        .variant(ButtonVariant::Secondary)
                        .debug_selector(|| "github-try-again".to_owned())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::actions::SetUpGithub), cx);
                        }),
                ),
            ),
        });
    }

    match github {
        None if connected => Some(row(
            status_label("Checking GitHub\u{2026}", Color::Faint).into_any_element(),
            None,
        )),
        Some(status) if status.repository.is_none() => Some(row(
            status_label("No GitHub repository", Color::Faint).into_any_element(),
            None,
        )),
        Some(status) if status.gh.is_none() => Some(row(
            status_label("GitHub CLI not installed", Color::Muted).into_any_element(),
            Some(set_up()),
        )),
        Some(status) if !status.signed_in => Some(row(
            status_label("Not signed in to GitHub", Color::Muted).into_any_element(),
            Some(set_up()),
        )),
        None | Some(_) => None,
    }
}

fn description_field(form: &PrForm, cx: &mut Context<Workspace>) -> gpui::Div {
    let note: Option<AnyElement> = if form.newer.is_some() {
        Some(
            Button::new("new-draft", "New draft")
                .variant(ButtonVariant::Ghost)
                .icon(IconName::Bee)
                .debug_selector(|| "new-draft".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.use_new_draft(cx)))
                .into_any_element(),
        )
    } else {
        match &form.draft_wait {
            DraftWait::Waiting => {
                Some(status_label("Drafting\u{2026}", Color::Faint).into_any_element())
            }
            DraftWait::Refused(reason) => {
                Some(status_label(reason.clone(), Color::Faint).into_any_element())
            }
            DraftWait::Idle if form.drafted() => Some(
                Label::new("Drafted by orchestrator")
                    .size(LabelSize::Small)
                    .color(Color::Faint)
                    .into_any_element(),
            ),
            DraftWait::Idle => None,
        }
    };

    v_flex()
        .gap(px(6.))
        .child(
            h_flex()
                .gap(px(8.))
                .min_h(px(28.))
                .child(field_label("Description"))
                .child(div().flex_1())
                .children(note),
        )
        .child(form.description.clone())
}

fn local_check_value(check: &LocalCheck) -> Option<String> {
    match check.state {
        CheckState::Failed => check.exit.as_ref().map(exit_value),
        CheckState::Passed => check.duration_ms.map(format_duration),
        CheckState::Waiting | CheckState::Running | CheckState::Skipped | CheckState::Stopped => {
            None
        }
    }
}

fn local_status(state: CheckState) -> CheckStatus {
    match state {
        CheckState::Waiting => CheckStatus::Waiting,
        CheckState::Running => CheckStatus::Running,
        CheckState::Passed => CheckStatus::Passed,
        CheckState::Failed => CheckStatus::Failed,
        CheckState::Skipped => CheckStatus::Skipped,
        CheckState::Stopped => CheckStatus::Cancelled,
    }
}

/// The local checks, once the project configures some. The run, its freshness and the
/// button to start it.
fn local_checks(
    workspace: &Workspace,
    task: &Task,
    form: &PrForm,
    cx: &mut Context<Workspace>,
) -> Option<gpui::Stateful<gpui::Div>> {
    if !workspace.has_local_checks(task.id) {
        return None;
    }

    let checks = workspace.data.checks_of(task.id);
    let config = checks.and_then(|checks| checks.config.as_ref());
    let run = checks.and_then(|checks| checks.run.as_ref());
    let head = workspace.data.merged_head(task.id);

    let over = run.is_none_or(run_is_over);
    let stale = run.is_some_and(|run| head.is_some_and(|head| head != run.commit));
    let unclean = run.is_some_and(|run| run_is_over(run) && !run.clean);
    let hook = match config {
        Some(config) => config.pre_push_hook.as_ref(),
        None => run.and_then(|run| run.pre_push_hook.as_ref()),
    };
    let creating = form.create == CreateStatus::Creating;

    let label = if run.is_some() {
        "Run again"
    } else {
        "Run checks"
    };
    let run_button = Button::new("run-checks", label)
        .variant(ButtonVariant::Secondary)
        .debug_selector(|| "run-checks".to_owned())
        .disabled(!over || form.checks_busy || creating)
        .on_click(cx.listener(|this, _, _, cx| this.run_checks(cx)));

    let rows = run.map(|run| {
        run.checks.iter().enumerate().map(|(index, check)| {
            let mut row = CheckRow::new(
                ("local-check", index),
                check.command.clone(),
                local_status(check.state),
            )
            .mono()
            .debug_selector(move || format!("local-check-{index}"));

            if check.state == CheckState::Stopped {
                row = row.status_label("Stopped");
            }
            if let Some(value) = local_check_value(check) {
                row = row.value(value);
            }

            if check.state == CheckState::Failed
                && let Some(terminal) = check.terminal
            {
                let shown = workspace.tile_of_daemon_terminal(terminal).is_some();

                row = row.action(
                    Button::new(("check-output", index), "Show output")
                        .variant(ButtonVariant::Ghost)
                        .debug_selector(move || format!("check-output-{index}"))
                        .disabled(!shown)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_check_output(terminal, cx);
                        })),
                );
            }

            row.into_any_element()
        })
    });

    let empty = match run {
        None => Some("Not run"),
        Some(run) if run.checks.is_empty() => Some("No checks"),
        Some(_) => None,
    };

    Some(
        v_flex()
            .id("local-checks")
            .role(Role::List)
            .aria_label("Local checks")
            .gap(px(4.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .min_h(px(30.))
                    .child(field_label("Local checks"))
                    .child(div().flex_1())
                    .child(run_button),
            )
            .children((stale || unclean).then(|| {
                h_flex()
                    .gap(px(6.))
                    .children(stale.then(|| Tag::new("Stale").tone(TagTone::Warn)))
                    .children(unclean.then(|| Tag::new("Uncommitted changes").tone(TagTone::Warn)))
            }))
            .children(rows.into_iter().flatten())
            .children(empty.map(|text| mono(text).color(Color::Faint)))
            .children(hook.map(|hook| {
                div()
                    .id("pre-push-hook")
                    .role(Role::ListItem)
                    .aria_label(format!("pre-push hook: {hook}"))
                    .child(mono(format!("pre-push hook: {hook}")).color(Color::Faint))
            }))
            .children(form.checks_error.clone().map(|message| {
                div()
                    .id("checks-error")
                    .role(Role::Alert)
                    .aria_label(message.clone())
                    .child(Label::new(message).size(LabelSize::Small).color(Color::Del))
            })),
    )
}

fn options(form: &PrForm, creating: bool, cx: &mut Context<Workspace>) -> gpui::Div {
    let workspace = cx.entity();
    let on_draft = workspace.clone();

    v_flex()
        .gap(px(8.))
        .child(
            Checkbox::new("pr-open-as-draft", "Open as draft")
                .toggle_state(form.as_draft)
                .disabled(creating)
                .on_toggle(move |value, _, cx| {
                    on_draft.update(cx, |this, cx| this.set_as_draft(value, cx));
                }),
        )
        .child(
            Checkbox::new("pr-close-after-merge", "Close the task after merge")
                .toggle_state(form.close_after_merge)
                .disabled(creating)
                .on_toggle(move |value, _, cx| {
                    workspace.update(cx, |this, cx| this.set_close_after_merge(value, cx));
                }),
        )
}

fn new_footer(
    form: &PrForm,
    state: CreateState,
    creating: bool,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let (label, enabled, note) = match state {
        CreateState::Ready => (create_label(form), true, None),
        CreateState::Anyway(problem) => ("Create PR anyway", true, Some((problem, Color::Warn))),
        CreateState::Blocked(Blocker::Creating) => (create_label(form), false, None),
        CreateState::Blocked(problem) => (create_label(form), false, Some((problem, Color::Muted))),
    };

    let note = if creating {
        Some(status_label(Blocker::Creating.label(), Color::Default))
    } else {
        note.map(|(problem, color)| status_label(problem.label(), color))
    };

    let error = match &form.create {
        CreateStatus::Failed(message) => Some(
            OutputBlock::new("create-error", "Pull request error", message.clone())
                .debug_selector(|| "create-error".to_owned())
                .tone(Color::Del),
        ),
        CreateStatus::Idle | CreateStatus::Creating => None,
    };

    footer(cx).children(error).child(
        h_flex()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().children(note))
            .child(
                Button::new("create-pr", label)
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Medium)
                    .debug_selector(|| "create-pr-button".to_owned())
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.create_pull_request(window, cx);
                    })),
            ),
    )
}

fn create_label(form: &PrForm) -> &'static str {
    if form.as_draft {
        "Create draft PR"
    } else {
        "Create PR"
    }
}

// The status of a pull request

fn status_panel(
    workspace: &Workspace,
    task: &Task,
    form: &PrForm,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let status = workspace.data.pull_request_status(task.id);
    let number = task.pull_request.as_ref().map_or(0, |link| link.number);
    let base = base_of(workspace, task);
    let title = status.map_or_else(
        || form.title.read(cx).text(),
        |status| SharedString::from(status.title.clone()),
    );
    let open = status.is_none_or(|status| status.state == PrState::Open);
    let closed = status.is_some_and(|status| status.state == PrState::Closed);
    let push = workspace
        .update_offered(task.id)
        .then(|| workspace.update_state(task.id));

    let open_button = Button::new("open-on-github", "Open on GitHub")
        .variant(ButtonVariant::Ghost)
        .icon(IconName::ExternalLink)
        .debug_selector(|| "open-on-github".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.open_pull_request_url(cx)));

    let tag = status.and_then(|status| match status.state {
        PrState::Merged => Some(Tag::new("Merged").tone(TagTone::Ok)),
        PrState::Closed => Some(Tag::new("Closed").tone(TagTone::Del)),
        PrState::Open if status.draft => Some(Tag::new("Draft")),
        PrState::Open => None,
    });

    let header = panel_header(
        h_flex()
            .gap(px(8.))
            .child(
                Label::new(format!("#{number}"))
                    .mono()
                    .size(LabelSize::Small)
                    .weight(FontWeight::MEDIUM),
            )
            .children(tag)
            .into_any_element(),
        open_button,
        cx,
    );

    let mut content = body()
        .gap(px(18.))
        .child(
            v_flex()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(19.6))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(branches_row(base, task.branch.clone(), None, cx)),
        )
        .child(github_checks(status));

    if push.is_some() {
        content = content.children(local_checks(workspace, task, form, cx));
    }

    content = content.child(reviewers(status, form, cx));

    if open {
        content = content.child(merge_options(task, form, cx));
    }

    if let PanelAction::Failed(message) = &form.action {
        content = content.child(
            OutputBlock::new("pull-request-error", "Pull request error", message.clone())
                .debug_selector(|| "pull-request-error".to_owned())
                .tone(Color::Del),
        );
    }

    let mut panel = v_flex().child(header).child(content);

    if open {
        panel = panel.child(status_footer(status, form, push, cx));
    } else if closed {
        panel = panel.child(closed_footer(cx));
    }

    panel
}

fn github_status(state: GithubCheckState) -> CheckStatus {
    match state {
        GithubCheckState::Queued => CheckStatus::Waiting,
        GithubCheckState::Running => CheckStatus::Running,
        GithubCheckState::Passed => CheckStatus::Passed,
        GithubCheckState::Failed => CheckStatus::Failed,
        GithubCheckState::Skipped => CheckStatus::Skipped,
        GithubCheckState::Cancelled => CheckStatus::Cancelled,
    }
}

fn github_row(index: usize, check: &GithubCheck) -> CheckRow {
    let mut row = CheckRow::new(
        ("github-check", index),
        check.name.clone(),
        github_status(check.state),
    )
    .debug_selector(move || format!("github-check-{index}"));

    if check.state == GithubCheckState::Queued {
        row = row.status_label("Queued");
    }
    if let Some(seconds) = check.duration_secs {
        row = row.value(format_duration(u64::from(seconds) * 1_000));
    }

    row
}

fn github_checks(status: Option<&PullRequest>) -> gpui::Stateful<gpui::Div> {
    let checks = status.map_or(&[][..], |status| status.checks.as_slice());
    let passed = checks
        .iter()
        .filter(|check| check.state == GithubCheckState::Passed)
        .count();

    v_flex()
        .id("github-checks")
        .role(Role::List)
        .aria_label("GitHub checks")
        .gap(px(2.))
        .child(
            h_flex()
                .mb(px(4.))
                .child(section_heading("GitHub checks"))
                .child(div().flex_1())
                .child(mono(format!("{passed} / {}", checks.len())).color(Color::Faint)),
        )
        .children(
            checks
                .iter()
                .enumerate()
                .map(|(index, check)| github_row(index, check)),
        )
        .children(
            checks
                .is_empty()
                .then(|| mono("No checks").color(Color::Faint)),
        )
}

fn review_text(state: ReviewState) -> (&'static str, Color) {
    match state {
        ReviewState::Requested => ("Requested", Color::Faint),
        ReviewState::Approved => ("Approved", Color::Ok),
        ReviewState::ChangesRequested => ("Changes requested", Color::Warn),
        ReviewState::Commented => ("Commented", Color::Muted),
    }
}

fn reviewers(
    status: Option<&PullRequest>,
    form: &PrForm,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let list = status.map_or(&[][..], |status| status.reviewers.as_slice());
    let asking = !parse_logins(&form.reviewers.read(cx).text()).is_empty();

    v_flex()
        .gap(px(6.))
        .child(section_heading("Reviewers"))
        .child(
            v_flex()
                .id("reviewers")
                .role(Role::List)
                .aria_label("Reviewers")
                .children(list.iter().enumerate().map(|(index, reviewer)| {
                    let (text, color) = review_text(reviewer.state);

                    h_flex()
                        .id(("reviewer", index))
                        .role(Role::ListItem)
                        .aria_label(format!("{}, {text}", reviewer.login))
                        .min_h(px(28.))
                        .gap(px(8.))
                        .child(
                            div().flex_1().min_w_0().flex().child(
                                Label::new(reviewer.login.clone())
                                    .mono()
                                    .size(LabelSize::Small)
                                    .truncate(),
                            ),
                        )
                        .child(Label::new(text).size(LabelSize::Small).color(color))
                })),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(form.reviewers.clone()))
                .child(
                    Button::new("request-review", "Request review")
                        .variant(ButtonVariant::Secondary)
                        .debug_selector(|| "request-review".to_owned())
                        .disabled(!asking || form.busy())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let task = this.active_task;
                            this.request_pr_review(task, cx);
                        })),
                ),
        )
}

fn merge_options(task: &Task, form: &PrForm, cx: &mut Context<Workspace>) -> gpui::Div {
    let workspace = cx.entity();
    let on_method = workspace.clone();
    let selected = METHODS
        .iter()
        .position(|(method, _)| *method == form.method)
        .unwrap_or(0);

    v_flex()
        .gap(px(8.))
        .child(section_heading("After merge"))
        .child(
            Checkbox::new("pr-close-after-merge", "Close the task after merge")
                .toggle_state(task.close_after_merge)
                .on_toggle(move |value, _, cx| {
                    workspace.update(cx, |this, cx| this.set_close_after_merge(value, cx));
                }),
        )
        .child(
            v_flex()
                .gap(px(6.))
                .child(field_label("Merge method"))
                .child(
                    Dropdown::new(
                        "merge-method",
                        "Merge method",
                        METHODS
                            .iter()
                            .map(|(_, label)| SharedString::new_static(label))
                            .collect(),
                        selected,
                    )
                    .full_width()
                    .on_change(move |index, _, cx| {
                        if let Some((method, _)) = METHODS.get(index) {
                            on_method.update(cx, |this, cx| this.choose_merge_method(*method, cx));
                        }
                    }),
                ),
        )
}

fn status_footer(
    status: Option<&PullRequest>,
    form: &PrForm,
    push: Option<CreateState>,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let draft = status.is_some_and(|status| status.draft);
    let mergeable = status.is_some_and(|status| status.mergeable);
    let busy = form.busy();
    let merging = form.action == PanelAction::Busy(ActionKind::Merge);
    let pushing = form.create == CreateStatus::Creating;

    let ready = draft.then(|| {
        div().flex_1().child(
            Button::new("ready-for-review", "Ready for review")
                .size(ButtonSize::Medium)
                .full_width()
                .debug_selector(|| "ready-for-review".to_owned())
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.mark_pr_ready(cx))),
        )
    });

    let merge = Button::new("merge", if merging { "Merging\u{2026}" } else { "Merge" })
        .variant(ButtonVariant::Primary)
        .size(ButtonSize::Medium)
        .full_width()
        .debug_selector(|| "merge".to_owned())
        // Unpushed commits would stay out of the merge, so they go first.
        .disabled(!mergeable || busy || push.is_some())
        .on_click(cx.listener(|this, _, _, cx| this.merge_pull_request(cx)));

    let queued = (form.action == PanelAction::MergeQueued).then(|| {
        status_label("Merge queued", Color::Muted).debug_selector(|| "merge-queued".to_owned())
    });

    let error = match &form.create {
        CreateStatus::Failed(message) => Some(
            OutputBlock::new("push-error", "Push error", message.clone())
                .debug_selector(|| "push-error".to_owned())
                .tone(Color::Del),
        ),
        CreateStatus::Idle | CreateStatus::Creating => None,
    };

    footer(cx)
        .children(error)
        .children(push.map(|state| push_row(state, pushing, busy, cx)))
        .children(queued)
        .child(
            h_flex()
                .gap(px(8.))
                .children(ready)
                .child(div().flex_1().child(merge)),
        )
}

/// New commits on the task branch, and the button that sends them to the pull request.
fn push_row(
    state: CreateState,
    pushing: bool,
    busy: bool,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let (label, enabled, note) = match state {
        CreateState::Ready => ("Push changes", true, None),
        CreateState::Anyway(problem) => ("Push anyway", true, Some((problem, Color::Warn))),
        CreateState::Blocked(problem) => ("Push changes", false, Some((problem, Color::Muted))),
    };
    let label = if pushing { "Pushing\u{2026}" } else { label };

    h_flex()
        .gap(px(8.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    status_label("New commits", Color::Warn)
                        .debug_selector(|| "new-commits".to_owned()),
                )
                .children(note.map(|(problem, color)| status_label(problem.label(), color))),
        )
        .child(
            Button::new("push-changes", label)
                .variant(ButtonVariant::Secondary)
                .debug_selector(|| "push-changes".to_owned())
                .disabled(!enabled || pushing || busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.update_pull_request(window, cx);
                })),
        )
}

/// A closed pull request can be replaced by a new one from the same branch.
fn closed_footer(cx: &mut Context<Workspace>) -> gpui::Div {
    footer(cx).child(
        Button::new("create-pr-again", "Create PR")
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Medium)
            .full_width()
            .debug_selector(|| "create-pr-again".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.replace_closed_pull_request(cx))),
    )
}
