# FoundationDB Recipes Simulation

Deterministic-simulation workloads for the recipes shipped with `foundationdb`.
Today that means one workload, `LeaderElectionWorkload`, which drives the leader
election recipe inside FoundationDB's own simulator and then judges the run
against twelve invariants.

## What it tests, and what it does not

The workload drives the recipe's **transaction-level primitives**
(`leader`, `try_claim`, `refresh`, `resign`) and emulates, on simulated
time, the state machine that the async handle layer (`LeaderElector`,
`LeaseHandle`) implements on real time: campaign, renew, hard-stop at the belief
horizon, resign.

That split is deliberate. The primitives take every instant they use as an
argument, so they are pure functions of `(record, observation state, supplied
time)` and a simulator can drive them exactly.

**The handle layer is simulated too**, by the Elector role. A drawn run converts
its two highest contenders into clients that hand the whole loop to the recipe's
own `LeaderElector`, built on this client's skewed `Clock`, the simulator's
generator and a `SimTimer` over `context.delay()`. Nothing about the handle layer
resists simulation: `Clock` and `Timer` are traits precisely so that the
production `Instant`/`tokio::time::sleep` pair can be swapped for the
simulator's timeline.

The two halves are judged differently, and that is the part worth remembering.
The driver's belief intervals are the ones **this driver** computed and logged,
so what they check is the protocol. The elector's are the recipe's own, read off
`LeaseHandle::believed_until`, and because the recipe owns those transactions
there is no log to wrap: `elector_invariants.rs` pairs the recipe's history
subspace with what the role recorded, in commit order, and asks whether a write
ever landed outside the term that authorized it. Safety by effect rather than by
code path.

The live-cluster integration tests in `foundationdb/tests/leader_election.rs`
remain the coverage for what no simulator sees: a real cluster's commit
behaviour under real concurrency.

## Roles

| Role | Who | What it does |
|------|-----|--------------|
| Contender | everyone not below | Campaigns, renews, does fenced work under its ballot, resigns, occasionally "crashes" (stops for longer than its lease without saying so) |
| Sleeper | client 1, when `sleeperEnabled` and there are at least 3 clients | The Kleppmann pause: takes a term, writes under it, stops for `pauseFactor` leases, then tries to use the stale term. Both the write and the renewal must be refused |
| Watcher | client 2, when there are at least 4 clients | Discovers leadership the only way this recipe offers: polls the leader record, logs what it saw, parks, repeats. It never campaigns |

Every role logs sightings of the leader record, not only the Watcher: attrition
kills clients, and the liveness check needs *somebody* to have been looking.
That is the failover story, and it needs no coordination.

There are **no watches anywhere**, in the recipe or in here. Discovery is
polling, which is the DAIS 2015 protocol the recipe descends from and which has
no notification primitive at all. A watch resolves with an error under exactly
the conditions an election exists for, and a loop that reads "the watch
resolved" as "the key changed" spins at wall-clock speed committing a
transaction per turn. One such loop took an `fdbserver` process from megabytes
to gigabytes in seconds. See "Discovery is polling, and there are no watches" in
the recipe's module documentation.

The Sleeper's scenario is **barriered**: the stale write and the stale renewal
are only attempted once a successor has both taken the term *and* committed a
write under a higher rank. Without that, a refused write proves nothing (it
could have been refused for want of any fence at all). The other clients hold
back for half a lease at the start of a run where a Sleeper exists, so that the
scenario actually happens rather than depending on who wins the opening race.

## Renewals are scheduled, not polled for

A renewal is due at a deadline, and the handle layer treats it as one: its
renewal driver is a future racing the work, and it sleeps exactly until the
deadline. This workload emulates that in two places, and both were bugs before
`ProgressMade` learned to report belief intervals that outlived their renewal
deadline without renewing:

- a step that finds the renewal already due renews *before* it rolls for a
  crash or a resign. Rolling first meant a term that already owed a renewal
  could be ended instead, which suppressed renewals systematically rather than
  randomly;
- a leader waits until its renewal is due rather than a whole step past it.
  With a three second lease a step is a large fraction of a term, and
  overshooting the deadline is how a leader reaches its horizon having never
  renewed at all.

## Every loop is bounded

Every role is a loop that ends when simulated time reaches the deadline, which
is only an exit condition if each iteration actually costs simulated time. The
waits are the fragile part: `context.delay()` yields an `FdbResult`, and a
client the simulator is tearing down gets an error rather than a wait. Code that
discards that result reads a failure as "the wait finished", stops being paced
by the clock, and starts being paced by how fast it can issue transactions. Each
turn of such a loop commits a transaction and appends a log entry, and the check
phase then has to read all of them back, so what began as a spin ends as an
out-of-memory death with nothing reported.

Four bounds, in order of how structural they are:

