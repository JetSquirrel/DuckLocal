//! The guide: the duck walks a new user through the window, one step at a
//! time, pointing at the part of the window each step is about.
//!
//! Each step names a [`Target`] — the Open button, the sidebar, Run, the
//! results — and the guide rings it, draws an arrow to it from its card, and
//! sets the card beside it. Where a target is is only known once it has been
//! laid out, so each target carries an [`anchor`]: an empty canvas that notes
//! its bounds while the guide is open. The guide draws from what the last
//! frame noted, and asks for another frame when this one's differ, so a
//! resized sidebar or a moved panel is followed a frame later. A target that
//! is not on screen (a collapsed sidebar, a dashboard in front) leaves its
//! step's card in the middle of the window, without an arrow.
//!
//! The guide opens once, at the first launch of someone with no history, and
//! again whenever it is asked for (DuckLocal menu, or the version's
//! right-click menu in the status bar). It sits above the window without
//! taking it over: only the card catches the pointer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::ui::scale::design;

/// The `settings` key set once the guide has been finished or skipped.
const DONE_SETTING: &str = "guide_done";

/// What a step points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The title bar's "Command line & AI" button.
    Ai,
    Open,
    Sidebar,
    Run,
    Results,
    /// The guide's own card, which the arrow starts from.
    Card,
}

/// Where a step's card goes relative to its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    Below,
    /// Below, its right edge under the target's: for a target at the
    /// window's right edge.
    BelowEnd,
    Right,
    Above,
    Center,
}

struct Step {
    target: Option<Target>,
    placement: Placement,
    mascot: &'static str,
    title: &'static str,
    body: &'static str,
}

const STEPS: [Step; 6] = [
    // First: what sets DuckLocal apart is an agent working in the same
    // window as the person, and this button is where that starts.
    Step {
        target: Some(Target::Ai),
        placement: Placement::BelowEnd,
        mascot: "duck-point-up-right",
        title: "tour.ai.title",
        body: "tour.ai.body",
    },
    Step {
        target: Some(Target::Open),
        placement: Placement::Below,
        mascot: "duck-drag-file",
        title: "tour.open.title",
        body: "tour.open.body",
    },
    Step {
        target: Some(Target::Sidebar),
        placement: Placement::Right,
        mascot: "duck-point-left",
        title: "tour.sidebar.title",
        body: "tour.sidebar.body",
    },
    Step {
        target: Some(Target::Run),
        placement: Placement::Below,
        mascot: "duck-point-up",
        title: "tour.run.title",
        body: "tour.run.body",
    },
    Step {
        target: Some(Target::Results),
        placement: Placement::Above,
        mascot: "duck-point-down",
        title: "tour.results.title",
        body: "tour.results.body",
    },
    Step {
        target: None,
        placement: Placement::Center,
        mascot: "duck-celebrate",
        title: "tour.done.title",
        body: "tour.done.body",
    },
];

/// Whether anchors note anything: only while the guide is open, so a closed
/// guide costs every frame nothing but the check.
static OPEN: AtomicBool = AtomicBool::new(false);

/// How many frames in a row the guide has redrawn to catch up with moved
/// anchors.
static CHASED: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The bounds the anchors noted this frame.
    static FRESH: RefCell<HashMap<Target, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The bounds the guide drew from.
    static SHOWN: RefCell<HashMap<Target, Bounds<Pixels>>> = RefCell::new(HashMap::new());
}

