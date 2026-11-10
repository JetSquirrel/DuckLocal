//! Which scroller a wheel gesture moves, when a `table` plot — itself
//! scrollable — sits inside the dashboard's scrolling stack.
//!
//! GPUI Kit gives a wheel event to the innermost scroll area under the
//! pointer, whole: a table that can scroll at all takes every event over it,
//! at its ends too, in the capture phase, and nothing reaches the dashboard.
//! Left alone, a dashboard scroll stops dead the moment a table slides under
//! the pointer. So the gesture is routed here first, by a capture listener
//! painted before the stack and its tables: an event that is the dashboard's
//! moves the stack and goes no further; one that is a table's is let through
//! to the table. Browsers settle whose it is with two rules, and so does
//! this:
//!
//! - Chaining: the inner scroller takes the wheel until it is at its edge in
//!   that direction; then the outer one does.
//! - Latching: one gesture stays with the scroller it started on, trackpad
//!   momentum included. A dashboard scroll that carries a table under the
//!   pointer keeps scrolling the dashboard, instead of stopping dead when the
//!   table catches it.
//!
//! - Revealing: a table takes a vertical gesture only once its edge in that
//!   direction is on screen. Scrolling down onto a table moves the dashboard
//!   until the table's bottom shows, and only then the rows — so the table
//!   arrives whole, rather than catching the wheel the moment its top edge
//!   slides under the pointer and holding the dashboard still mid-table.
//!
//! A trackpad says where a gesture begins: its first event is `Started`,
//! often with no movement yet (fingers down, nothing moved — macOS reports
//! both `MayBegin` and `Began` as `Started`). So a gesture is decided by its
//! first event that moves after a `Started`, and that decides afresh even
//! while the last swipe's momentum is still streaming in: the latch is still
//! live then, and without remembering the `Started` a run of quick swipes
//! would all stay with whatever the first one chose — a table already at its
//! edge would hold the dashboard still until the momentum died. A mouse wheel
//! reports no phases; for it, a pause between events is what ends a gesture.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{px, Bounds, HitboxId, Pixels, ScrollEvent, ScrollHandle, TouchPhase, Window};

/// Events closer together than this belong to one gesture. Momentum arrives
/// every frame; a wheel's separate clicks a person means as one move come
/// well inside this too.
const GESTURE_GAP: Duration = Duration::from_millis(150);

/// Slack at the edges, so rounding does not leave a scroller "not quite at
/// the end" forever.
const EDGE: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The dashboard's own stack.
    Stack,
    /// The table of the plot at this index.
    Table(usize),
}

#[derive(Clone, Copy, Debug)]
struct Latch {
    target: Target,
    at: Instant,
}

/// A table plot as last painted: where it is, and the handle its rows
/// scroll on.
#[derive(Clone)]
struct Spot {
    ix: usize,
    hitbox: HitboxId,
    bounds: Bounds<Pixels>,
    rows: ScrollHandle,
}

/// The gesture in progress, the stack's scroll position, and where the
/// stack's viewport and its tables were last laid out.
#[derive(Clone, Default)]
pub struct WheelLatch {
    latch: Rc<Cell<Option<Latch>>>,
    /// A `Started` arrived and nothing has moved since: the next event that
    /// moves begins a new gesture.
    fresh: Rc<Cell<bool>>,
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    stack: ScrollHandle,
    tables: Rc<RefCell<Vec<Spot>>>,
}

impl WheelLatch {
    /// The target of the gesture `now` continues, if it continues one.
    fn current(&self, now: Instant) -> Option<Target> {
        self.latch
            .get()
            .filter(|latch| now.saturating_duration_since(latch.at) < GESTURE_GAP)
            .map(|latch| latch.target)
    }

    fn hold(&self, target: Target, now: Instant) {
        self.latch.set(Some(Latch { target, at: now }));
    }

    /// Whether `event` begins a gesture: it is the `Started` itself, or the
    /// first event to move after one. An event that does not move only notes
    /// a `Started`, for the move that follows.
    fn begins(&self, event: &ScrollEvent, moved: bool) -> bool {
        let started = event.touch_phase == TouchPhase::Started;
        if !moved {
            if started {
                self.fresh.set(true);
            }
            return false;
        }
        self.fresh.replace(false) || started
    }