- **Absolute-deadline pacing.** Loops do not wait "a step"; they own a deadline
  cursor, advance it *before* awaiting, and wait until it. A wait that returns
  instantly therefore leaves the next round asking for a real one, so the loop
  cannot spin even if every wait is broken. This is upstream FoundationDB's own
  `delayUntil` idiom, and it is drift-free as a bonus: a round whose work took
  time is followed by a shorter wait rather than a full step on top.
- **A failed wait is not a wait.** Every delay in the crate reports its failure.
  In the roles that means the role traces it and ends; in `SimTimer`, whose
  `Timer::sleep` signature has nowhere to put an error, it means the sleep never
  resolves, and the elector role's own give-up race is what notices and says so.
  Both match the simulator: the errors we see (`operation_cancelled`,
  `broken_promise`) are the bridge saying the flow-side actor is gone, which in
  C++ destroys the coroutine frame so nothing after the wait runs, and a delay
  owned by a killed process is forwarded to `Never()` in `sim2.cpp` and never
  fires. Retrying a failing delay is not an option: that is the same hot loop
  wearing a different hat.
- **A liveness guard on every loop** (`liveness.rs`). Three iterations in a row
  that begin at the same simulated instant end the role with a `WarnAlways`
  naming the loop. It is the backstop for whatever the first bound misses.
- **A per-client operation ceiling** in the journal, derived from the run's
  duration and step with an order of magnitude of headroom. Past it every
  operation is refused, and the role ends like any other failure. This is what
  converts a future hot loop into a loud, fast failure rather than an expensive
  one.
- **A bounded check phase.** The logs are read a page at a time across
  transactions, up to a hard cap of what the ceilings can explain; a read that
  hits the cap traces `Severity::Error` and fails the run. Retries are limited
  rather than unbounded, so a read that cannot succeed says so inside the check
  budget instead of consuming all of it and reporting nothing. The invariants
  that pair every belief with every other keep at most 64 violations plus a line
  counting the rest, which bounds memory without softening any judgement: a
  report with any violation in it fails the run.

None of this weakens an invariant. Caps bound how much a failing run writes
down, never whether it failed.

## Fencing, and when it is installed

Winning a ballot fences nothing by itself: the ranked register refuses an old
rank only once a higher one has been read into it. This workload therefore
installs the fence **in the claim transaction**, not in one of its own
afterwards. Two windows close as a result, and both were observed in early
runs of this suite:

- between a claim committing and its fence being installed, the deposed
  leader's writes are still accepted;
- a term that is won and then abandoned (a claim that took so long to commit
  that it came back past its own lease, for instance) leaves that window open
  for good, because its winner never installs anything.

Fenced work is also raced against the belief horizon and dropped when the
horizon wins, which is what the handle layer does to the work future. Dropping
a transaction cannot un-issue a commit, so it is the fence, not the horizon,
that makes a late write safe; the horizon race only keeps a leader from
knowingly retrying work it has stopped believing in.

## Forced recovery

The recipe's unknown-commit recovery is reached when a claim commits and its
caller never learns that it did. Under simulation that essentially never
happens by itself: every logged transaction sets `AutomaticIdempotency`, so
the client resolves the unknown commit before the recipe is asked anything,
and `UuidRecoveryNoDup` holds vacuously.

Rather than weaken the log's idempotency (a versionstamped append really is
not idempotent, and a retried one would double-count), the driver injects the
condition one layer up. A contender that drew the `forcedRecovery` feature
throws away a claim reply it did receive, records nothing, believes nothing,
and re-runs the **same** `ClaimAttempt` later. Everything the recipe sees is
what it would have seen had the reply really been lost, and both resolutions
get exercised:

- a short delay resumes inside the lease, so the re-probe finds its own record
  and adopts it without consuming a second ballot;
- a delay past a lease resumes after a contender may have stolen the term, so
  the re-probe finds a stranger at or past its own ballot, retires the token,
  and campaigns again with a fresh one.

An `injected_unknown` marker is written in a transaction of its own after the
claim committed, so it exists exactly when the claim it describes does, and
`RecoveryExercised` uses it to tell a run that took the path from one that
merely could have. A reply is only dropped while the run still has room to
re-probe it, so an injection nobody resolved is a broken resumption rather
than a run that ran out of time.

## Clocks

Each client reads time through its own skewed clock, and that reading is the
only notion of time the recipe ever receives; true simulated time is reserved
for the check phase, which uses it as an oracle.

- **Offset** is injected but cancels out of every measurement the protocol
  makes, since all of them are elapsed times on one clock. It is there so that a
  recipe that accidentally compared two clients' timestamps would fail loudly.
- **Rate error** does not cancel. It is the assumption the belief-exclusion
  argument rests on, and the check phase derives its tolerances from the same
  bound the clocks are built with (`none` = 0, `random` = 1%, `extreme` = 5%).
