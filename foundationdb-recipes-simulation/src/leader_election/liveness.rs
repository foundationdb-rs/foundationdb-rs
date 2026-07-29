//! Catching a loop that spins without the clock moving.
//!
//! Every role in this workload is a loop whose exit condition is simulated time
//! reaching a deadline. That is only an exit condition if each iteration
//! actually waits, and the waits are the part that can fail: the delay a role
//! parks on hands back an `FdbResult`, and a client the simulator is tearing
//! down gets an error instead of a wait. A loop that treats a failed wait as a
//! completed one stops being paced by simulated time and starts being paced by
//! how fast the process can issue transactions.
//!
//! That failure has been observed here, and it is expensive: a role spinning at
//! wall-clock speed commits a transaction per turn, and the log those
//! transactions write is what the check phase has to read back. One such loop
//! took fdbserver from megabytes to gigabytes in seconds.
//!
//! Every wait in this crate now reports its failure rather than swallowing it,
//! which is the real fix. This guard is the backstop for whatever that misses:
//! a loop that completes [`LivenessGuard::STALL_LIMIT`] iterations in a row
//! without simulated time advancing is not making progress, whatever it thinks
//! it is doing, and the honest thing is to say so loudly and stop.

use std::time::Duration;

/// A loop's own witness that simulated time is passing
///
/// Sampled at the top of each iteration. Equal readings mean the whole
/// iteration, waits included, took no simulated time at all.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LivenessGuard {
    /// The reading the previous iteration started from
    last: Option<Duration>,
    /// Consecutive iterations that started at the same reading
    stalls: u32,
}

impl LivenessGuard {
    /// How many stalled iterations in a row are one too many
    ///
    /// Three rather than one: a single pair of equal readings is ordinary. The
    /// simulator's clock only moves when something waits, and an iteration that
    /// legitimately did no waiting (a campaign round that found a pending
    /// injection and handed the step straight back, say) reads the same instant
    /// twice. Three in a row is a loop that has stopped waiting altogether.
    pub(crate) const STALL_LIMIT: u32 = 3;

    /// A guard that has seen nothing yet
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record the instant an iteration begins
    ///
    /// Returns whether the loop may go round again. `false` means simulated
    /// time has stood still across [`STALL_LIMIT`](Self::STALL_LIMIT)
    /// consecutive iterations, and the caller must end the role rather than
    /// take another turn.
    pub(crate) fn tick(&mut self, now: Duration) -> bool {
        self.stalls = match self.last {
            Some(last) if last == now => self.stalls + 1,
            _ => 0,
        };
        self.last = Some(now);
        self.stalls < Self::STALL_LIMIT
    }
}

/// Where the next tick of a paced loop falls, given the cursor and now
///
/// The arithmetic behind the pacing idiom the role loops use (FoundationDB's
/// own `delayUntil`): advance the cursor by one step, then wait out whatever is
/// left between now and it. Split out from the loops so the property the idiom
/// exists for can be tested without a simulator: the cursor always moves, so a
/// wait that completed instantly still leaves the next round asking for a real
/// one.
///
/// Saturating in both directions. `Sim2::delay` asserts on a negative argument,
/// and a cursor the work has already overrun must produce no wait rather than a
/// negative one.
pub(crate) fn next_tick(cursor: Duration, step: Duration, now: Duration) -> (Duration, Duration) {
    let cursor = cursor.saturating_add(step);
    (cursor, cursor.saturating_sub(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_cursor_that_keeps_moving_cannot_spin() {
        // The property the idiom is for. Simulated time never advances (every
        // wait returns instantly, as it would if delays were broken), and the
        // requested wait grows anyway, because the cursor moved and the clock
        // did not. One instant wait is possible; a second is not.
        let now = Duration::ZERO;
        let mut cursor = now;
        let mut waits = Vec::new();
        for _ in 0..5 {
            let (moved, wait) = next_tick(cursor, 10 * MS, now);
            cursor = moved;
            waits.push(wait);
        }
        assert_eq!(
            waits,
            vec![10 * MS, 20 * MS, 30 * MS, 40 * MS, 50 * MS],
            "a stalled clock must make the requested waits grow, not repeat"
        );
    }

    #[test]
    fn a_cursor_does_not_drift() {
        // A round whose work took time is followed by a shorter wait, not a
        // full step on top of it, so the loop keeps its average cadence.
        let (cursor, wait) = next_tick(Duration::ZERO, 10 * MS, 4 * MS);
        assert_eq!(cursor, 10 * MS);
        assert_eq!(wait, 6 * MS, "the work already spent 4ms of the step");
    }

    #[test]
    fn a_cursor_the_work_overran_asks_for_no_wait_rather_than_a_negative_one() {
        // `Sim2::delay` aborts the simulated process on a negative delay.
        // `Duration` is unsigned so this cannot compile into one, and the
        // saturation is what keeps it that way if the arithmetic ever moves.
        let (cursor, wait) = next_tick(Duration::ZERO, 10 * MS, 25 * MS);
        assert_eq!(cursor, 10 * MS);
        assert!(wait.is_zero());

        // And it catches up rather than staying behind: two more rounds and the
        // cursor is back in front of the clock.
        let (cursor, wait) = next_tick(cursor, 10 * MS, 25 * MS);
        assert!(wait.is_zero());
        let (_, wait) = next_tick(cursor, 10 * MS, 25 * MS);
        assert_eq!(wait, 5 * MS);
    }

    #[test]
    fn a_loop_whose_clock_moves_runs_forever() {
        let mut guard = LivenessGuard::new();
        for tick in 0..1_000u32 {
            assert!(guard.tick(MS * tick));
        }
    }

    #[test]
    fn a_loop_whose_clock_stands_still_is_stopped() {
        let mut guard = LivenessGuard::new();
        // The first reading establishes the baseline and can stall nothing.
        assert!(guard.tick(MS));
        // Then three iterations that took no simulated time at all.
        assert!(guard.tick(MS));
        assert!(guard.tick(MS));
        assert!(
            !guard.tick(MS),
            "a loop that went round {} times without the clock moving must stop",
            LivenessGuard::STALL_LIMIT
        );
    }

    #[test]
    fn a_stall_that_resolves_is_forgiven() {
        let mut guard = LivenessGuard::new();
        assert!(guard.tick(MS));
        assert!(guard.tick(MS));
        assert!(guard.tick(MS), "two stalls in a row are ordinary");

        // The clock moved, so the count starts over rather than accumulating
        // over the whole run: a role that stalls twice per lease for an hour is
        // not the failure this is looking for.
        assert!(guard.tick(2 * MS));
        assert!(guard.tick(2 * MS));
        assert!(guard.tick(2 * MS));
        assert!(!guard.tick(2 * MS));
    }

    #[test]
    fn the_clock_going_backwards_is_not_a_stall() {
        // Roles sample the simulator's own undistorted clock, so this should not
        // happen; if it ever does, it is progress of a sort and not the thing
        // this guard exists to catch.
        let mut guard = LivenessGuard::new();
        assert!(guard.tick(3 * MS));
        assert!(guard.tick(2 * MS));
        assert!(guard.tick(MS));
        assert!(guard.tick(Duration::ZERO));
    }
}