/// An empty, absolutely placed canvas over its parent that notes the parent's
/// bounds for the guide. The parent must be `relative`.
pub fn anchor(target: Target) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            if OPEN.load(Ordering::Relaxed) {
                FRESH.with(|fresh| fresh.borrow_mut().insert(target, bounds));
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Whether to open the guide at this launch: never finished or skipped, and
/// nothing in the history — someone who has run queries knows the window.
/// Reads the history store; call it off the UI thread.
pub fn offer_at_launch() -> bool {
    let done = crate::history::get_setting(DONE_SETTING)
        .ok()
        .flatten()
        .is_some();
    !done && crate::history::recent(1).is_ok_and(|entries| entries.is_empty())
}

/// The guide asks the window for an editor and its results, which the
/// first-run screen stands in for until there is data.
pub struct NeedsEditor;

impl EventEmitter<NeedsEditor> for Tour {}

/// The open guide, if any, for the menu's "Show the guide" to reach.
struct GlobalTour(WeakEntity<Tour>);

impl Global for GlobalTour {}

pub struct Tour {
    step: Option<usize>,
}

impl Tour {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.set_global(GlobalTour(cx.entity().downgrade()));
        Self { step: None }
    }

    /// Open the guide at its first step.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.go(0, cx);
    }

    /// The menu's "Show the guide".
    pub fn start_global(cx: &mut App) {
        if let Some(tour) = cx.try_global::<GlobalTour>().and_then(|g| g.0.upgrade()) {
            tour.update(cx, |tour, cx| tour.start(cx));
        }
    }

    fn go(&mut self, step: usize, cx: &mut Context<Self>) {
        OPEN.store(true, Ordering::Relaxed);
        self.step = Some(step);
        if matches!(STEPS[step].target, Some(Target::Run | Target::Results)) {
            cx.emit(NeedsEditor);
        }
        cx.notify();
    }

    /// Finished or skipped: closed, and not offered again at launch.
    fn close(&mut self, cx: &mut Context<Self>) {
        OPEN.store(false, Ordering::Relaxed);
        self.step = None;
        FRESH.with(|fresh| fresh.borrow_mut().clear());
        SHOWN.with(|shown| shown.borrow_mut().clear());
        cx.background_spawn(async {
            if let Err(error) = crate::history::set_setting(DONE_SETTING, "1") {
                tracing::warn!("Could not note the guide as seen: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    fn render_card(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let step = &STEPS[index];
        let last = index + 1 == STEPS.len();
        let mascot = if cx.theme().mode.is_dark() {
            format!("mascot/{}-dark.png", step.mascot)
        } else {
            format!("mascot/{}.png", step.mascot)
        };
        let body = match step.target {
            Some(Target::Run) => trf(step.body, &[RUN_KEYS]),
            None => trf(step.body, &[tr(AGAIN_HINT)]),
            Some(_) => tr(step.body).to_string(),
        };
        v_flex()
            .id("tour-card")
            .relative()
            .w(design(CARD_WIDTH))
            .p_4()
            .gap_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_lg()
            // The card is the only part of the guide that takes the pointer;
            // the window under the rest stays in reach.
            .occlude()
            .child(anchor(Target::Card))
            .child(
                h_flex().justify_center().child(
                    img(SharedString::from(mascot))
                        .h(design(MASCOT_HEIGHT))
                        .object_fit(ObjectFit::Contain),
                ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr(step.title)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(body),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(trf(
                                "tour.step",
                                &[&(index + 1).to_string(), &STEPS.len().to_string()],
                            )),
                    )
                    .child(div().flex_1())
                    .when(!last, |this| {
                        this.child(
                            Button::new("tour-skip")
                                .ghost()
                                .small()
                                .label(tr("tour.skip"))
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        )
                    })
                    .when(index > 0, |this| {
                        this.child(
                            Button::new("tour-back")
                                .outline()
                                .small()
                                .label(tr("tour.back"))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.go(index - 1, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("tour-next")
                            .primary()
                            .small()
                            .label(tr(if last { "tour.finish" } else { "tour.next" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if last {
                                    this.close(cx);
                                } else {
                                    this.go(index + 1, cx);
                                }
                            })),
                    ),
            )
    }
}

/// The card's width, and the duck's height on it, in design pixels.
const CARD_WIDTH: f32 = 340.;
const MASCOT_HEIGHT: f32 = 120.;
/// The room between a card and what it points at, which the arrow crosses.
const GAP: f32 = 36.;

/// Run's keystroke, as the toolbar's own hint writes it.
const RUN_KEYS: &str = if cfg!(target_os = "macos") {
    "⌘↵"
} else {
    "Ctrl+Enter"
};

/// Where the guide can be opened again: a menu bar on macOS, the version's
/// right-click menu elsewhere.
const AGAIN_HINT: &str = if cfg!(target_os = "macos") {
    "tour.again.menu"
} else {
    "tour.again.status_bar"
};

impl Render for Tour {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(index) = self.step else {
            return div().into_any_element();
        };
        // What the anchors noted last frame is what this frame draws from.
        let shown = FRESH.with(|fresh| std::mem::take(&mut *fresh.borrow_mut()));
        SHOWN.with(|kept| *kept.borrow_mut() = shown.clone());

        let step = &STEPS[index];
        let target = step.target.and_then(|target| shown.get(&target).copied());
        let card = shown.get(&Target::Card).copied();
        let viewport = window.viewport_size();
        let gap = design(GAP);

        // The card's corner, and which of its corners that is.
        let (anchor_corner, position) = match (target, step.placement) {
            (Some(t), Placement::Below) => (Anchor::TopLeft, point(t.left(), t.bottom() + gap)),
            (Some(t), Placement::BelowEnd) => (
                Anchor::TopRight,
                point(t.right() + px(8.), t.bottom() + gap),
            ),
            (Some(t), Placement::Right) => (
                Anchor::TopLeft,
                point(t.right() + gap, t.top() + design(72.)),
            ),
            (Some(t), Placement::Above) => (
                Anchor::BottomLeft,
                point(t.left() + design(48.), t.top() - gap),
            ),
            _ => (
                Anchor::TopCenter,
                point(viewport.width / 2., viewport.height * 0.22),
            ),
        };
        let arrow = target
            .zip(card)
            .and_then(|(t, card)| arrow_ends(step.placement, t, card, gap));
        let accent = cx.theme().primary;

        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                // The ring around the target and the arrow to it, in window
                // coordinates, which is what the anchors noted.
                .child(
                    canvas(
                        |_, _, _| {},
                        move |_, _, window, _| {
                            if let Some(t) = target {
                                let ring = Bounds::from_corners(
                                    t.origin + point(px(3.), px(3.)),
                                    t.bottom_right() - point(px(3.), px(3.)),
                                );
                                window.paint_quad(quad(
                                    ring,
                                    px(8.),
                                    transparent_black(),
                                    px(2.),
                                    accent,
                                    BorderStyle::Solid,
                                ));
                            }
                            if let Some((from, ctrl, to)) = arrow {
                                paint_arrow(from, ctrl, to, accent, window);
                            }
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .child(
                    anchored()
                        .anchor(anchor_corner)
                        .position(position)
                        .snap_to_window_with_margin(Edges::all(px(12.)))
                        .child(self.render_card(index, cx)),
                )
                // Last, after every anchor including the card's: if what they
                // noted this frame is not what was drawn, draw again.
                .child(
                    canvas(
                        |_, window, _| {
                            let moved = FRESH.with(|fresh| {
                                SHOWN.with(|shown| *fresh.borrow() != *shown.borrow())
                            });
                            // A refresh asked for mid-draw is dropped, so it
                            // goes on the next frame; a few in a row at most,
                            // in case something keeps moving.
                            let again = moved && CHASED.fetch_add(1, Ordering::Relaxed) < 8;
                            if !moved {
                                CHASED.store(0, Ordering::Relaxed);
                            }
                            if again {
                                window.on_next_frame(|window, _| window.refresh());
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_0(),
                ),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// The arrow from the card's edge facing the target to just short of the
/// target: its start, its curve's control point, and its tip. `None` when the
/// card has been pushed somewhere the arrow would cross it.
fn arrow_ends(
    placement: Placement,
    target: Bounds<Pixels>,
    card: Bounds<Pixels>,
    gap: Pixels,
) -> Option<(Point<Pixels>, Point<Pixels>, Point<Pixels>)> {
    let reach = gap * 0.3;
    let (from, to) = match placement {
        Placement::Below => {
            let to_x = target
                .center()
                .x
                .clamp(target.left() + px(12.), target.right());
            (
                point(card.left() + px(40.), card.top() - px(4.)),
                point(to_x, target.bottom() + reach),
            )
        }
        Placement::BelowEnd => {
            let to_x = target
                .center()
                .x
                .clamp(target.left(), target.right() - px(12.));
            (
                point(card.right() - px(48.), card.top() - px(4.)),
                point(to_x, target.bottom() + reach),
            )
        }
        Placement::Right => (
            point(card.left() - px(4.), card.top() + px(56.)),
            point(target.right() - px(10.), card.top() + px(28.)),
        ),
        Placement::Above => (
            point(card.left() + px(56.), card.bottom() + px(4.)),
            point(card.left() + px(32.), target.top() + reach),
        ),
        Placement::Center => return None,
    };
    // Only an arrow that runs the way the step points: one the window's edge
    // pushed the card past would cut back through it.
    let runs_right_way = match placement {
        Placement::Below | Placement::BelowEnd => to.y < from.y,
        Placement::Right => to.x < from.x,
        Placement::Above => to.y > from.y,
        Placement::Center => false,
    };
    if !runs_right_way {
        return None;
    }
    // A gentle bow: the control point sits off the straight line, to the side
    // the hand would swing.
    let mid = point((from.x + to.x) / 2., (from.y + to.y) / 2.);
    let bow = match placement {
        Placement::Right => point(px(0.), px(-14.)),
        Placement::BelowEnd => point(px(14.), px(0.)),
        _ => point(px(-14.), px(0.)),
    };
    Some((from, mid + bow, to))
}

/// A hand-drawn-looking arrow: a curved shaft and an open head.
fn paint_arrow(
    from: Point<Pixels>,
    ctrl: Point<Pixels>,
    to: Point<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let mut shaft = PathBuilder::stroke(px(2.));
    shaft.move_to(from);
    shaft.curve_to(to, ctrl);
    if let Ok(path) = shaft.build() {
        window.paint_path(path, color);
    }
    // The head's two strokes, folded back from the tip along the curve's
    // final direction.
    let dx = f32::from(to.x - ctrl.x);
    let dy = f32::from(to.y - ctrl.y);
    let length = (dx * dx + dy * dy).sqrt().max(1.);
    let (ux, uy) = (dx / length, dy / length);
    let side = 9.;
    let spread = 0.5_f32;
    let (sin, cos) = spread.sin_cos();
    let mut head = PathBuilder::stroke(px(2.));
    for sign in [1., -1.] {
        let (rx, ry) = (-ux * cos + sign * -uy * sin, -uy * cos + sign * ux * sin);
        head.move_to(to);
        head.line_to(point(to.x + px(rx * side), to.y + px(ry * side)));
    }
    if let Ok(path) = head.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::{arrow_ends, Placement, STEPS};
    use gpui_kit::{point, px, size, Bounds, Pixels};

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn an_arrow_runs_from_the_card_toward_its_target() {
        let open = bounds(60., 4., 80., 28.);
        let card = bounds(60., 68., 340., 300.);
        let (from, _, to) = arrow_ends(Placement::Below, open, card, px(36.)).unwrap();
        assert!(from.y > to.y && to.y > open.bottom());

        let sidebar = bounds(0., 40., 280., 800.);
        let card = bounds(316., 112., 340., 300.);
        let (from, _, to) = arrow_ends(Placement::Right, sidebar, card, px(36.)).unwrap();
        assert!(from.x > to.x && to.x < sidebar.right());
    }

    #[test]
    fn a_card_pushed_past_its_target_draws_no_arrow() {
        // The window's edge moved the card above the button it points up at.
        let run = bounds(600., 500., 80., 28.);
        let card = bounds(560., 100., 340., 300.);
        assert_eq!(arrow_ends(Placement::Below, run, card, px(36.)), None);
    }

    #[test]
    fn every_step_has_its_words_in_both_languages() {
        for step in &STEPS {
            for key in [step.title, step.body] {
                for lang in [crate::i18n::Language::En, crate::i18n::Language::Zh] {
                    assert_ne!(crate::i18n::trf_in(lang, key, &[]), key, "{key}");
                }
            }
        }
    }
}
