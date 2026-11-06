//! The Field guide: a creature that visits the title bar while agents work, the catch, and
//! the guide that lists all 20 creatures.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Bounds, Context, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, Pixels,
    Point, Role, ScrollHandle, SharedString, Subscription, Task, Window, canvas, div, fill, point,
    px, relative, size,
};
use swarm_creatures::{Frame, Motion, PAPER, Rgb, SIDE, SILHOUETTE};
use swarm_protocol::creatures::{CREATURES, Creature, Tier, random_seed};
use swarm_protocol::{Catch, Client};
use swarm_ui::motion::reduce_motion;
use swarm_ui::prelude::*;
use swarm_ui::roving::RovingFocus;
use swarm_ui::styles::focus_ring;

use crate::actions::OpenFieldGuide;
use crate::announce::announce;
use crate::lifecycle;
use crate::palette::Overlay;
use crate::settings;
use crate::workspace::Workspace;

/// How long a creature stays in the title bar, and how long its flight takes.
pub(crate) const VISIT: Duration = Duration::from_secs(45);

/// At most this many creatures come in 24 hours.
pub(crate) const MAX_PER_DAY: usize = 4;

/// Two creatures come at least this far apart.
pub(crate) const MIN_GAP: Duration = Duration::from_mins(45);

const DAY: Duration = Duration::from_hours(24);

/// Once agents work and a creature may come, it comes after a random wait in this range.
const WAIT: (Duration, Duration) = (Duration::from_mins(1), Duration::from_mins(30));

/// How often the app looks if a creature should come.
const TICK: Duration = Duration::from_secs(15);

/// The most one tick takes off the wait. A late tick still counts, a night of sleep does not.
const MOST_PER_TICK: Duration = Duration::from_secs(30);

/// Each sprite pixel in the title bar is half a logical pixel: 32 pixels for the sprite,
/// which fits the 40 pixel bar and is one device pixel on a Retina screen. Below that, the
/// frame of every second pixel is drawn.
const BAR_UNIT: f32 = 0.5;

/// Space above the sprite in the title bar.
const BAR_TOP: f32 = 4.;

/// How long a wing frame shows, and a crawl frame.
const WING_FRAME: f32 = 0.14;
const CRAWL_FRAME: f32 = 0.18;

/// A flier speeds up and slows down by this part of the bar, this many seconds a round. The
/// wave never beats the pace across, so it hovers but never turns back.
const WANDER: f32 = 0.025;
const WANDER_PERIOD: f32 = 9.;

/// How often the guide changes frames.
const GUIDE_FRAME: Duration = Duration::from_millis(140);

/// The guide shows this many creatures in a row.
const COLUMNS: usize = 5;

/// A sprite pixel in a card of the guide is this many logical pixels.
const CARD_UNIT: f32 = 2.;

/// The cards in the guide, by the shown order. Creatures come grouped by tier.
const TIERS: [Tier; 3] = [Tier::Common, Tier::Rare, Tier::UltraRare];

// ---------------------------------------------------------------------------------------
// The rules for when a creature comes
// ---------------------------------------------------------------------------------------

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// When creatures came and how long until the next one. Times are milliseconds since the
/// Unix epoch, so a test sets the clock.
#[derive(Debug, Default)]
pub(crate) struct Schedule {
    starts: Vec<u64>,
    /// The wait left before the next creature.
    left: Option<u64>,
    /// The last tick that counted, while a creature may come.
    counted: Option<u64>,
}

impl Schedule {
    /// True when the daily cap and the gap allow a creature now.
    fn open(&self, now: u64) -> bool {
        let last = self.starts.last().copied();

        self.starts.len() < MAX_PER_DAY
            && last.is_none_or(|last| now.saturating_sub(last) >= millis(MIN_GAP))
    }

    /// Takes the start times the daemon kept, so the cap and the gap hold after a restart.
    pub(crate) fn remember(&mut self, starts: &[u64]) {
        self.starts.extend(starts);
        self.starts.sort_unstable();
        self.starts.dedup();
    }

    /// The wait before a creature comes, from `random`.
    fn wait(random: u64) -> u64 {
        let (least, most) = (millis(WAIT.0), millis(WAIT.1));

        least + random % (most - least + 1)
    }

    /// Looks if a creature comes now. `eligible` says agents work, the eggs are on and the
    /// window is in front. The wait runs only while `eligible` is true: it pauses when it
    /// goes false and goes on from there. Returns true once, when the wait is over.
    pub(crate) fn tick(&mut self, now: u64, eligible: bool, random: u64) -> bool {
        self.starts
            .retain(|start| now.saturating_sub(*start) < millis(DAY));

        if !eligible || !self.open(now) {
            self.counted = None;

            return false;
        }

        let step = self
            .counted
            .map_or(0, |at| now.saturating_sub(at).min(millis(MOST_PER_TICK)));
        self.counted = Some(now);

        let left = self.left.get_or_insert_with(|| Self::wait(random));
        *left = left.saturating_sub(step);
        if *left > 0 {
            return false;
        }

        self.left = None;
        self.counted = None;
        self.starts.push(now);

        true
    }
}

/// The tier a roll of 0 to 99 picks: 70 % common, 25 % rare, 5 % ultra rare.
pub(crate) fn tier_for(roll: u64) -> Tier {
    match roll % 100 {
        0..70 => Tier::Common,
        70..95 => Tier::Rare,
        _ => Tier::UltraRare,
    }
}

/// The creature, as a place in the list, that `random` picks: a tier by weight, then one of
/// its creatures.
pub(crate) fn pick_creature(random: u64) -> usize {
    let tier = tier_for(random);
    let of_tier: Vec<usize> = CREATURES
        .iter()
        .enumerate()
        .filter(|(_, creature)| creature.tier == tier)
        .map(|(index, _)| index)
        .collect();

    let choice = usize::try_from(random / 100).unwrap_or(0) % of_tier.len().max(1);

    of_tier.get(choice).copied().unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------

/// A small count as a float. A sprite is 64 pixels across.
fn float(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn paint_color(rgb: Rgb) -> Hsla {
    let [red, green, blue] = rgb;
    let packed = (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue);

    Hsla::from(gpui::rgb(packed))
}

/// The quads of one frame, each with its color. `origin` is the top left corner and each
/// sprite pixel is `unit` logical pixels. The origin and every edge are snapped to a device
/// pixel, so the art stays sharp and has no seams. When a sprite pixel is smaller than a
/// device pixel, the frame of every second pixel is drawn at twice the unit: no run drops.
fn sprite_quads(
    frame: &Frame,
    origin: Point<Pixels>,
    unit: f32,
    factor: f32,
) -> impl Iterator<Item = (Bounds<Pixels>, Rgb)> + '_ {
    let (frame, unit) = if unit * factor < 1. {
        (frame.halved(), unit * 2.)
    } else {
        (frame, unit)
    };
    let snap = move |value: f32| (value * factor).round() / factor;
    let (left, top) = (snap(f32::from(origin.x)), snap(f32::from(origin.y)));

    (0..frame.side()).flat_map(move |row| {
        let upper = snap(top + float(row) * unit);
        let lower = snap(top + float(row + 1) * unit);

        frame.runs(row).iter().map(move |run| {
            let start = snap(left + float(run.column) * unit);
            let end = snap(left + float(run.column + run.length) * unit);
            let bounds = Bounds::new(
                point(px(start), px(upper)),
                size(px(end - start), px(lower - upper)),
            );

            (bounds, run.color)
        })
    })
}

/// Paints one frame. With `tint` every pixel has that color: a silhouette.
fn paint_frame(
    window: &mut Window,
    origin: Point<Pixels>,
    unit: f32,
    frame: &Frame,
    tint: Option<Rgb>,
) {
    for (bounds, color) in sprite_quads(frame, origin, unit, window.scale_factor()) {
        window.paint_quad(fill(bounds, paint_color(tint.unwrap_or(color))));
    }
}

/// One frame of a creature, drawn with quads. It is decoration: the card or the button
/// around it carries the name.
#[derive(IntoElement)]
struct Art {
    frame: &'static Frame,
    unit: f32,
    silhouette: bool,
}

impl Art {
    fn new(frame: &'static Frame, unit: f32) -> Self {
        Self {
            frame,
            unit,
            silhouette: false,
        }
    }

    fn silhouette(mut self, silhouette: bool) -> Self {
        self.silhouette = silhouette;
        self
    }

    /// The width and the height of the art.
    fn side(unit: f32) -> f32 {
        float(SIDE) * unit
    }
}

impl RenderOnce for Art {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            frame,
            unit,
            silhouette,
        } = self;
        let tint = silhouette.then_some(SILHOUETTE);

        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| paint_frame(window, bounds.origin, unit, frame, tint),
        )
        .size(px(Self::side(unit)))
        .flex_none()
    }
}

