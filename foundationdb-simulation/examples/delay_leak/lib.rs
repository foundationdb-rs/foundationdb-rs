//! Reproduction for a memory leak in `WorkloadContext::delay`.
//!
//! The workload awaits `iterations` very short simulated delays in a loop and
//! periodically reports two numbers:
//!
//! - `rust_live_bytes`: bytes currently allocated by Rust code in this cdylib,
//!   tracked by a counting global allocator. If this grows, the leak is on the
//!   Rust side (e.g. the waker `Arc` handed to `fdb_future_set_callback`).
//! - `rss_kib`: the resident set size of the whole `fdbserver` process, read
//!   from `/proc/self/statm`. If this grows while `rust_live_bytes` stays flat,
//!   the leak is on the C/C++ side (`ExternalWorkload::delay` / libfdb_c).
//!
//! Both are observational only: nothing branches on them.
//!
//! Options: `mode`, `iterations`, `reportEvery`, `delayMs` (default 0.1),
//! `dropDelayMs` (length of the delay that gets dropped, default 10x `delayMs`),
//! `settleMs` (if set, await this long after every report, then report again),
//! `rssLimitMiB` (select_ready stops once RSS grew this much, default 2048),
//! `client0Only` (default true; false runs the loop on every test client).
//!
//! Modes (option `mode`):
//! - `await` (default): `ctx.delay(d).await` each iteration.
//! - `drop`: create the delay future and drop it before it is ready (cancel path),
//!   without yielding in between.
//! - `drop_rounds`: like `drop`, but waits 5 simulated seconds at every report
//!   period so queued cancellations and timers can drain.
//! - `poll_drop`: poll a delay once (registering the C callback), drop it while
//!   pending (the `select!`/timeout case), then await a short delay.
//! - `drop_yield`: drop a pending delay (like a timeout losing a race), then await a
//!   short delay so the network thread processes the cancellation each iteration.
//! - `select_ready`: `select!` between an always-ready future and a fresh timeout:
//!   the loop never yields, so simulated time is frozen.
//! - `pump`: `select!` between a fresh timeout and a future that only needs a
//!   Rust-side wake: the deferred cancel callbacks keep re-polling the task.
//! - `per_poll`: a hand-rolled future that creates a delay on every poll.
//! - `grv`: control, await `get_read_version` instead of a delay.
//! - `none`: control run, no delay at all (yields nothing, just loops).

use std::{
    alloc::{GlobalAlloc, Layout, System},
    future::Future,
    sync::atomic::{AtomicIsize, Ordering},
    time::Duration,
};

use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, RustWorkloadFactory, Severity, SimDatabase, WorkloadContext,
    WrappedWorkload, register_factory,
};

struct CountingAlloc;
static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn rss_kib() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    pages * 4
}

/// Returns Pending once (waking itself right away), then Ready.
struct YieldOnce(bool);
impl Future for YieldOnce {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

struct DelayLeakWorkload {
    context: WorkloadContext,
    client_id: i32,
    iterations: u64,
    report_every: u64,
    delay: Duration,
    mode: String,
    rss_limit_kib: u64,
    settle_every_report: Option<Duration>,
    drop_delay: Duration,
    client0_only: bool,
    rss_start: u64,
    rss_end: u64,
    rust_start: isize,
    rust_end: isize,
}

impl DelayLeakWorkload {
    fn report(&self, i: u64) {
        let rss = rss_kib();
        let rust = LIVE.load(Ordering::Relaxed);
        println!(
            "delay_leak[{}] mode={} iter={i} rss_kib={rss} rust_live_bytes={rust}",
            self.client_id, self.mode
        );
        self.context.trace(
            Severity::Info,
            "DelayLeakProgress",
            &[
                ("Mode", self.mode.as_str()),
                ("Iteration", &i.to_string()),
                ("RssKiB", &rss.to_string()),
                ("RustLiveBytes", &rust.to_string()),
            ],
        );
    }
}

impl RustWorkload for DelayLeakWorkload {
    async fn setup(&mut self, _db: SimDatabase) {}

