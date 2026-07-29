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
//! # A failed delay is not a wake-up
//!
//! The delay future yields an `FdbResult`, and [`Timer::sleep`] has nowhere to
//! put a failure: the trait returns `()`, because the recipe treats waiting as
//! something that either happens or does not happen yet. So the failure has to
//! be expressed in the only vocabulary the signature has, and there are exactly
//! two candidates: return, or never return.
//!
//! This returns never, which is also what the simulator itself does. In
//! `sim2.cpp`, a delay belonging to a process the simulator has killed is
//! forwarded to `Never()` and simply never fires; the C++ code waiting on it
//! does not resume, and nothing after the wait runs. Parking here is that
//! behaviour reproduced across the FFI rather than an invention of ours.
//!
//! The alternative is worse for the same reason it looks harmless. A sleep that
//! resolved on a failed delay would be a lie the caller cannot detect, and the
//! caller is a loop: the recipe's campaign parks between rounds, and a park that
//! resolves instantly, every time, turns that loop into one bounded by how fast
//! the database answers rather than by the clock. That is the failure this crate
//! was hardened against, and it cost an fdbserver process gigabytes in seconds.
//!
//! # Where the error comes from at all
//!
//! Upstream's `delay()` has no failure mode. The errors this bridge sees,
//! `operation_cancelled` (1101) and `broken_promise` (1100), are manufactured by
//! the ExternalWorkload bridge's blanket catch and mean one thing: the flow-side
//! actor backing this delay is gone. In C++ that is not an error a workload
//! handles, it is the coroutine frame being destroyed, and every upstream
//! workload rethrows `actor_cancelled` immediately rather than continuing. Ending
//! the role, which is what the [`roles`](super::roles) side does with the same
//! error, is the faithful translation of that; parking is what this side does
//! because it has no way to say it.
//!
//! Never returning is honest and, here, safe. A delay that fails means the
//! simulator is tearing this client down, so "the wait never ends" is what
//! actually happened. Nothing deadlocks on it: every use of this timer sits
//! inside [`elector_role`](super::elector_role), which races the whole of
//! `LeaderElector::lead` against a give-up delay of its own. If delays are
//! failing, that one fails too and resolves immediately, so the role ends and
//! says why. If delays are working, this timer behaves exactly as before.
//!
//! What it must not do is retry. A loop that re-issues a failing delay is the
//! same hot loop wearing a different hat.
//!
//! # The clean fix, deliberately not done
//!
//! All of this is working around a trait that cannot express failure. A
//! fallible `Timer::sleep` returning `FdbResult<()>`, or any signal the recipe's
//! campaign and renewal loops could branch on, would let the elector decide for
//! itself what a broken timer means instead of leaving this impl to pick the
//! least dishonest `()`. That is a change to the recipe's public API, and it is
//! not being made as part of a hardening pass; parking is sound in the meantime
//! and this note is here so the next person does not mistake it for a design.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use foundationdb::recipes::leader_election::Timer;
use foundationdb_simulation::{Severity, WorkloadContext, details};
use futures::future::BoxFuture;

/// The context, plus whether this timer has already complained
///
/// Behind an [`Arc`] so that the sleep future can report a failure it only
/// learns about after the call that created it has returned. That is the one
/// place a context copy is allowed into a `'static` future here, and it is
/// sound for the same reason the copy itself is: the futures this timer hands
/// out are owned by the `LeaderElector`, which
/// [`elector_role::run`](super::elector_role::run) drops before its phase
/// returns, so none of them can outlive the workload instance. The eager
/// creation of the delay itself is unchanged.
struct Sink {
    context: WorkloadContext,
    reported: AtomicBool,
}

// SAFETY: the wrapped context is a bundle of raw pointers into fdbserver, which
// drives every workload callback on a single thread, so the context is never
// accessed concurrently. This is the same justification `SimClock` and `SimRng`
// carry in `foundationdb_simulation::env`, and ultimately the one on
// `Clone for WorkloadContext`.
unsafe impl Send for Sink {}
// SAFETY: see the `Send` impl above.
unsafe impl Sync for Sink {}

impl Sink {
    /// Say that a wait did not happen, the first time it does not
    ///
    /// Once per timer: a client whose delays have stopped working will fail
    /// every subsequent sleep too, and a trace per failure would bury the run.
    fn report_once(&self) {
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }
        self.context.trace(
            Severity::WarnAlways,
            "LeaderElectionSimTimerDelayFailed",
            details![
                "Detail" => "a delay failed, so this sleep will never complete; \
                             the role's own deadline race is what ends it"
            ],
        );
    }
}

/// A [`Timer`] over the simulated timeline
///
/// Holds a copy of the fdbserver-owned workload context, with the same lifetime
/// caveat as [`SimClock`](foundationdb_simulation::SimClock): keep one no
/// longer than the workload instance it was built from.
pub(crate) struct SimTimer {
    sink: Arc<Sink>,
}

impl SimTimer {
    /// Wait on the schedule of `context`
    pub(crate) fn new(context: &WorkloadContext) -> Self {
        Self {
            sink: Arc::new(Sink {
                context: context.clone(),
                reported: AtomicBool::new(false),
            }),
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
        // future: the delay reads the context eagerly. See the module
        // documentation.
        let delay = self.sink.context.delay(duration);
        let sink = Arc::clone(&self.sink);
        Box::pin(async move {
            if delay.await.is_err() {
                // The wait did not happen, so this sleep does not end. See
                // "A failed delay is not a wake-up".
                sink.report_once();
                futures::future::pending::<()>().await;
            }
        })
    }
}