    /// The cell the stack records its viewport's bounds in, as it paints.
    pub fn viewport(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.viewport.clone()
    }

    /// The handle the stack scrolls on.
    pub fn stack(&self) -> &ScrollHandle {
        &self.stack
    }

    /// A new frame of the stack is being laid out: its tables report in
    /// again as each is, and one no longer drawn is forgotten.
    pub fn begin_frame(&self) {
        self.tables.borrow_mut().clear();
    }

    /// The table of plot `ix` was laid out at `bounds`, behind `hitbox`.
    pub fn table_laid_out(
        &self,
        ix: usize,
        hitbox: HitboxId,
        bounds: Bounds<Pixels>,
        rows: ScrollHandle,
    ) {
        self.tables.borrow_mut().push(Spot {
            ix,
            hitbox,
            bounds,
            rows,
        });
    }

    /// Decide whose `event` is, in the capture phase, before any scroll area
    /// has seen it. Returns true when it has been dealt with here — the
    /// stack moved, or the event belongs to a gesture that has left its
    /// table — and must go no further; false leaves it to whichever area is
    /// under the pointer.
    pub fn route(&self, event: &ScrollEvent, window: &Window) -> bool {
        let Some(viewport) = self.viewport.get() else {
            return false;
        };
        if !viewport.contains(&event.position) {
            return false;
        }
        let now = Instant::now();
        let delta = event.delta.pixel_delta(window.line_height());
        let moved = delta.x != Pixels::ZERO || delta.y != Pixels::ZERO;
        let latched = self.current(now);
        let started = self.begins(event, moved);
        // Only a table with rows to scroll takes the wheel from the stack;
        // any other leaves it where it was going anyway.
        let spot = self
            .tables
            .borrow()
            .iter()
            .find(|spot| {
                spot.hitbox.should_handle_scroll(window) && spot.rows.max_offset().y > px(0.)
            })
            .cloned();
        let Some(spot) = spot else {
            if !moved {
                return false;
            }
            // Momentum from a table's gesture, with the pointer since moved
            // off the table: it is not the stack's to spend.
            if !started && matches!(latched, Some(Target::Table(_))) {
                self.hold_from(latched, now);
                return true;
            }
            self.hold(Target::Stack, now);
            return false;
        };
        let keeps = reveals(
            (spot.bounds.top().into(), spot.bounds.bottom().into()),
            (viewport.top().into(), viewport.bottom().into()),
            delta.x.into(),
            delta.y.into(),
        ) && keeps(
            spot.rows.offset().y.into(),
            spot.rows.max_offset().y.into(),
            delta.x.into(),
            delta.y.into(),
        );
        let Some(target) = decide(latched, started, moved, keeps, spot.ix) else {
            // Nothing moved, so there is nothing to route.
            return false;
        };
        self.hold(target, now);
        match target {
            Target::Table(ix) => ix != spot.ix,
            Target::Stack => {
                let mut offset = self.stack.offset();
                let max = self.stack.max_offset().y;
                offset.y = (offset.y + delta.y).clamp(-max, px(0.));
                self.stack.set_offset(offset);
                true
            }
        }
    }

    fn hold_from(&self, latched: Option<Target>, now: Instant) {
        if let Some(target) = latched {
            self.hold(target, now);
        }
    }
}

/// Who an event over table `ix` goes to. `None` for an event that does not
/// move: fingers landing on a trackpad say a gesture has begun, not where it
/// is going, and deciding then would hand every trackpad gesture over a
/// table to the stack.
fn decide(
    latched: Option<Target>,
    started: bool,
    moved: bool,
    keeps: bool,
    ix: usize,
) -> Option<Target> {
    if !moved {
        return None;
    }
    Some(match latched {
        Some(target) if !started => target,
        _ if keeps => Target::Table(ix),
        _ => Target::Stack,
    })
}