/// The frame at `elapsed` seconds when each frame shows for `period` seconds.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a frame number is a small whole number"
)]
fn frame_at(elapsed: f32, period: f32, frames: usize) -> usize {
    (elapsed / period).max(0.) as usize % frames.max(1)
}

/// The frame of a creature for `motion`, `elapsed` seconds into it.
fn frame_of(index: usize, motion: Motion, elapsed: f32) -> Option<&'static Frame> {
    let sprite = swarm_creatures::sprite(index)?;
    let motion = sprite.motion(motion);
    let frames = sprite.frames(motion);
    let period = if motion == Motion::Crawl {
        CRAWL_FRAME
    } else {
        WING_FRAME
    };

    frames.get(frame_at(elapsed, period, frames.len()))
}

// ---------------------------------------------------------------------------------------
// The creature in the title bar
// ---------------------------------------------------------------------------------------

/// Where the creature is at `delta` of its visit: 1 at the right end of the bar, 0 at the
/// left end.
fn place(delta: f32, motion: Motion) -> f32 {
    let rest = 1. - delta;
    if motion != Motion::Fly {
        return rest;
    }

    let elapsed = delta * VISIT.as_secs_f32();
    let wander = WANDER * (elapsed * std::f32::consts::TAU / WANDER_PERIOD).sin();

    (rest - wander).clamp(0., 1.)
}

/// What the creature tells the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisitEvent {
    Clicked,
}

/// A creature in the empty part of the title bar. It has its own view, so its animation
/// redraws only itself.
pub(crate) struct Visit {
    creature: usize,
    motion: Motion,
    /// Reduce Motion: the creature sits still.
    still: bool,
    serial: usize,
}

impl EventEmitter<VisitEvent> for Visit {}

impl Visit {
    fn creature(&self) -> Option<&'static Creature> {
        CREATURES.get(self.creature)
    }
}

impl Render for Visit {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creature) = self.creature() else {
            return div().into_any_element();
        };
        let (index, serial) = (self.creature, self.serial);
        let motion =
            swarm_creatures::sprite(index).map_or(self.motion, |sprite| sprite.motion(self.motion));
        let side = Art::side(BAR_UNIT);

        let mover = div()
            .id(("creature", serial))
            .role(Role::Button)
            .aria_label(format!("Catch the {}", creature.english))
            .debug_selector(|| "creature".to_owned())
            .absolute()
            .top(px(BAR_TOP))
            .size(px(side))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                // The title bar under the creature must not start dragging the window.
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(cx.listener(|_, _, _, cx| cx.emit(VisitEvent::Clicked)));

        let moving = if self.still {
            let art = frame_of(index, Motion::Still, 0.).map(|frame| Art::new(frame, BAR_UNIT));

            mover
                .left(relative(0.5))
                .ml(px(-side / 2.))
                .children(art)
                .into_any_element()
        } else {
            mover
                .with_animation(
                    ("fly", serial),
                    Animation::new(VISIT),
                    move |mover, delta| {
                        let elapsed = delta * VISIT.as_secs_f32();
                        let rest = 1. - delta;
                        let at = place(delta, motion);
                        let fade = (delta / 0.05).min(rest / 0.05).min(1.);
                        let bob = if motion == Motion::Fly {
                            let wave = (elapsed * std::f32::consts::TAU / 1.6).sin();

                            (2. * wave / BAR_UNIT).round() * BAR_UNIT
                        } else {
                            0.
                        };
                        let art =
                            frame_of(index, motion, elapsed).map(|frame| Art::new(frame, BAR_UNIT));

                        mover
                            .left(relative(at))
                            .ml(px(-at * side))
                            .top(px(BAR_TOP + bob))
                            .opacity(fade)
                            .children(art)
                    },
                )
                .into_any_element()
        };

        // The region exists while the creature does, so a screen reader speaks it once.
        let region = LiveRegion::status(
            ("creature-status", serial),
            format!("{} appeared", creature.english),
        );

        // No id and no handlers: a press on the empty bar still drags the window.
        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .child(announce("creature-announcement", region))
            .child(moving)
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------------------
// The workspace side
// ---------------------------------------------------------------------------------------

struct ActiveVisit {
    creature: usize,
    serial: usize,
    view: Entity<Visit>,
    _clicks: Subscription,
    _leaving: Task<()>,
}

/// What the Field guide keeps in the workspace.
#[derive(Default)]
pub(crate) struct FieldGuide {
    schedule: Schedule,
    visit: Option<ActiveVisit>,
    /// The creatures caught, as the daemon last told.
    catches: Vec<Catch>,
    /// Catches made while the daemon was away: sent when it is back.
    queued: Vec<&'static str>,
    serial: usize,
    ticker: Option<Task<()>>,
}

impl FieldGuide {
    /// The English name of the creature that is in the title bar now.
    pub(crate) fn visitor_name(&self) -> Option<&'static str> {
        let visit = self.visit.as_ref()?;

        CREATURES
            .get(visit.creature)
            .map(|creature| creature.english)
    }

    /// The creature for the title bar, when one is there.
    pub(crate) fn visitor_element(&self) -> Option<AnyElement> {
        self.visit
            .as_ref()
            .map(|visit| visit.view.clone().into_any_element())
    }

    #[cfg(test)]
    pub(crate) fn visitor(&self) -> Option<usize> {
        self.visit.as_ref().map(|visit| visit.creature)
    }

    #[cfg(test)]
    pub(crate) fn catch_of(&self, slug: &str) -> Option<&Catch> {
        self.catches.iter().find(|catch| catch.creature == slug)
    }
}

/// The catches the daemon has, and the ones we know that it has not, or not yet: for each
/// creature the higher count and the earlier first catch.
fn merge_catches(daemon: Vec<Catch>, known: &[Catch]) -> Vec<Catch> {
    let mut merged = daemon;

    for catch in known {
        match merged
            .iter_mut()
            .find(|other| other.creature == catch.creature)
        {
            Some(other) => {
                other.count = other.count.max(catch.count);
                other.first_caught_at = other.first_caught_at.min(catch.first_caught_at);
            }
            None => merged.push(catch.clone()),
        }
    }

    merged
}

