//! Which scroller a wheel gesture moves, when a `table` plot — itself
//! scrollable — sits inside the dashboard's scrolling stack.
//!
//! GPUI hands a wheel event to every scroll container under the pointer and
//! none of them stops it, so a swipe over a table moved the table's rows and
//! the whole dashboard at once, and the two fought. Browsers settle this with
//! two rules, and so does this:
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
//! often with no movement yet (fingers down, nothing moved). So a gesture is
//! decided by its first event that moves, and a new `Started` decides afresh
//! even while the last swipe's momentum is still streaming in. A mouse wheel
//! reports no phases; for it, a pause between events is what ends a gesture.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{point, Bounds, Pixels, ScrollHandle, ScrollWheelEvent, TouchPhase, Window};

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

/// The gesture in progress, shared by the stack and each table's handler,
/// and where the stack's viewport was last laid out.
#[derive(Clone, Default)]
pub struct WheelLatch {
    latch: Rc<Cell<Option<Latch>>>,
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
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

    /// The cell the stack records its viewport's bounds in, as it paints.
    pub fn viewport(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.viewport.clone()
    }

    /// The stack's own handler: it only sees events no table kept, so any
    /// event reaching it that moves is the stack's.
    pub fn stack_scrolled(&self, event: &ScrollWheelEvent, window: &Window) {
        let delta = event.delta.pixel_delta(window.line_height());
        if delta.x != Pixels::ZERO || delta.y != Pixels::ZERO {
            self.hold(Target::Stack, Instant::now());
        }
    }

    /// The table of plot `ix`, laid out at `table`, has already applied
    /// `event` to its rows (its listener runs before this one); decide
    /// whether it keeps the event. Returns true when propagation should stop
    /// — the stack must not move.
    pub fn table_scrolled(
        &self,
        ix: usize,
        handle: &ScrollHandle,
        table: Option<Bounds<Pixels>>,
        event: &ScrollWheelEvent,
        window: &Window,
    ) -> bool {
        let now = Instant::now();
        let delta = event.delta.pixel_delta(window.line_height());
        let offset = handle.offset();
        let before = offset.y - delta.y;
        let latched = self.current(now);
        let moved = delta.x != Pixels::ZERO || delta.y != Pixels::ZERO;
        let revealed = match (table, self.viewport.get()) {
            (Some(table), Some(viewport)) => reveals(
                (table.top().into(), table.bottom().into()),
                (viewport.top().into(), viewport.bottom().into()),
                delta.x.into(),
                delta.y.into(),
            ),
            // Not laid out yet: nothing to reveal.
            _ => true,
        };
        let keeps = revealed
            && keeps(
                before.into(),
                handle.max_offset().y.into(),
                delta.x.into(),
                delta.y.into(),
            );
        let started = event.touch_phase == TouchPhase::Started;
        let Some(target) = decide(latched, started, moved, keeps, ix) else {
            // Nothing moved, so there is nothing to take back, and no reason
            // to change hands.
            return latched.is_some_and(|target| target != Target::Stack);
        };
        self.hold(target, now);
        if target != Target::Table(ix) {
            // Not this table's gesture: take back what its listener did. Only
            // the rows move on this handle; the columns scroll on another.
            handle.set_offset(point(offset.x, before));
        }
        target != Target::Stack
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
    use std::time::{Duration, Instant};

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
}