/// Whether an inner scroller at vertical offset `before` (GPUI's: 0 at the
/// top, `-max` at the bottom) should take a gesture that starts with this
/// delta: always for a sideways swipe, which the stack cannot use, and for a
/// vertical one while it has room to move that way.
fn keeps(before: f32, max: f32, dx: f32, dy: f32) -> bool {
    if dy.abs() < dx.abs() {
        return true;
    }
    if dy < 0.0 {
        before > -max + EDGE
    } else if dy > 0.0 {
        before < -EDGE
    } else {
        false
    }
}

/// Whether a table spanning `table` (top, bottom) shows its edge in the
/// direction of this delta inside a viewport spanning `viewport`: its bottom
/// for a downward scroll (negative `dy`), its top for an upward one. A
/// sideways swipe needs nothing revealed; the stack cannot use it.
fn reveals(table: (f32, f32), viewport: (f32, f32), dx: f32, dy: f32) -> bool {
    if dy.abs() < dx.abs() {
        return true;
    }
    if dy < 0.0 {
        table.1 <= viewport.1 + REVEAL_SLACK
    } else if dy > 0.0 {
        table.0 >= viewport.0 - REVEAL_SLACK
    } else {
        true
    }
}

/// How far an edge may sit outside the viewport and still count as shown:
/// the stack's own edge rounding, and the pixel of a border.
const REVEAL_SLACK: f32 = 2.0;

#[cfg(test)]
mod tests {
    use super::{decide, keeps, reveals, Target, WheelLatch, GESTURE_GAP};
    use gpui_kit::{ScrollEvent, TouchPhase};
    use std::time::{Duration, Instant};

    fn phase(touch_phase: TouchPhase) -> ScrollEvent {
        ScrollEvent {
            touch_phase,
            ..Default::default()
        }
    }

    const MAX: f32 = 400.0;

    #[test]
    fn a_table_keeps_the_wheel_until_its_edge() {
        // Scrolling down (negative delta) from the top, and in the middle.
        assert!(keeps(0.0, MAX, 0.0, -20.0));
        assert!(keeps(-200.0, MAX, 0.0, -20.0));
        // At the bottom, down goes to the stack; up stays.
        assert!(!keeps(-MAX, MAX, 0.0, -20.0));
        assert!(keeps(-MAX, MAX, 0.0, 20.0));
        // At the top, up goes to the stack.
        assert!(!keeps(0.0, MAX, 0.0, 20.0));
    }

    #[test]
    fn a_table_takes_the_wheel_once_it_is_on_screen_that_way() {
        let viewport = (0.0, 800.0);
        // Scrolling down onto a table whose bottom is still below the fold:
        // the dashboard keeps moving.
        assert!(!reveals((600.0, 900.0), viewport, 0.0, -20.0));
        // Its bottom is in view: the table may take it.
        assert!(reveals((480.0, 780.0), viewport, 0.0, -20.0));
        assert!(reveals((501.0, 801.0), viewport, 0.0, -20.0));
        // Scrolling up onto a table whose top is above the fold.
        assert!(!reveals((-100.0, 200.0), viewport, 0.0, 20.0));
        assert!(reveals((10.0, 310.0), viewport, 0.0, 20.0));
        // Sideways needs nothing revealed.
        assert!(reveals((600.0, 900.0), viewport, -30.0, 2.0));
    }

    #[test]
    fn a_table_that_does_not_scroll_passes_every_vertical_gesture_on() {
        assert!(!keeps(0.0, 0.0, 0.0, -20.0));
        assert!(!keeps(0.0, 0.0, 0.0, 20.0));
    }

    #[test]
    fn a_sideways_swipe_stays_with_the_table() {
        // The stack would read a sideways delta as vertical; the table's
        // columns are what a sideways swipe means.
        assert!(keeps(-MAX, MAX, 30.0, -2.0));
        assert!(keeps(0.0, 0.0, -30.0, 0.0));
    }

    #[test]
    fn rounding_at_an_edge_counts_as_the_edge() {
        assert!(!keeps(-MAX + 0.2, MAX, 0.0, -20.0));
        assert!(!keeps(-0.2, MAX, 0.0, 20.0));
    }