- **Jumps and regressions** (`extreme` only) are injected separately from rate
  error and kept inside the same budget. A step larger than that is a fault the
  design makes no claim about, and injecting one would produce a "failure" that
  says nothing except that the simulation broke its own assumptions.

## Configuration

Every knob is read exactly once, in the workload's constructor. `getOption`
consumes, and fdbserver fails a run that leaves options unconsumed, so a
misspelled knob is a failed run rather than a silently ignored setting. A run
belongs to exactly one of two knob families: `swarmEnabled` and
`testDurationSecs` are read first and always, and when `swarmEnabled` is set
nothing else is read at all, because everything the run does is drawn from
the seed instead. Otherwise the anchor knobs below are read, and an anchor
file spells out all eleven even where a value does nothing.

| Anchor knob | Meaning |
|------|---------|
| `leaseDurationSecs` | The lease every claim advertises |
| `stepIntervalSecs` | How long a client waits between actions (jittered) |
| `testDurationSecs` | How long the start phase runs, in simulated time |
| `resignProbability` | Chance per step that a leader hands its term back |
| `crashProbability` | Chance per step that a leader stops responding for longer than its lease |
| `clockSkewMode` | `none`, `random` or `extreme` |
| `pauseFactor` | How many leases the Sleeper pauses for |
| `sleeperEnabled` | Whether the Sleeper role is assigned at all |
| `minLeadershipClaims` | Applied claims and steals the run must have achieved |
| `minRenewals` | Applied renewals the run must have achieved |
| `minObservedIdentities` | Distinct leader identities the sightings must cover |

| Swarm knob | Meaning |
|------|---------|
| `swarmEnabled` | Hands the whole run to `SwarmPlan::draw`; every other option above is drawn from the seed instead of read from the file |
| `testDurationSecs` | How long the start phase runs, in simulated time (the one anchor knob a swarm file still carries) |

| Configuration | Purpose |
|---------------|---------|
| `test_swarm.toml` | Per-seed drawn configuration: a random feature subset, storm schedules, boundary-value palettes |
| `test_strict_mutex.toml` | Identical clocks, no faults: the check runs with **zero** tolerance |
| `test_pause_fencing.toml` | The Kleppmann pause, barriered, with no attrition to interrupt it |

In the configurations that inject faults, the chaos workloads stop at about
two thirds of the run and the workload keeps going to `testDurationSecs`.
`ProgressMade` asks whether the run recovered and made progress, and a run
whose faults never stop has no window to recover *into*: whether it clears
the floor then depends on how the kills happened to land rather than on the
protocol.

## Swarm testing

`test_swarm.toml` does not describe a scenario, it describes a seed. When
`swarmEnabled` is set, `SwarmPlan::draw` turns the run's shared random number
into a full configuration: which features are hard on or hard off, the clock
skew mode, the lease and step and pause palettes, and the crash and resign
storm schedules. Everything the anchor configurations spell out by hand, a
swarm run decides for itself, and it decides differently on every seed.

**Why hard on or off, not a probability.** A bug can hide from a fixed-rate
knob in two ways. Active suppression is a feature that, while on, masks a bug
class in another feature; lowering its probability only makes the mask rarer,
it never removes it. Passive suppression is subtler: with several features
each firing at their own low rate, the mix of operations is a random walk that
almost never runs the streak of same-kind operations a bug needs to surface
(a Hoeffding-style concentration result). Neither is defeated by turning a
dial down. Both need the feature *absent* for some runs, which is what a hard
off gives you and a probability never does.

**Why the subset size is fat-tailed.** Six independent coins make the number
of enabled features binomial, concentrated around half of them: all-on,
all-off, and the single-feature isolate that best exposes one feature's code
path all become rare. `SwarmPlan::draw` picks the shape of the subset from a
fat-tailed selector first, so those extremes get oversampled instead of
starved.

**Why faults come in storms, not per-step coins.** A coin rolled every step
spreads faults uniformly, which is the one arrival pattern real outages never
have, and it makes two things nearly unreachable: a burst dense enough to
crash a replacement leader mid-claim, and a quiet tail long enough to prove
the system actually recovers. A drawn plan instead places a handful of storms,
each with its own intensity, which is bursty in the way a Hurst exponent above
0.5 describes.

**Why knob values are boundary-heavy.** The palettes a plan draws from include
their own edges: zero, the tightest lease, the largest pause factor. A
boundary value has low Kolmogorov complexity and is disproportionately where
off-by-ones live, so it is worth oversampling the same way the feature subset
is.

**The progress floor is never vacuous.** A drawn plan derives its own progress
thresholds from what it drew, and `min_acquisitions >= 1` always holds no
matter how quiet the plan is: even the emptiest drawable configuration is
still required to prove something happened.