impl Workspace {
    /// Starts looking every few seconds if a creature should come. Once is enough.
    pub(crate) fn start_field_guide(&mut self, cx: &mut Context<Self>) {
        if self.field_guide.ticker.is_some() {
            return;
        }

        self.field_guide.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;

                let ticked = Self::update_with_window(&this, cx, |workspace, window, cx| {
                    let active = window.is_some_and(|window| window.is_window_active());

                    workspace.tick_field_guide(active, &mut random_seed, cx);
                });

                if let Err(error) = ticked {
                    log::debug!("the workspace closed, the Field guide stops: {error}");
                    break;
                }
            }
        }));
    }

    /// True while agents work, the eggs are on, the window is in front and no creature is
    /// in the title bar.
    fn field_guide_eligible(&self, active: bool, cx: &App) -> bool {
        active
            && self.field_guide.visit.is_none()
            && settings::easter_eggs(cx)
            && self
                .data
                .agents
                .iter()
                .any(|agent| agent.status == AgentStatus::Working)
    }

    /// One look at the rules. `random` gives the dice.
    pub(crate) fn tick_field_guide(
        &mut self,
        active: bool,
        random: &mut impl FnMut() -> u64,
        cx: &mut Context<Self>,
    ) {
        let eligible = self.field_guide_eligible(active, cx);
        let now = self.now_ms();

        if self.field_guide.schedule.tick(now, eligible, random()) {
            self.spawn_creature(pick_creature(random()), cx);
            self.keep_visit(now, cx);
        }
    }

    /// Tells the daemon a creature came, so the limits hold after a restart.
    fn keep_visit(&self, at: u64, cx: &mut Context<Self>) {
        let Some(client) = self.daemon.clone().filter(Client::is_connected) else {
            return;
        };

        cx.background_spawn(async move {
            if let Err(error) = client.record_visit(at) {
                log::warn!("cannot record the visit of the creature: {error}");
            }
        })
        .detach();
    }

    /// Takes the start times the daemon kept.
    pub(crate) fn load_visits(client: Client, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let asking = cx
                .background_executor()
                .spawn(async move { client.visits() });

            let starts = match asking.await {
                Ok(starts) => starts,
                Err(error) => {
                    log::warn!("cannot read when the creatures came: {error}");
                    return;
                }
            };

            let kept = this.update(cx, |workspace, _| {
                workspace.field_guide.schedule.remember(&starts);
            });
            if let Err(error) = kept {
                log::debug!("the workspace closed before the visits came: {error}");
            }
        })
        .detach();
    }

    /// Puts the creature at this place in the list in the title bar for [`VISIT`].
    pub(crate) fn spawn_creature(&mut self, creature: usize, cx: &mut Context<Self>) {
        self.field_guide.serial += 1;
        let serial = self.field_guide.serial;

        let still = reduce_motion(cx);
        let view = cx.new(|_| Visit {
            creature,
            motion: Motion::Fly,
            still,
            serial,
        });
        let clicks = cx.subscribe(&view, |workspace, _, _: &VisitEvent, cx| {
            workspace.catch_creature(cx);
        });
        let leaving = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VISIT).await;

            let left = this.update(cx, |workspace, cx| workspace.creature_left(serial, cx));
            if let Err(error) = left {
                log::debug!("the workspace closed before the creature left: {error}");
            }
        });

        self.field_guide.visit = Some(ActiveVisit {
            creature,
            serial,
            view,
            _clicks: clicks,
            _leaving: leaving,
        });
        cx.notify();
    }

    fn creature_left(&mut self, serial: usize, cx: &mut Context<Self>) {
        if self
            .field_guide
            .visit
            .as_ref()
            .is_some_and(|visit| visit.serial == serial)
        {
            self.field_guide.visit = None;
            cx.notify();
        }
    }

    /// Catches the creature in the title bar: the click and the palette command end here.
    pub(crate) fn catch_creature(&mut self, cx: &mut Context<Self>) {
        let Some(visit) = self.field_guide.visit.take() else {
            return;
        };
        let Some(creature) = CREATURES.get(visit.creature) else {
            return;
        };
        let slug = creature.slug;
        drop(visit);
        cx.notify();

        let Some(client) = self.daemon.clone().filter(Client::is_connected) else {
            let count = self
                .field_guide
                .catches
                .iter()
                .find(|catch| catch.creature == slug)
                .map_or(0, |catch| catch.count);
            let first_caught_at = self
                .field_guide
                .catches
                .iter()
                .find(|catch| catch.creature == slug)
                .map_or_else(|| self.now_ms(), |catch| catch.first_caught_at);

            self.field_guide.queued.push(slug);
            self.catch_recorded(
                Catch {
                    creature: slug.to_owned(),
                    first_caught_at,
                    count: count + 1,
                },
                None,
                cx,
            );

            return;
        };

        cx.spawn(async move |this, cx| {
            let recording = cx
                .background_executor()
                .spawn(async move { client.record_catch(slug) });

            let caught = match recording.await {
                Ok(caught) => caught,
                Err(error) => {
                    log::warn!("cannot record the catch of {slug}: {error}");
                    return;
                }
            };

            let shown = Self::update_with_window(&this, cx, |workspace, window, cx| {
                workspace.catch_recorded(caught, window, cx);
            });
            if let Err(error) = shown {
                log::debug!("the workspace closed before the catch was shown: {error}");
            }
        })
        .detach();
    }

    /// Keeps a catch the daemon recorded, shows the toast and updates the count.
    pub(crate) fn catch_recorded(
        &mut self,
        caught: Catch,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let (slug, count) = (caught.creature.clone(), caught.count);

        match self
            .field_guide
            .catches
            .iter_mut()
            .find(|catch| catch.creature == slug)
        {
            Some(known) => *known = caught,
            None => self.field_guide.catches.push(caught),
        }
        self.sync_caught_creatures();

        if let Some(creature) = CREATURES.iter().position(|creature| creature.slug == slug) {
            self.place_catch_toast(creature, count, window, cx);
        }
        cx.notify();
    }

    fn sync_caught_creatures(&mut self) {
        self.data.caught_creatures = self
            .field_guide
            .catches
            .iter()
            .map(|catch| catch.creature.clone())
            .collect();
    }

    /// Sends the catches made while the daemon was away, then takes the catches it has. A
    /// catch that is not sent stays queued, and the ones we know stay in the list.
    pub(crate) fn load_catches(&mut self, client: Client, cx: &mut Context<Self>) {
        let queued = std::mem::take(&mut self.field_guide.queued);

        cx.spawn(async move |this, cx| {
            let asking = cx.background_executor().spawn(async move {
                let unsent: Vec<&'static str> = queued
                    .into_iter()
                    .filter(|slug| {
                        client
                            .record_catch(slug)
                            .inspect_err(|error| {
                                log::warn!("cannot record the catch of {slug}: {error}");
                            })
                            .is_err()
                    })
                    .collect();

                (unsent, client.catches())
            });

            let (unsent, catches) = asking.await;
            let catches = match catches {
                Ok(catches) => catches,
                Err(error) => {
                    log::warn!("cannot read the caught creatures: {error}");
                    Vec::new()
                }
            };

            let shown = this.update(cx, |workspace, cx| {
                workspace.field_guide.queued.extend(unsent);
                workspace.field_guide.catches =
                    merge_catches(catches, &workspace.field_guide.catches);
                workspace.sync_caught_creatures();
                cx.notify();
            });
            if let Err(error) = shown {
                log::debug!("the workspace closed before the catches came: {error}");
            }
        })
        .detach();
    }

    /// Opens the guide over the app.
    pub(crate) fn open_field_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let catches = self.field_guide.catches.clone();
        let view = cx.new(|cx| FieldGuideView::new(catches, window, cx));
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                |workspace, _, event, window, cx| match event {
                    FieldGuideEvent::Dismissed => workspace.close_overlay(window, cx),
                },
            );

        self.overlay = Some(Overlay::FieldGuide {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }
}

/// Opens the guide in the main window, from wherever the action comes.
pub(crate) fn register(cx: &mut App) {
    cx.on_action(|_: &OpenFieldGuide, cx| cx.defer(open_in_main_window));
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
            workspace.open_field_guide(window, cx);
        });
    });

    if let Err(error) = opened {
        log::debug!("the main window closed before the Field guide opened: {error}");
    }
}

// ---------------------------------------------------------------------------------------
// The guide
// ---------------------------------------------------------------------------------------

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Common => "Common",
        Tier::Rare => "Rare",
        Tier::UltraRare => "Ultra rare",
    }
}

fn tier_tone(tier: Tier) -> TagTone {
    match tier {
        Tier::Common => TagTone::Neutral,
        Tier::Rare => TagTone::Accent,
        Tier::UltraRare => TagTone::Warn,
    }
}