    #[test]
    fn fingers_landing_decide_nothing() {
        // A trackpad's first event has no movement: it must not latch the
        // gesture to the stack before the table has had a say.
        assert_eq!(decide(None, true, false, false, 3), None);
        assert_eq!(decide(Some(Target::Stack), true, false, false, 3), None);
        // The first event that moves decides.
        assert_eq!(decide(None, false, true, true, 3), Some(Target::Table(3)));
    }

    #[test]
    fn a_new_swipe_decides_afresh_while_momentum_streams() {
        // Momentum from a dashboard scroll is still arriving (the latch is
        // live), and a new swipe begins over a table that can scroll.
        assert_eq!(
            decide(Some(Target::Stack), true, true, true, 3),
            Some(Target::Table(3))
        );
        // ... and one at the table's edge goes to the stack.
        assert_eq!(
            decide(Some(Target::Table(3)), true, true, false, 3),
            Some(Target::Stack)
        );
        // Within a gesture, the latch holds either way.
        assert_eq!(
            decide(Some(Target::Stack), false, true, true, 3),
            Some(Target::Stack)
        );
        assert_eq!(
            decide(Some(Target::Table(3)), false, true, false, 3),
            Some(Target::Table(3))
        );
    }

    #[test]
    fn a_gesture_stays_with_its_target_until_it_pauses() {
        let latch = WheelLatch::default();
        let start = Instant::now();
        assert_eq!(latch.current(start), None);
        latch.hold(Target::Stack, start);
        assert_eq!(
            latch.current(start + Duration::from_millis(16)),
            Some(Target::Stack)
        );
        assert_eq!(latch.current(start + GESTURE_GAP), None);
    }

    #[test]
    fn a_still_started_makes_the_next_move_begin_a_gesture() {
        let latch = WheelLatch::default();
        // Momentum from the last swipe: moving, and begins nothing.
        assert!(!latch.begins(&phase(TouchPhase::Moved), true));
        // Fingers land with no movement (macOS's MayBegin), so the latch is
        // still live when the first move comes — which must decide afresh.
        assert!(!latch.begins(&phase(TouchPhase::Started), false));
        assert!(latch.begins(&phase(TouchPhase::Moved), true));
        // ... and only that one.
        assert!(!latch.begins(&phase(TouchPhase::Moved), true));
        // A Started that moves begins a gesture by itself.
        assert!(latch.begins(&phase(TouchPhase::Started), true));
        assert!(!latch.begins(&phase(TouchPhase::Moved), true));
    }

    /// A stack with one scrolling area inside it, wired as the dashboard
    /// wires a table plot: GPUI Kit's own bounce around both, the route
    /// listener painted first.
    mod routed {
        use super::super::WheelLatch;
        use gpui_kit::base::ScrollBounce;
        use gpui_kit::{
            canvas, div, point, px, size, Context, DispatchPhase, HitboxBehavior,
            InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, ScrollDelta,
            ScrollEvent, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
            TestAppContext, TouchPhase, VisualTestContext, Window,
        };

        struct Page {
            wheel: WheelLatch,
            rows: ScrollHandle,
        }