**Reproducing a run.** The plan is a pure function of the seed, so a failing
seed reproduces the same plan. The script prints the seed before every
iteration and the exact replay command on failure; the setup phase also
traces the drawn configuration as `LeaderElectionSwarmPlan` on every client,
which is the first thing to read when a swarm run fails.

References:

- Groce, Zhang, Eide, Chen, Regehr, "Swarm Testing" (ISSTA 2012)
- Will Wilson's swarm-testing talk (Antithesis)
- Pierre Zemb, "Writing Rust FDB workloads that find bugs",
  https://pierrezemb.fr/posts/writing-rust-fdb-workloads-that-find-bugs/

## Invariants

Each one is a pure function of the replayed log plus the evidence it needs, and
each one has unit tests that feed it a hand-mutated log it must reject. The
suite this replaces had seven invariants that could not fail for any input, so
a check earns its place here only with a counterexample.

| # | Invariant | Falsified by |
|---|-----------|--------------|
| 1 | `DualPathReplay` | A write nobody logged, or a logged write that never landed |
| 2 | `BallotSuccession` | A ballot that resets, skips, or moves on a renewal; a write decided on a stale read |
| 3 | `OneClaimPerBallot` | Two processes acquiring the same ballot, which is the state fencing assumes cannot exist |
| 4 | `NoBeliefOverlap` | A successor believing it leads while its predecessor still does |
| 5 | `StealObservationDiscipline` | A steal taken before a full lease of unbroken observation |
| 6 | `VacantReclaim` | A resign that loses the ballot, or a claim over a live record |
| 7 | `FencingHolds` | The paused leader's stale write landing after its term ended |
| 8 | `SleeperWasFenced` | The pause scenario passing vacuously: the Sleeper reached its barrier and then never attempted the stale write and renewal that `FencingHolds` judges |
| 9 | `UuidRecoveryNoDup` | A recovered unknown commit claiming a second time instead of adopting its own record, or a retired token campaigning again |
| 10 | `ProgressMade` | A run in which nothing happened, which every safety check passes vacuously |
| 11 | `HistoryFaithful` | A history entry escaping the transaction of the transition it describes |
| 12 | `RecoveryExercised` | An injected unknown commit nobody resolved, or an injector that stopped firing and left `UuidRecoveryNoDup` vacuous |

A violation is traced at `Severity::Error`, which is the only thing that fails a
FoundationDB simulation run, and the log is dumped around the first one. The
check phase runs on **every** client: attrition kills clients, and a run whose
only judge was killed used to pass by default.

## Running

`fdbserver` **7.4.6 or newer** is required: the C-API workload path in 7.4.3,
7.4.4 and 7.4.5 has an incompatible ABI. `nix develop` provides a suitable one.

```bash
# Every configuration once
nix develop -c ./scripts/run_leader_election_simulation.sh

# Ten iterations of each
nix develop -c ./scripts/run_leader_election_simulation.sh 10

# Ten iterations of one
nix develop -c ./scripts/run_leader_election_simulation.sh 10 pause_fencing
```

The script prints the seed of every iteration and, on failure, the exact command
to replay it. Traces land in `./target/traces/`.

To run one configuration by hand:

```bash
cargo build -p foundationdb-recipes-simulation --release
nix develop -c fdbserver -r simulation \
    -f foundationdb-recipes-simulation/test_swarm.toml \
    -b on --trace-format json -L ./target/traces --logsize 1GiB -s <SEED>
```

The pure half of the crate needs no simulator at all:

```bash
cargo test -p foundationdb-recipes-simulation --lib
```

## Layout

| File | What lives there |
|------|------------------|
| `log_schema.rs` | The versionstamped operation log: records, keys, and the hand-built fixtures the invariant tests mutate |
| `replay.rs` | Commit-ordered replay. Judges nothing, which is what keeps the invariants falsifiable |
| `invariants.rs` | The twelve checks on the driver's half, their tolerances, and a counterexample test for each |
| `elector_invariants.rs` | The checks on the elector's half, which pair the recipe's own history with the role's log in commit order |
| `swarm.rs` | The plan a seed draws: features, lease, step, fault windows, and the thresholds that follow from them |
| `liveness.rs` | The guard that stops a loop going round without the clock moving |
| `clock.rs` | Per-client skewed clocks |
| `timer.rs` | The `Timer` the recipe's elector waits on, over the simulated timeline |
| `logged_op.rs` | The wrapper that commits a primitive and its log record together, and the per-client operation ceiling |
| `roles.rs` | The role loops, and the belief bookkeeping |
| `elector_role.rs` | The role that runs the recipe's own `LeaderElector` rather than emulating it |
| `workload.rs` | Option parsing, the three phases, and the check phase's reporting |

The first five are pure: they know nothing about FoundationDB beyond the tuple
layer, take their inputs as values, and are tested without a simulator anywhere
in sight.
