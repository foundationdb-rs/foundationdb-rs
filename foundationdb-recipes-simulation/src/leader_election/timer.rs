//! Waiting, as the simulator does it.
//!
//! The recipe's async handle layer reads time through a [`Clock`] and waits
//! through a [`Timer`], and the two are separate traits precisely so that a
//! caller on a simulated timeline can pair the simulator's clock with a timer
//! that drives the simulator's own schedule. [`SkewedClock`](super::clock::SkewedClock)
//! is the first half; this is the second.
//!
//! [`Clock`]: foundationdb::env::Clock
//!
//! # Why the delay is created eagerly
//!
//! [`Timer::sleep`] hands back a `'static` future, and the only thing that can
//! produce one here is [`WorkloadContext::delay`], which reads the
//! fdbserver-owned context. The context copy this holds is valid exactly as
//! long as the workload instance is, so it must never be moved into a future
//! that outlives the call: the future is built while `&self` is alive and only
//! the resulting `'static` future is boxed.
//!
//! # A failed delay is a spurious wake-up
//!
//! The delay future yields an `FdbResult`, and this discards it. A failure
//! therefore reads as waking early, which is sound because the elector never
//! treats a completed sleep as a safety fact: every decision it makes resamples
//! its clock, and the belief horizon is enforced against that reading rather
//! than against whichever timer happened to fire.

use std::fmt;
use std::time::Duration;

use foundationdb::recipes::leader_election::Timer;
use foundationdb_simulation::WorkloadContext;
use futures::future::BoxFuture;

/// A [`Timer`] over the simulated timeline
///
/// Holds a copy of the fdbserver-owned workload context, with the same lifetime
/// caveat as [`SimClock`](foundationdb_simulation::SimClock): keep one no
/// longer than the workload instance it was built from.
pub(crate) struct SimTimer {
    context: WorkloadContext,
}

// SAFETY: the wrapped context is a bundle of raw pointers into fdbserver, which
// drives every workload callback on a single thread, so the context is never
// accessed concurrently. This is the same justification `SimClock` and `SimRng`
// carry in `foundationdb_simulation::env`, and ultimately the one on
// `Clone for WorkloadContext`.
unsafe impl Send for SimTimer {}
// SAFETY: see the `Send` impl above.
unsafe impl Sync for SimTimer {}

impl SimTimer {
    /// Wait on the schedule of `context`
    pub(crate) fn new(context: &WorkloadContext) -> Self {
        Self {
            context: context.clone(),
        }
    }
}

impl fmt::Debug for SimTimer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SimTimer")
    }
}

impl Timer for SimTimer {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        // Created here, while `&self` is alive, and never inside the boxed
        // future: the context copy must not outlive this call. See the module
        // documentation.
        let delay = self.context.delay(duration);
        Box::pin(async move {
            // A failed delay is an early wake-up, and the caller resamples its
            // clock before it acts on anything.
            let _ = delay.await;
        })
    }
}