/// How often, for a label.
fn times(count: u32) -> String {
    if count == 1 {
        "once".to_owned()
    } else {
        format!("{count} times")
    }
}

/// What a catch toast says under "<name> caught".
pub(crate) fn catch_message(count: u32) -> String {
    if count <= 1 {
        "New".to_owned()
    } else {
        format!("Caught {}", times(count))
    }
}

/// The creature at rest, small, for the catch toast. It is decoration.
pub(crate) fn toast_art(creature: usize) -> Option<impl IntoElement + use<>> {
    let frame = frame_of(creature, Motion::Still, 0.)?;

    Some(Art::new(frame, BAR_UNIT))
}

/// The places of the creatures in the list, row by row as the guide shows them: each tier
/// in rows of [`COLUMNS`].
pub(crate) fn grid_rows() -> Vec<Vec<usize>> {
    let mut rows = Vec::new();

    for tier in TIERS {
        let of_tier: Vec<usize> = CREATURES
            .iter()
            .enumerate()
            .filter(|(_, creature)| creature.tier == tier)
            .map(|(index, _)| index)
            .collect();

        rows.extend(of_tier.chunks(COLUMNS).map(<[usize]>::to_vec));
    }

    rows
}

/// Where an arrow key, Home or End takes the selection. Left and right follow the reading
/// order. Up and down keep the column, or the last card of a shorter row.
pub(crate) fn move_in_grid(rows: &[Vec<usize>], from: usize, key: &str) -> Option<usize> {
    let order: Vec<usize> = rows.iter().flatten().copied().collect();
    let place = order.iter().position(|index| *index == from)?;

    let row = rows.iter().position(|row| row.contains(&from))?;
    let column = rows.get(row)?.iter().position(|index| *index == from)?;

    let vertical = |target: Option<&Vec<usize>>| {
        let target = target?;

        target.get(column).or_else(|| target.last()).copied()
    };

    match key {
        "left" => place
            .checked_sub(1)
            .and_then(|before| order.get(before))
            .copied(),
        "right" => order.get(place + 1).copied(),
        "up" => vertical(row.checked_sub(1).and_then(|above| rows.get(above))),
        "down" => vertical(rows.get(row + 1)),
        "home" => order.first().copied(),
        "end" => order.last().copied(),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldGuideEvent {
    Dismissed,
}

/// The open guide.
pub(crate) struct FieldGuideView {
    catches: Vec<Catch>,
    focus_handle: FocusHandle,
    roving: RovingFocus,
    scroll: ScrollHandle,
    /// The child of the scrolling list that holds each card, from the last draw.
    row_of: Vec<usize>,
    frame: usize,
    frame_timer: Option<Task<()>>,
    /// What had the focus before the guide opened.
    pub(crate) opener: Option<FocusHandle>,
}

impl EventEmitter<FieldGuideEvent> for FieldGuideView {}

impl Focusable for FieldGuideView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FieldGuideView {
    fn new(catches: Vec<Catch>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let opener = window.focused(cx);

        let mut roving = RovingFocus::new(gpui::Axis::Horizontal);
        roving.sync(CREATURES.len(), cx);
        window.focus(&roving.handle(0), cx);

        let frames = (!reduce_motion(cx)).then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(GUIDE_FRAME).await;

                    let moved = this.update(cx, |view, cx| {
                        view.frame = view.frame.wrapping_add(1);
                        cx.notify();
                    });
                    if moved.is_err() {
                        break;
                    }
                }
            })
        });

        Self {
            catches,
            focus_handle: cx.focus_handle(),
            roving,
            scroll: ScrollHandle::new(),
            row_of: vec![0; CREATURES.len()],
            frame: 0,
            frame_timer: frames,
            opener,
        }
    }

    fn catch_of(&self, creature: &Creature) -> Option<&Catch> {
        self.catches
            .iter()
            .find(|catch| catch.creature == creature.slug)
    }

    fn caught(&self) -> usize {
        CREATURES
            .iter()
            .filter(|creature| self.catch_of(creature).is_some())
            .count()
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.roving.set_active(index);
        window.focus(&self.roving.handle(index), cx);

        if let Some(row) = self.row_of.get(index) {
            self.scroll.scroll_to_item(*row);
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let Some(current) = self.roving.focused(window, cx) else {
            return;
        };

        let key = keystroke.key.as_str();
        if matches!(key, "enter" | "space") {
            if let Some(creature) = CREATURES.get(current)
                && self.catch_of(creature).is_some()
            {
                cx.open_url(creature.wikipedia);
            }

            cx.stop_propagation();

            return;
        }

        if let Some(target) = move_in_grid(&grid_rows(), current, key) {
            self.select(target, window, cx);
            cx.stop_propagation();
        }
    }

    fn card(&mut self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let creature = CREATURES.get(index)?;
        let sprite = swarm_creatures::sprite(index)?;
        let caught = self.catch_of(creature).map(|catch| catch.count);
        let colors = cx.theme().colors();
        let accent = colors.accent;
        let tier = tier_name(creature.tier);

        let motion = if self.frame_timer.is_some() {
            Motion::Fly
        } else {
            Motion::Still
        };
        let shown = if caught.is_some() {
            sprite.motion(motion)
        } else {
            Motion::Still
        };
        let frames = sprite.frames(shown);
        let frame = frames.get(self.frame % frames.len().max(1));

        let art = frame.map(|frame| Art::new(frame, CARD_UNIT).silhouette(caught.is_none()));

        let label = match caught {
            Some(count) => format!(
                "{}, {}, {tier}, caught {}",
                creature.english,
                creature.latin,
                times(count)
            ),
            None => format!("Not caught yet, {tier}"),
        };

        let stage = div()
            .h(px(Art::side(CARD_UNIT) + 24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(colors.line)
            // GPUI clips children to a rectangle, so the paper rounds its own top corners
            // inside the card's 12 px corner and 1 px border.
            .rounded_t(px(11.))
            .bg(paint_color(PAPER))
            .children(art);

        let details = match caught {
            Some(count) => v_flex()
                .gap(px(4.))
                .p(px(12.))
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Label::new(creature.english).weight(FontWeight::SEMIBOLD)),
                        )
                        .child(Tag::new(tier).tone(tier_tone(creature.tier))),
                )
                .child(
                    div().italic().child(
                        Label::new(creature.latin)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .child(
                    Label::new(format!("Caught {}", times(count)))
                        .size(LabelSize::Small)
                        .color(Color::Faint),
                )
                .child(Self::wikipedia_link(index, creature)),
            None => v_flex().gap(px(4.)).p(px(12.)).child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap(px(8.))
                    .child(Label::new("?").weight(FontWeight::SEMIBOLD))
                    .child(Tag::new(tier).tone(tier_tone(creature.tier))),
            ),
        };

        Some(
            div()
                .id(("creature-card", index))
                .role(Role::ListItem)
                .aria_label(label)
                .debug_selector(move || format!("creature-card-{index}"))
                .track_focus(&self.roving.handle(index))
                .focus_visible(|style| focus_ring(style, accent))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(12.))
                .border_1()
                .border_color(colors.line)
                .bg(colors.raised)
                .on_click(cx.listener(move |view, _, window, cx| view.select(index, window, cx)))
                .child(stage)
                .child(details)
                .into_any_element(),
        )
    }

    fn wikipedia_link(index: usize, creature: &Creature) -> impl IntoElement + use<> {
        let url = creature.wikipedia;

        div()
            .id(("creature-wikipedia", index))
            .role(Role::Link)
            .aria_label(format!("{} on Wikipedia", creature.english))
            .debug_selector(move || format!("creature-wikipedia-{index}"))
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.open_url(url);
            })
            .child(
                Label::new("Wikipedia")
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
    }
}