        impl Render for Page {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let (frame, route, spot) =
                    (self.wheel.clone(), self.wheel.clone(), self.wheel.clone());
                let viewport = self.wheel.viewport();
                let stack = self.wheel.stack().clone();
                let rows = self.rows.clone();
                let handle = rows.clone();
                div()
                    .w(px(400.))
                    .h(px(400.))
                    .relative()
                    .child(
                        canvas(
                            move |_, _, _| frame.begin_frame(),
                            move |_, _, window, _| {
                                window.on_mouse_event(
                                    move |event: &ScrollEvent, phase, window, cx| {
                                        if phase == DispatchPhase::Capture
                                            && route.route(event, window)
                                        {
                                            cx.stop_propagation();
                                            window.refresh();
                                        }
                                    },
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(ScrollBounce::new(
                        "stack-bounce",
                        &stack,
                        div()
                            .id("stack")
                            .size_full()
                            .track_scroll(&stack)
                            .overflow_y_scroll()
                            .child(div().h(px(300.)))
                            .child(
                                div()
                                    .h(px(200.))
                                    .w_full()
                                    .relative()
                                    .child(ScrollBounce::new(
                                        "rows-bounce",
                                        &rows,
                                        div()
                                            .id("rows")
                                            .size_full()
                                            .track_scroll(&rows)
                                            .overflow_y_scroll()
                                            .child(div().h(px(1000.))),
                                    ))
                                    .child(
                                        canvas(
                                            move |b, window, _| {
                                                let hitbox =
                                                    window.insert_hitbox(b, HitboxBehavior::Normal);
                                                spot.table_laid_out(
                                                    0,
                                                    hitbox.id,
                                                    b,
                                                    handle.clone(),
                                                );
                                            },
                                            |_, _, _, _| {},
                                        )
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full(),
                                    ),
                            )
                            .child(div().h(px(1000.))),
                    ))
                    .child(
                        canvas(move |b, _, _| viewport.set(Some(b)), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                    )
            }
        }

        fn swipe(cx: &mut VisualTestContext, y: f32, dy: f32, touch_phase: TouchPhase) {
            cx.simulate_event(ScrollEvent {
                position: point(px(100.), px(y)),
                delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                touch_phase,
                ..Default::default()
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn open(cx: &mut TestAppContext) -> (ScrollHandle, ScrollHandle, &mut VisualTestContext) {
            cx.update(gpui_kit::init);
            let (page, cx) = cx.add_window_view(|_, _| Page {
                wheel: WheelLatch::default(),
                rows: ScrollHandle::default(),
            });
            cx.simulate_resize(size(px(400.), px(400.)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let (stack, rows) = page.read_with(cx, |page, _| {
                (page.wheel.stack().clone(), page.rows.clone())
            });
            (stack, rows, cx)
        }

        fn y(handle: &ScrollHandle) -> Pixels {
            handle.offset().y
        }

        /// The table starts half below the fold (300..500 in a 400 viewport).
        /// One swipe down that begins over it must carry the dashboard on
        /// past the point where the table is whole — not stop there.
        #[gpui_kit::test]
        fn a_dashboard_scroll_runs_on_over_a_table(cx: &mut TestAppContext) {
            let (stack, rows, cx) = open(cx);
            swipe(cx, 350., 0., TouchPhase::Started);
            for _ in 0..8 {
                swipe(cx, 350., -40., TouchPhase::Moved);
            }
            assert_eq!(y(&stack), px(-320.));
            assert_eq!(y(&rows), px(0.));
        }

        /// A swipe that begins on a table shown whole scrolls its rows; one
        /// that begins with the rows at their end scrolls the dashboard.
        #[gpui_kit::test]
        fn a_table_at_its_end_hands_the_next_swipe_to_the_dashboard(cx: &mut TestAppContext) {
            let (stack, rows, cx) = open(cx);
            // Bring the table whole into view: it now spans 150..350.
            swipe(cx, 50., 0., TouchPhase::Started);
            swipe(cx, 50., -150., TouchPhase::Moved);
            swipe(cx, 50., 0., TouchPhase::Ended);
            assert_eq!(y(&stack), px(-150.));

            swipe(cx, 250., 0., TouchPhase::Started);
            swipe(cx, 250., -300., TouchPhase::Moved);
            assert_eq!(y(&rows), px(-300.));
            assert_eq!(y(&stack), px(-150.));
            // The rest of the rows, and then some: the gesture stays the
            // table's, which stops at its end.
            swipe(cx, 250., -900., TouchPhase::Moved);
            swipe(cx, 250., 0., TouchPhase::Ended);
            assert_eq!(y(&rows), px(-800.));
            assert_eq!(y(&stack), px(-150.));

            swipe(cx, 250., 0., TouchPhase::Started);
            swipe(cx, 250., -60., TouchPhase::Moved);
            assert_eq!(y(&rows), px(-800.));
            assert_eq!(y(&stack), px(-210.));
        }
    }
}
