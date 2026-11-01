//! A workspace that shows the welcome screen against a fake daemon, for the tests of the
//! system check and the welcome screen.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use swarm_protocol::{Client, Event, Request, SystemCheck as Report, ToolCheck, ToolState};

use super::{SystemCheckView, Tool};
use crate::daemon::tests::{fake_daemon_replying, redraw, setup, use_projects, wait_until};
use crate::notifications::harness::Recorder;
use crate::notifications::{self, Delivery, Permission};
use crate::workspace::Workspace;

/// Every request the fake daemon got.
pub(crate) struct Seen {
    requests: flume::Receiver<Request>,
    all: RefCell<Vec<Request>>,
}

impl Seen {
    pub(crate) fn count(&self, matching: impl Fn(&Request) -> bool) -> usize {
        self.all.borrow_mut().extend(self.requests.try_iter());

        self.all
            .borrow()
            .iter()
            .filter(|request| matching(request))
            .count()
    }

    pub(crate) fn checks(&self) -> usize {
        self.count(|request| matches!(request, Request::SystemCheck { .. }))
    }

    /// The terminals the app asked for.
    pub(crate) fn created(&self) -> Vec<swarm_protocol::CreateTerminal> {
        self.all.borrow_mut().extend(self.requests.try_iter());

        self.all
            .borrow()
            .iter()
            .filter_map(|request| match request {
                Request::Create { terminal, .. } => Some(terminal.clone()),
                _ => None,
            })
            .collect()
    }
}

pub(crate) struct Fixture<'a> {
    pub workspace: Entity<Workspace>,
    pub cx: &'a mut VisualTestContext,
    pub seen: Seen,
    /// What the fake daemon answers to the next system check.
    pub report: Arc<Mutex<Report>>,
    pub recorder: Rc<Recorder>,
}

impl Fixture<'_> {
    pub(crate) fn check(&self) -> Entity<SystemCheckView> {
        self.workspace
            .read_with(self.cx, |workspace, cx| {
                workspace
                    .welcome
                    .as_ref()
                    .map(|welcome| welcome.read(cx).check().clone())
            })
            .expect("the welcome screen shows")
    }

    /// Draws until the daemon and macOS have answered.
    pub(crate) fn settle(&mut self) {
        let check = self.check();

        assert!(
            wait_until(self.cx, |cx| {
                redraw(cx);

                check.read_with(cx, |check, _| check.is_settled())
            }),
            "the system check never settled"
        );
    }

    /// Draws until `done` holds, as the daemon answers on another thread.
    pub(crate) fn wait(&mut self, done: impl Fn(&Seen, &VisualTestContext) -> bool) -> bool {
        let seen = &self.seen;

        wait_until(self.cx, |cx| {
            redraw(cx);

            done(seen, cx)
        })
    }

    pub(crate) fn click(&mut self, selector: &'static str) {
        let center = self.cx.debug_bounds(selector).map(|bounds| bounds.center());

        assert!(center.is_some(), "{selector} is not on screen");
        if let Some(center) = center {
            self.cx.simulate_click(center, Modifiers::none());
        }
        redraw(self.cx);
    }
}

pub(crate) fn tool_check(state: ToolState, version: Option<&str>, fix: &[&str]) -> ToolCheck {
    ToolCheck {
        state,
        version: version.map(str::to_owned),
        detail: None,
        fix: fix.iter().map(|part| (*part).to_owned()).collect(),
    }
}

/// A workspace with no project in a live app, so the welcome screen shows, and a daemon that
/// answers the system check with `report`. macOS answers `permission`.
pub(crate) fn welcome_fixture(
    report: Report,
    permission: Permission,
    cx: &mut TestAppContext,
) -> Fixture<'_> {
    welcome_fixture_at(None, report, permission, cx)
}

/// Like [`welcome_fixture`], with the settings read from and saved to `settings`.
pub(crate) fn welcome_fixture_at(
    settings: Option<std::path::PathBuf>,
    report: Report,
    permission: Permission,
    cx: &mut TestAppContext,
) -> Fixture<'_> {
    let report = Arc::new(Mutex::new(report));
    let (sent, requests) = flume::unbounded();
    let socket = fake_daemon_replying({
        let report = Arc::clone(&report);

        move |request| {
            sent.send(request.clone()).ok();

            match request {
                Request::SystemCheck { call } => Some(Event::SystemCheck {
                    call,
                    check: report.lock().expect("lock the report").clone(),
                }),
                _ => None,
            }
        }
    });

    let (workspace, cx) = setup(cx);
    let recorder = Recorder::new(permission);
    cx.update(|_, cx| {
        match settings {
            Some(path) => crate::settings::load(Some(path), cx),
            None => {
                crate::settings::state(cx);
            }
        }
        notifications::use_delivery(Rc::clone(&recorder) as Rc<dyn Delivery>, cx);
    });

    cx.executor().allow_parking();
    let client = Client::connect(&socket).expect("connect to the fake daemon");
    use_projects(&workspace, &[], cx);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.use_daemon(client, &[], window, cx);
    });
    redraw(cx);

    let mut fixture = Fixture {
        workspace,
        cx,
        seen: Seen {
            requests,
            all: RefCell::new(Vec::new()),
        },
        report,
        recorder,
    };
    fixture.settle();

    fixture
}

/// A report in which everything is ready but the tools in `broken`.
pub(crate) fn report_with(broken: &[(Tool, ToolCheck)]) -> Report {
    let mut report = crate::daemon::tests::ready_system_check();

    for (tool, check) in broken {
        match tool {
            Tool::Git => report.git = check.clone(),
            Tool::ClaudeCode => report.claude = check.clone(),
            Tool::Codex => report.codex = check.clone(),
            Tool::GithubCli => report.gh = check.clone(),
            Tool::GithubAccount => report.github_account = check.clone(),
            Tool::Notifications => {}
        }
    }

    report
}