/// A row of cards. The cards are list items, so the row is the list that holds them.
fn card_row(
    position: usize,
    label: String,
    cards: Vec<AnyElement>,
    spacers: usize,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(("field-guide-row", position))
        .role(Role::List)
        .aria_label(label)
        .gap(px(12.))
        .p(px(2.))
        .children(cards)
        .children((0..spacers).map(|_| div().flex_1().min_w_0()))
}

impl Render for FieldGuideView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.roving.sync(CREATURES.len(), cx);

        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 64.).clamp(560., 1040.);
        // The dialog is centered with 32 px around it; its title and padding take about 100.
        let height = (f32::from(viewport.height) - 64. - 100.).max(240.);

        let mut content: Vec<AnyElement> = Vec::new();
        let mut row_of = vec![0; CREATURES.len()];
        let rows = grid_rows();

        for (tier_number, tier) in TIERS.into_iter().enumerate() {
            let total = CREATURES
                .iter()
                .filter(|creature| creature.tier == tier)
                .count();
            let caught = CREATURES
                .iter()
                .filter(|creature| creature.tier == tier && self.catch_of(creature).is_some())
                .count();

            content.push(
                h_flex()
                    .gap(px(8.))
                    .pt(px(4.))
                    .child(
                        Label::new(tier_name(tier))
                            .weight(FontWeight::SEMIBOLD)
                            .heading(("field-guide-tier", tier_number), 2),
                    )
                    .child(
                        Label::new(format!("{caught}/{total}"))
                            .mono()
                            .size(LabelSize::Small)
                            .color(Color::Faint),
                    )
                    .into_any_element(),
            );

            let tier_rows = rows.iter().filter(|row| {
                row.first()
                    .and_then(|index| CREATURES.get(*index))
                    .is_some_and(|creature| creature.tier == tier)
            });

            for (number, row) in tier_rows.enumerate() {
                let cards: Vec<AnyElement> = row
                    .iter()
                    .filter_map(|index| {
                        if let Some(slot) = row_of.get_mut(*index) {
                            *slot = content.len();
                        }

                        self.card(*index, cx)
                    })
                    .collect();
                let spacers = COLUMNS.saturating_sub(cards.len());

                let label = format!("{}, row {}", tier_name(tier), number + 1);

                content.push(card_row(content.len(), label, cards, spacers).into_any_element());
            }
        }
        self.row_of = row_of;

        let title = format!("Field guide {}/{}", self.caught(), CREATURES.len());
        let on_dismiss = cx.entity();

        let list = v_flex()
            .id("field-guide-list")
            .debug_selector(|| "field-guide-list".to_owned())
            .gap(px(10.))
            .max_h(px(height))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .children(content);

        Dialog::new("field-guide", SharedString::from(title), &self.focus_handle)
            .width(px(width))
            .on_dismiss(move |_, cx| {
                on_dismiss.update(cx, |_, cx| cx.emit(FieldGuideEvent::Dismissed));
            })
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use swarm_protocol::{Event, Request};

    use super::*;
    use crate::daemon::tests::{fake_daemon_replying, redraw, setup, wait_until};
    use crate::workspace::follow_test_reduce_motion;

    const MINUTE: u64 = 60_000;

    #[test]
    fn no_creature_comes_while_it_may_not() {
        let mut schedule = Schedule::default();

        for minute in 0..120 {
            assert!(!schedule.tick(minute * MINUTE, false, 0));
        }
    }

    #[test]
    fn a_creature_comes_when_the_wait_is_over_and_only_once() {
        let mut schedule = Schedule::default();

        assert!(
            run(&mut schedule, 0, MINUTE, true, 0).is_empty(),
            "the wait is not over"
        );
        assert!(schedule.tick(MINUTE, true, 0));
        assert!(
            !schedule.tick(MINUTE + 1, true, 0),
            "the gap has just begun"
        );
    }

    /// Ticks every [`TICK`] from `from` until `to`, and gives the times a creature came.
    fn run(schedule: &mut Schedule, from: u64, to: u64, eligible: bool, random: u64) -> Vec<u64> {
        (from..to)
            .step_by(usize::try_from(millis(TICK)).expect("a tick fits"))
            .filter(|now| schedule.tick(*now, eligible, random))
            .collect()
    }

    #[test]
    fn the_wait_pauses_while_a_creature_may_not_come_and_goes_on_after() {
        let mut schedule = Schedule::default();
        let random = 9 * MINUTE;
        let wait = Schedule::wait(random);
        assert_eq!(wait, 10 * MINUTE);

        assert!(run(&mut schedule, 0, 6 * MINUTE, true, random).is_empty());
        assert!(run(&mut schedule, 6 * MINUTE, 60 * MINUTE, false, random).is_empty());

        // The ticks from 0 to 5:45 counted 5:45, so 4:15 is left.
        let back = 60 * MINUTE;
        let left = wait - (6 * MINUTE - millis(TICK));
        assert!(run(&mut schedule, back, back + left, true, random).is_empty());
        assert!(schedule.tick(back + left, true, random));
    }

    #[test]
    fn a_pause_does_not_roll_a_new_wait() {
        let mut schedule = Schedule::default();
        run(&mut schedule, 0, 5 * MINUTE, true, 29 * MINUTE);
        run(&mut schedule, 5 * MINUTE, 10 * MINUTE, false, 29 * MINUTE);

        // A new roll of the dice would be the shortest wait, 1 minute.
        let times = run(&mut schedule, 10 * MINUTE, 60 * MINUTE, true, 0);

        assert_eq!(times.len(), 1);
        assert!(times[0] > 30 * MINUTE, "{times:?}");
    }

    #[test]
    fn short_pauses_do_not_keep_a_creature_away() {
        let mut schedule = Schedule::default();
        let mut times = Vec::new();

        // In front for 2 minutes, in the back for 1, over and over.
        for round in 0..20 {
            let start = round * 3 * MINUTE;
            times.extend(run(
                &mut schedule,
                start,
                start + 2 * MINUTE,
                true,
                29 * MINUTE,
            ));
            run(
                &mut schedule,
                start + 2 * MINUTE,
                start + 3 * MINUTE,
                false,
                0,
            );
        }

        assert_eq!(times.len(), 1, "{times:?}");
    }

    #[test]
    fn a_long_step_counts_as_a_short_one() {
        let mut schedule = Schedule::default();
        schedule.tick(0, true, 0);

        assert!(
            !schedule.tick(8 * 60 * MINUTE, true, 0),
            "a night of sleep is not the wait"
        );
        assert!(schedule.tick(8 * 60 * MINUTE + 30_000, true, 0));
    }

    #[test]
    fn the_cap_and_the_gap_hold_across_pauses() {
        let mut schedule = Schedule::default();
        let mut times = Vec::new();

        for hour in 0..24 {
            let start = hour * 60 * MINUTE;
            times.extend(run(&mut schedule, start, start + 40 * MINUTE, true, 0));
            run(
                &mut schedule,
                start + 40 * MINUTE,
                start + 60 * MINUTE,
                false,
                0,
            );
        }

        assert_eq!(times.len(), MAX_PER_DAY);
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= millis(MIN_GAP), "{pair:?}");
        }
    }

    #[test]
    fn a_creature_stays_45_seconds() {
        assert_eq!(VISIT, Duration::from_secs(45));
    }

    #[test]
    fn a_flier_crosses_the_bar_from_right_to_left_and_never_turns_back() {
        let places: Vec<f32> = (0..=1000_u16)
            .map(|step| place(f32::from(step) / 1000., Motion::Fly))
            .collect();

        assert!((places[0] - 1.).abs() < 1e-4, "{}", places[0]);
        assert!(places[1000].abs() < 1e-4, "{}", places[1000]);
        for pair in places.windows(2) {
            assert!(pair[1] <= pair[0], "{pair:?}");
        }
    }

    #[test]
    fn a_crawler_keeps_one_pace() {
        assert!((place(0.25, Motion::Crawl) - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn creatures_come_at_least_45_minutes_apart() {
        let mut schedule = Schedule::default();
        let times = run(&mut schedule, 0, 600 * MINUTE, true, 0);

        assert!(times.len() > 1);
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= millis(MIN_GAP), "{pair:?}");
        }
    }

    #[test]
    fn at_most_four_creatures_come_in_a_day() {
        let mut schedule = Schedule::default();
        let times = run(&mut schedule, 0, 24 * 60 * MINUTE, true, 0);

        assert_eq!(times.len(), MAX_PER_DAY);
    }

    #[test]
    fn the_cap_lifts_a_day_after_the_first_creature() {
        let mut schedule = Schedule::default();
        let times = run(&mut schedule, 0, 26 * 60 * MINUTE, true, 0);

        let day = millis(DAY);
        assert!(times.len() > MAX_PER_DAY);
        assert!(times[MAX_PER_DAY] >= times[0] + day, "{times:?}");
    }

    #[test]
    fn the_wait_comes_from_the_dice_within_its_range() {
        let (least, most) = (millis(WAIT.0), millis(WAIT.1));

        assert_eq!(Schedule::wait(0), least);
        assert_eq!(Schedule::wait(most - least), most);
        assert_eq!(Schedule::wait(most - least + 1), least);
    }

    #[test]
    fn a_row_of_cards_is_the_list_that_holds_the_list_items() {
        use gpui::Element as _;

        let row = card_row(0, "Common, row 1".to_owned(), Vec::new(), 0);

        assert_eq!(row.a11y_role(), Some(Role::List));
    }

    fn bee_frame() -> &'static Frame {
        let sprite = swarm_creatures::sprite(0).expect("a sprite");

        &sprite.frames(Motion::Still)[0]
    }

    fn run_count(frame: &Frame) -> usize {
        (0..frame.side()).map(|row| frame.runs(row).len()).sum()
    }

    fn bar_quads(factor: f32, origin: Point<Pixels>) -> Vec<(Bounds<Pixels>, Rgb)> {
        sprite_quads(bee_frame(), origin, BAR_UNIT, factor).collect()
    }

    /// True when every quad is at least one device pixel wide and high.
    fn none_vanishes(quads: &[(Bounds<Pixels>, Rgb)], factor: f32) -> bool {
        quads.iter().all(|(bounds, _)| {
            f32::from(bounds.size.width) * factor > 0.99
                && f32::from(bounds.size.height) * factor > 0.99
        })
    }

    #[test]
    fn on_a_retina_screen_the_bar_draws_each_run_of_the_full_frame() {
        let quads = bar_quads(2., point(px(10.), px(4.)));

        assert_eq!(quads.len(), run_count(bee_frame()));
        assert!(none_vanishes(&quads, 2.));
    }

    #[test]
    fn on_a_one_x_screen_the_bar_draws_the_frame_of_every_second_pixel() {
        let quads = bar_quads(1., point(px(10.), px(4.)));

        assert_eq!(quads.len(), run_count(bee_frame().halved()));
        assert!(none_vanishes(&quads, 1.));
    }

    #[test]
    fn between_one_x_and_two_x_no_run_of_the_bar_vanishes() {
        let quads = bar_quads(1.5, point(px(10.), px(4.)));

        assert_eq!(quads.len(), run_count(bee_frame().halved()));
        assert!(none_vanishes(&quads, 1.5));
    }

    #[test]
    fn the_sprite_origin_snaps_to_a_whole_device_pixel() {
        for factor in [1., 1.5, 2.] {
            let quads = bar_quads(factor, point(px(10.3), px(4.6)));

            let whole = |value: Pixels| {
                let device = f32::from(value) * factor;

                (device - device.round()).abs() < 0.001
            };
            assert!(
                quads
                    .iter()
                    .all(|(bounds, _)| whole(bounds.origin.x) && whole(bounds.origin.y)),
                "scale factor {factor}"
            );
        }
    }

    /// A small seeded generator, so the odds can be counted.
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);

        z ^ (z >> 31)
    }

    #[test]
    fn the_tiers_split_the_rolls_70_25_and_5() {
        let tiers: Vec<Tier> = (0..100).map(tier_for).collect();
        let count = |tier| tiers.iter().filter(|found| **found == tier).count();

        assert_eq!(count(Tier::Common), 70);
        assert_eq!(count(Tier::Rare), 25);
        assert_eq!(count(Tier::UltraRare), 5);
    }

    #[test]
    fn picked_creatures_follow_the_tier_weights() {
        let mut state = 7;
        let mut counts = [0_usize; 3];
        let draws = 20_000;

        for _ in 0..draws {
            let index = pick_creature(splitmix(&mut state));
            let tier = CREATURES[index].tier;
            let slot = TIERS.iter().position(|candidate| *candidate == tier);

            counts[slot.expect("a known tier")] += 1;
        }

        let percent = |slot: usize| counts[slot] * 100 / draws;
        assert!((68..=72).contains(&percent(0)), "{counts:?}");
        assert!((23..=27).contains(&percent(1)), "{counts:?}");
        assert!((4..=6).contains(&percent(2)), "{counts:?}");
    }

    #[test]
    fn every_creature_can_be_picked() {
        let mut state = 11;
        let mut seen = [false; 20];

        for _ in 0..20_000 {
            seen[pick_creature(splitmix(&mut state))] = true;
        }

        assert!(seen.iter().all(|found| *found), "{seen:?}");
    }

    #[test]
    fn the_grid_shows_each_tier_in_rows_of_five() {
        let shape: Vec<usize> = grid_rows().iter().map(Vec::len).collect();

        assert_eq!(shape, [5, 5, 5, 1, 4]);
        assert_eq!(grid_rows().concat(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn the_arrows_move_through_the_grid() {
        let rows = grid_rows();

        assert_eq!(move_in_grid(&rows, 0, "right"), Some(1));
        assert_eq!(
            move_in_grid(&rows, 4, "right"),
            Some(5),
            "on to the next row"
        );
        assert_eq!(move_in_grid(&rows, 0, "left"), None);
        assert_eq!(move_in_grid(&rows, 2, "down"), Some(7));
        assert_eq!(move_in_grid(&rows, 7, "up"), Some(2));
        assert_eq!(move_in_grid(&rows, 2, "up"), None);
        assert_eq!(move_in_grid(&rows, 19, "down"), None);
        assert_eq!(move_in_grid(&rows, 7, "home"), Some(0));
        assert_eq!(move_in_grid(&rows, 7, "end"), Some(19));
        assert_eq!(move_in_grid(&rows, 7, "x"), None);
    }

    #[test]
    fn up_and_down_land_on_the_last_card_of_a_shorter_row() {
        let rows = grid_rows();

        assert_eq!(move_in_grid(&rows, 13, "down"), Some(15));
        assert_eq!(move_in_grid(&rows, 15, "down"), Some(16));
        assert_eq!(move_in_grid(&rows, 19, "up"), Some(15));
    }

    fn work(workspace: &Entity<Workspace>, status: AgentStatus, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, _| {
            for agent in &mut workspace.data.agents {
                agent.status = status;
            }
        });
    }

    fn tick(workspace: &Entity<Workspace>, now: u64, cx: &mut VisualTestContext) {
        workspace.update(cx, |workspace, cx| {
            workspace.fixed_now = Some(now);
            workspace.tick_field_guide(true, &mut || 0, cx);
        });
    }

    fn visitor(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<usize> {
        workspace.read_with(cx, |workspace, _| workspace.field_guide.visitor())
    }

    /// Ticks every [`TICK`] for two minutes. The first creature comes after one.
    fn wait_for_a_creature(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        for step in 0..=8 {
            tick(workspace, 1_000_000 + step * millis(TICK), cx);
        }
    }

    #[gpui::test]
    fn a_creature_comes_while_an_agent_works(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        work(&workspace, AgentStatus::Working, cx);

        wait_for_a_creature(&workspace, cx);

        assert!(visitor(&workspace, cx).is_some());
    }

    #[gpui::test]
    fn no_creature_comes_when_no_agent_works(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        work(&workspace, AgentStatus::Idle, cx);

        wait_for_a_creature(&workspace, cx);

        assert!(visitor(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn no_creature_comes_with_the_easter_eggs_off(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        work(&workspace, AgentStatus::Working, cx);
        cx.update(|_, cx| {
            settings::follow_appearance(workspace.clone(), cx);
            settings::change_appearance(|appearance| appearance.easter_eggs = false, cx);
        });

        wait_for_a_creature(&workspace, cx);

        assert!(visitor(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn no_creature_comes_while_the_window_is_in_the_back(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        work(&workspace, AgentStatus::Working, cx);

        for minute in 0..3 {
            workspace.update(cx, |workspace, cx| {
                workspace.fixed_now = Some(1_000_000 + minute * MINUTE);
                workspace.tick_field_guide(false, &mut || 0, cx);
            });
        }

        assert!(visitor(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn a_creature_leaves_after_its_visit(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        work(&workspace, AgentStatus::Working, cx);
        wait_for_a_creature(&workspace, cx);
        assert!(visitor(&workspace, cx).is_some());

        cx.executor()
            .advance_clock(VISIT.saturating_sub(Duration::from_secs(1)));
        cx.run_until_parked();
        assert!(visitor(&workspace, cx).is_some(), "it is still there");

        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert!(visitor(&workspace, cx).is_none());
    }

    #[gpui::test]
    fn the_creature_is_in_the_title_bar_and_announced_once(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        redraw(cx);
        assert!(cx.debug_bounds("creature").is_none());

        workspace.update(cx, |workspace, cx| workspace.spawn_creature(0, cx));
        redraw(cx);

        let bounds = cx.debug_bounds("creature").expect("the creature is drawn");
        assert!(
            bounds.origin.y < crate::workspace::TITLE_BAR_HEIGHT,
            "{bounds:?}"
        );
        assert!(cx.debug_bounds("creature-announcement").is_some());
    }

    #[gpui::test]
    fn the_creature_sits_still_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(true, cx));
        workspace.update(cx, |workspace, cx| workspace.spawn_creature(0, cx));
        redraw(cx);
        let first = cx.debug_bounds("creature").expect("drawn");

        cx.executor().advance_clock(Duration::from_secs(5));
        redraw(cx);
        let later = cx.debug_bounds("creature").expect("still drawn");

        assert_eq!(first, later);
    }

    #[gpui::test]
    fn the_creature_moves_when_motion_is_allowed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(false, cx));
        workspace.update(cx, |workspace, cx| workspace.spawn_creature(0, cx));
        redraw(cx);
        let first = cx.debug_bounds("creature").expect("drawn");

        cx.executor().advance_clock(Duration::from_secs(10));
        redraw(cx);
        let later = cx.debug_bounds("creature").expect("still drawn");

        assert!(later.origin.x < first.origin.x, "{first:?} then {later:?}");
    }

    fn caught_slugs(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, _| workspace.data.caught_creatures.clone())
    }

    #[gpui::test]
    fn a_click_catches_the_creature(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.spawn_creature(0, cx));
        redraw(cx);
        let bounds = cx.debug_bounds("creature").expect("drawn");

        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);

        assert!(visitor(&workspace, cx).is_none(), "it is gone");
        assert_eq!(caught_slugs(&workspace, cx), ["apis-mellifera"]);
    }

    #[gpui::test]
    fn a_catch_shows_the_toast_with_new_for_the_first_time(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(true, cx));

        workspace.update(cx, |workspace, cx| {
            workspace.spawn_creature(2, cx);
            workspace.catch_creature(cx);
        });
        redraw(cx);

        let toast = workspace.read_with(cx, |workspace, _| {
            workspace
                .eggs
                .toast
                .as_ref()
                .map(|toast| (toast.task.to_string(), toast.caught.map(|c| c.count)))
        });
        assert_eq!(
            toast,
            Some(("Buff-tailed bumblebee caught".to_owned(), Some(1)))
        );
        assert!(cx.debug_bounds("honey-toast").is_some());
        assert!(
            cx.debug_bounds("honey-toast-action").is_none(),
            "a catch has nothing to review"
        );
    }

    #[test]
    fn the_toast_says_new_for_a_first_catch_and_counts_the_next() {
        assert_eq!(catch_message(1), "New");
        assert_eq!(catch_message(2), "Caught 2 times");
    }

    #[gpui::test]
    fn catching_again_counts_up_and_keeps_one_entry(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        for _ in 0..2 {
            workspace.update(cx, |workspace, cx| {
                workspace.spawn_creature(0, cx);
                workspace.catch_creature(cx);
            });
        }

        let count = workspace.read_with(cx, |workspace, _| {
            workspace
                .field_guide
                .catch_of("apis-mellifera")
                .map(|catch| catch.count)
        });
        assert_eq!(count, Some(2));
        assert_eq!(caught_slugs(&workspace, cx).len(), 1);
    }

    #[gpui::test]
    fn the_palette_offers_the_catch_while_the_creature_is_there(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| workspace.spawn_creature(2, cx));

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("Catch the Buff-tailed bumblebee");
        cx.simulate_keystrokes("enter");
        redraw(cx);

        assert!(visitor(&workspace, cx).is_none());
        assert_eq!(caught_slugs(&workspace, cx), ["bombus-terrestris"]);
    }

    #[gpui::test]
    fn the_palette_does_not_offer_a_catch_without_a_creature(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("Catch the");
        cx.simulate_keystrokes("enter");
        redraw(cx);

        assert!(caught_slugs(&workspace, cx).is_empty());
    }

    #[gpui::test]
    fn the_palette_opens_the_field_guide(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        cx.simulate_keystrokes("cmd-k");
        cx.simulate_input("Field guide");
        cx.simulate_keystrokes("enter");
        redraw(cx);

        let open = workspace.read_with(cx, |workspace, _| {
            matches!(workspace.overlay, Some(Overlay::FieldGuide { .. }))
        });
        assert!(open);
    }

    fn open_guide(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_field_guide(window, cx);
        });
        redraw(cx);
    }

    fn catch(
        workspace: &Entity<Workspace>,
        creature: usize,
        times: usize,
        cx: &mut VisualTestContext,
    ) {
        for _ in 0..times {
            workspace.update(cx, |workspace, cx| {
                workspace.spawn_creature(creature, cx);
                workspace.catch_creature(cx);
            });
        }
    }

    #[gpui::test]
    fn the_guide_lists_all_twenty_creatures(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);

        open_guide(&workspace, cx);

        for index in 0..20 {
            let card: &'static str = format!("creature-card-{index}").leak();
            assert!(cx.debug_bounds(card).is_some(), "{card} is missing");
        }
    }

    #[gpui::test]
    fn the_guide_links_the_creatures_caught_and_hides_the_others(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        catch(&workspace, 1, 3, cx);

        open_guide(&workspace, cx);

        assert!(cx.debug_bounds("creature-wikipedia-1").is_some());
        assert!(cx.debug_bounds("creature-wikipedia-0").is_none());
        assert!(cx.debug_bounds("creature-wikipedia-19").is_none());
    }

    #[gpui::test]
    fn the_guide_title_counts_the_creatures_caught(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        catch(&workspace, 1, 1, cx);
        catch(&workspace, 12, 2, cx);

        open_guide(&workspace, cx);

        let caught = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::FieldGuide { view, .. }) => view.read(cx).caught(),
            _ => 0,
        });
        assert_eq!(caught, 2);
    }

    #[gpui::test]
    fn escape_closes_the_guide(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_guide(&workspace, cx);

        cx.simulate_keystrokes("escape");
        redraw(cx);

        let open = workspace.read_with(cx, |workspace, _| workspace.overlay.is_some());
        assert!(!open);
    }

    fn selected(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<usize> {
        cx.update(|window, cx| {
            workspace
                .read(cx)
                .overlay
                .as_ref()
                .and_then(|overlay| match overlay {
                    Overlay::FieldGuide { view, .. } => view.read(cx).roving.focused(window, cx),
                    _ => None,
                })
        })
    }

    #[gpui::test]
    fn the_arrow_keys_move_between_the_cards(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        open_guide(&workspace, cx);
        assert_eq!(selected(&workspace, cx), Some(0));

        cx.simulate_keystrokes("right");
        assert_eq!(selected(&workspace, cx), Some(1));

        cx.simulate_keystrokes("down");
        assert_eq!(selected(&workspace, cx), Some(6));

        cx.simulate_keystrokes("end");
        assert_eq!(selected(&workspace, cx), Some(19));
    }

    #[gpui::test]
    fn the_guide_stands_still_under_reduce_motion(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(true, cx));
        open_guide(&workspace, cx);

        let animated = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::FieldGuide { view, .. }) => view.read(cx).frame_timer.is_some(),
            _ => true,
        });
        assert!(!animated);
    }

    #[gpui::test]
    fn the_guide_changes_frames_when_motion_is_allowed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| follow_test_reduce_motion(false, cx));
        open_guide(&workspace, cx);

        cx.executor().advance_clock(GUIDE_FRAME * 3);
        cx.run_until_parked();

        let frame = workspace.read_with(cx, |workspace, cx| match &workspace.overlay {
            Some(Overlay::FieldGuide { view, .. }) => view.read(cx).frame,
            _ => 0,
        });
        assert!(frame >= 2, "{frame}");
    }

    fn catches_reply(known: Vec<Catch>) -> std::path::PathBuf {
        fake_daemon_replying(move |request| match request {
            Request::Catches { call } => Some(Event::Catches {
                call,
                catches: known.clone(),
            }),
            Request::RecordCatch { call, creature } => Some(Event::CatchRecorded {
                call,
                caught: Catch {
                    creature,
                    first_caught_at: 5,
                    count: 4,
                },
            }),
            _ => None,
        })
    }

    #[gpui::test]
    fn the_catches_come_from_the_daemon_at_connect(cx: &mut TestAppContext) {
        let socket = catches_reply(vec![Catch {
            creature: "vespa-crabro".to_owned(),
            first_caught_at: 9,
            count: 2,
        }]);
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        let loaded = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| {
                workspace.data.caught_creatures == ["vespa-crabro"]
            })
        });

        assert!(loaded, "the catches never loaded");
    }

    #[gpui::test]
    fn a_catch_is_recorded_by_the_daemon(cx: &mut TestAppContext) {
        let socket = catches_reply(Vec::new());
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });

        workspace.update(cx, |workspace, cx| {
            workspace.spawn_creature(0, cx);
            workspace.catch_creature(cx);
        });
        let recorded = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| {
                workspace
                    .field_guide
                    .catch_of("apis-mellifera")
                    .is_some_and(|catch| catch.count == 4)
            })
        });

        assert!(recorded, "the daemon's answer never arrived");
        assert_eq!(caught_slugs(&workspace, cx), ["apis-mellifera"]);
    }

    #[test]
    fn the_cap_holds_with_the_start_times_the_daemon_kept() {
        let mut schedule = Schedule::default();
        let now = 10 * millis(DAY);
        let kept: Vec<u64> = (1..=4).map(|hours| now - hours * 3_600_000).collect();

        schedule.remember(&kept);

        let mut came = false;
        for minute in 0..120 {
            came |= schedule.tick(now + minute * MINUTE, true, 0);
        }
        assert!(!came);
    }

    #[test]
    fn the_gap_holds_with_the_start_time_the_daemon_kept() {
        let mut schedule = Schedule::default();
        let now = 10 * millis(DAY);
        schedule.remember(&[now - 10 * MINUTE]);

        let mut came = Vec::new();
        for minute in 0..60 {
            if schedule.tick(now + minute * MINUTE, true, 0) {
                came.push(minute);
            }
        }

        assert!(came.iter().all(|minute| *minute >= 35), "{came:?}");
    }

    #[test]
    fn remembering_a_start_twice_keeps_it_once() {
        let mut schedule = Schedule::default();

        schedule.remember(&[5, 3]);
        schedule.remember(&[3, 5, 9]);

        assert_eq!(schedule.starts, [3, 5, 9]);
    }

    #[gpui::test]
    fn the_start_times_come_from_the_daemon_at_connect(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::Visits { call } => Some(Event::Visits {
                call,
                starts: vec![1, 2, 3, 4],
            }),
            _ => None,
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        let loaded = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| {
                workspace.field_guide.schedule.starts == [1, 2, 3, 4]
            })
        });

        assert!(loaded, "the start times never loaded");
    }

    #[gpui::test]
    fn a_creature_that_comes_is_recorded_by_the_daemon(cx: &mut TestAppContext) {
        let (sent, visits) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::RecordVisit { call, at } => {
                sent.send(at).ok();

                Some(Event::Done { call })
            }
            _ => None,
        });
        let (workspace, cx) = setup(cx);
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        work(&workspace, AgentStatus::Working, cx);

        wait_for_a_creature(&workspace, cx);
        let recorded = wait_until(cx, |_| !visits.is_empty());

        assert!(recorded);
        assert_eq!(visits.try_recv().ok(), Some(1_000_000 + MINUTE));
    }

    #[gpui::test]
    fn a_catch_made_while_disconnected_is_sent_when_the_daemon_is_back(cx: &mut TestAppContext) {
        let (sent, catches) = flume::unbounded();
        let socket = fake_daemon_replying(move |request| match request {
            Request::RecordCatch { call, creature } => {
                sent.send(creature.clone()).ok();

                Some(Event::CatchRecorded {
                    call,
                    caught: Catch {
                        creature,
                        first_caught_at: 5,
                        count: 1,
                    },
                })
            }
            Request::Catches { call } => Some(Event::Catches {
                call,
                catches: Vec::new(),
            }),
            _ => None,
        });
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.spawn_creature(0, cx);
            workspace.catch_creature(cx);
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        let sent_back = wait_until(cx, |_| !catches.is_empty());

        assert!(sent_back);
        assert_eq!(catches.try_recv().ok().as_deref(), Some("apis-mellifera"));
    }

    #[gpui::test]
    fn loading_the_catches_keeps_the_ones_made_while_disconnected(cx: &mut TestAppContext) {
        let socket = fake_daemon_replying(|request| match request {
            Request::RecordCatch { call, .. } => Some(Event::Error {
                call: Some(call),
                id: None,
                message: "busy".into(),
            }),
            Request::Catches { call } => Some(Event::Catches {
                call,
                catches: Vec::new(),
            }),
            _ => None,
        });
        let (workspace, cx) = setup(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.spawn_creature(0, cx);
            workspace.catch_creature(cx);
        });
        cx.executor().allow_parking();
        let client = Client::connect(&socket).expect("connect to the fake daemon");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.use_daemon(client.clone(), &[], window, cx);
        });
        let requeued = wait_until(cx, |cx| {
            workspace.read_with(cx, |workspace, _| !workspace.field_guide.queued.is_empty())
        });

        assert!(requeued, "the unsent catch was dropped from the queue");
        assert_eq!(caught_slugs(&workspace, cx), ["apis-mellifera"]);
    }

    #[test]
    fn merging_keeps_the_higher_count_and_the_earlier_first_catch() {
        let daemon = vec![Catch {
            creature: "a".to_owned(),
            first_caught_at: 9,
            count: 1,
        }];
        let known = [
            Catch {
                creature: "a".to_owned(),
                first_caught_at: 4,
                count: 2,
            },
            Catch {
                creature: "b".to_owned(),
                first_caught_at: 7,
                count: 1,
            },
        ];

        let merged = merge_catches(daemon, &known);

        assert_eq!(merged.len(), 2);
        assert_eq!((merged[0].count, merged[0].first_caught_at), (2, 4));
        assert_eq!(merged[1].creature, "b");
    }
}