    async fn start(&mut self, db: SimDatabase) {
        if self.client0_only && self.client_id != 0 {
            return;
        }
        // Warm up allocator / FDB internal pools before taking the baseline.
        for _ in 0..1_000 {
            let _ = self.context.delay(self.delay).await;
        }
        self.rss_start = rss_kib();
        self.rust_start = LIVE.load(Ordering::Relaxed);
        self.report(0);
        // Wall-clock is observational only (how fast memory grows), nothing branches on it.
        let wall = std::time::Instant::now();
        for i in 1..=self.iterations {
            match self.mode.as_str() {
                "await" => self
                    .context
                    .delay(self.delay)
                    .await
                    .expect("delay future should resolve"),
                "drop" => drop(self.context.delay(self.delay)),
                "grv" => {
                    // Control: pace with a future owned by fdbserver instead of
                    // delay, the workaround used by workloads today.
                    if let Ok(trx) = db.create_trx() {
                        let _ = trx.get_read_version().await;
                    }
                }
                "select_ready" => {
                    // A select! loop whose "work" branch is immediately ready, so
                    // the fresh timeout loses every time and the loop never yields
                    // to the simulator: simulated time never advances.
                    use futures_util::FutureExt;
                    let work = std::future::ready(());
                    let timeout = self.context.delay(self.drop_delay);
                    futures_util::select_biased! {
                        _ = work.fuse() => {},
                        _ = timeout.fuse() => {},
                    }
                    if rss_kib() > self.rss_start + self.rss_limit_kib {
                        println!(
                            "delay_leak[{}] RSS limit hit at iter={i} sim_now={}",
                            self.client_id,
                            self.context.now()
                        );
                        self.report(i);
                        break;
                    }
                }
                "pump" => {
                    // The work branch progresses through a Rust-side wake only
                    // (YieldOnce, like a channel between tasks). fdb_rt polls woken
                    // tasks only from FDB callbacks, and the deferred cancel of the
                    // previous losing timeout supplies exactly that callback, so
                    // the loop drives itself while simulated time stays frozen.
                    use futures_util::FutureExt;
                    if i == 1 {
                        // Kick: one real race so a first timeout gets dropped.
                        let timeout = self.context.delay(self.drop_delay);
                        let kick = self.context.delay(self.delay);
                        futures_util::select_biased! {
                            _ = timeout.fuse() => {},
                            _ = kick.fuse() => {},
                        }
                    } else {
                        let timeout = self.context.delay(self.drop_delay);
                        futures_util::select_biased! {
                            _ = timeout.fuse() => {},
                            _ = YieldOnce(false).fuse() => {},
                        }
                    }
                    if rss_kib() > self.rss_start + self.rss_limit_kib {
                        println!(
                            "delay_leak[{}] RSS limit hit at iter={i} sim_now={}",
                            self.client_id,
                            self.context.now()
                        );
                        self.report(i);
                        break;
                    }
                }
                "per_poll" => {
                    // A hand-rolled timeout that creates the delay inside poll():
                    // each poll creates, polls and drops a delay, whose deferred
                    // cancel callback re-polls the task, forever.
                    let ctx = &self.context;
                    let limit = self.rss_start + self.rss_limit_kib;
                    let mut polls = 0u64;
                    let runaway = std::future::poll_fn(|cx| {
                        polls += 1;
                        if rss_kib() > limit {
                            return std::task::Poll::Ready(());
                        }
                        let mut d = Box::pin(ctx.delay(Duration::from_secs(60)));
                        let _ = d.as_mut().poll(cx);
                        std::task::Poll::Pending
                    });
                    // Bound the run in simulated time, for when the loop does not spin.
                    use futures_util::FutureExt;
                    futures_util::select_biased! {
                        _ = runaway.fuse() => {},
                        _ = ctx.delay(Duration::from_secs(120)).fuse() => {},
                    }
                    println!(
                        "delay_leak[{}] per_poll: {polls} polls, sim_now={}",
                        self.client_id,
                        self.context.now()
                    );
                    self.report(i);
                    break;
                }
                "drop_rounds" => {
                    // Burst of drops without yielding, then let the network
                    // settle once per report period. If memory is only queued
                    // (not leaked), RSS plateaus after the first round.
                    drop(self.context.delay(self.delay));
                    if i % self.report_every == 0 {
                        let _ = self.context.delay(Duration::from_secs(5)).await;
                    }
                }
                "poll_drop" => {
                    // A timeout that loses a `select!`: the delay is polled once
                    // (so a callback and waker are registered on the FDBFuture),
                    // then dropped while pending. Then yield so the
                    // cancellation is processed.
                    let mut pending = Box::pin(self.context.delay(self.drop_delay));
                    std::future::poll_fn(|cx| {
                        let _ = pending.as_mut().poll(cx);
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(pending);
                    self.context
                        .delay(self.delay)
                        .await
                        .expect("delay future should resolve");
                }
                "drop_yield" => {
                    // A timeout that loses the race: a longer delay is created
                    // and dropped while still pending, then we yield to the
                    // network with a short delay so the cancellation is processed
                    // before the next iteration.
                    drop(self.context.delay(self.drop_delay));
                    self.context
                        .delay(self.delay)
                        .await
                        .expect("delay future should resolve");
                }
                _ => {}
            }
            if i % self.report_every == 0 {
                self.report(i);
                println!(
                    "    wall={:?} sim_now={}",
                    wall.elapsed(),
                    self.context.now()
                );
                if let Some(settle) = self.settle_every_report {
                    let _ = self.context.delay(settle).await;
                    self.report(i);
                }
            }
        }
        // Let cancelled delays (drop mode) settle before the final reading.
        let _ = self.context.delay(Duration::from_secs(1)).await;
        self.rss_end = rss_kib();
        self.rust_end = LIVE.load(Ordering::Relaxed);
        let per_iter =
            (self.rss_end as f64 - self.rss_start as f64) * 1024.0 / self.iterations as f64;
        println!(
            "delay_leak[{}] DONE mode={} iterations={} rss_growth_kib={} (~{per_iter:.1} B/iter) rust_live_growth_bytes={}",
            self.client_id,
            self.mode,
            self.iterations,
            self.rss_end as i64 - self.rss_start as i64,
            self.rust_end - self.rust_start
        );
    }

    async fn check(&mut self, _db: SimDatabase) {}

    fn get_metrics(&self, mut out: Metrics<'_>) {
        out.extend([
            Metric::val(
                "rss_growth_kib",
                self.rss_end as f64 - self.rss_start as f64,
            ),
            Metric::val(
                "rust_live_growth_bytes",
                (self.rust_end - self.rust_start) as f64,
            ),
        ]);
    }

    fn get_check_timeout(&self) -> f64 {
        3000.
    }
}

struct DelayLeakFactory;
impl RustWorkloadFactory for DelayLeakFactory {
    fn create(name: String, context: WorkloadContext) -> WrappedWorkload {
        let client_id = context.client_id();
        let iterations = context.get_option("iterations").unwrap_or(200_000);
        let report_every = context.get_option("reportEvery").unwrap_or(20_000);
        let delay_ms: f64 = context.get_option("delayMs").unwrap_or(0.1);
        let mode = context
            .get_option::<String>("mode")
            .unwrap_or_else(|| "await".to_string());
        let drop_delay_ms: f64 = context.get_option("dropDelayMs").unwrap_or(delay_ms * 10.0);
        let rss_limit_mib: u64 = context.get_option("rssLimitMiB").unwrap_or(2048);
        let settle_ms: Option<f64> = context.get_option("settleMs");
        let client0_only = context.get_option("client0Only").unwrap_or(true);
        match name.as_str() {
            "DelayLeakWorkload" => DelayLeakWorkload {
                context,
                client_id,
                iterations,
                report_every,
                delay: Duration::from_secs_f64(delay_ms / 1000.0),
                mode,
                rss_limit_kib: rss_limit_mib * 1024,
                settle_every_report: settle_ms.map(|ms| Duration::from_secs_f64(ms / 1000.0)),
                drop_delay: Duration::from_secs_f64(drop_delay_ms / 1000.0),
                client0_only,
                rss_start: 0,
                rss_end: 0,
                rust_start: 0,
                rust_end: 0,
            }
            .wrap(),
            _ => panic!("Unknown workload name: {name}"),
        }
    }
}

register_factory!(DelayLeakFactory);
