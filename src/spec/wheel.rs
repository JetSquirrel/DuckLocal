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
//! - Latching: one gesture — a burst of events, trackpad momentum included —
//!   stays with the scroller it started on. A dashboard scroll that carries
//!   a table under the pointer keeps scrolling the dashboard, instead of
//!   stopping dead when the table catches it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{point, ScrollHandle, ScrollWheelEvent, Window};

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

/// The gesture in progress, shared by the stack and each table's handler.
#[derive(Clone, Default)]
pub struct WheelLatch(Rc<Cell<Option<Latch>>>);

impl WheelLatch {
    /// The target of the gesture `now` continues, if it continues one.
    fn current(&self, now: Instant) -> Option<Target> {
        self.0
            .get()
            .filter(|latch| now.saturating_duration_since(latch.at) < GESTURE_GAP)
            .map(|latch| latch.target)
    }

    fn hold(&self, target: Target, now: Instant) {
        self.0.set(Some(Latch { target, at: now }));
    }

    /// The stack's own handler: it only sees events no table kept, so any
    /// event reaching it is the stack's.
    pub fn stack_scrolled(&self) {
        self.hold(Target::Stack, Instant::now());
    }

    /// The table of plot `ix` has already applied `event` to its rows (its
    /// listener runs before this one); decide whether it keeps the event.
    /// Returns true when propagation should stop — the stack must not move.
    pub fn table_scrolled(
        &self,
        ix: usize,
        handle: &ScrollHandle,
        event: &ScrollWheelEvent,
        window: &Window,
    ) -> bool {
        let now = Instant::now();
        let delta = event.delta.pixel_delta(window.line_height());
        let offset = handle.offset();
        let before = offset.y - delta.y;
        let target = self.current(now).unwrap_or_else(|| {
            if keeps(
                before.into(),
                handle.max_offset().y.into(),
                delta.x.into(),
                delta.y.into(),
            ) {
                Target::Table(ix)
            } else {
                Target::Stack
            }
        });
        self.hold(target, now);
        if target != Target::Table(ix) {
            // Not this table's gesture: take back what its listener did. Only
            // the rows move on this handle; the columns scroll on another.
            handle.set_offset(point(offset.x, before));
        }
        target != Target::Stack
    }
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

#[cfg(test)]
mod tests {
    use super::{keeps, Target, WheelLatch, GESTURE_GAP};
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
