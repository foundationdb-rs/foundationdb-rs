# FoundationDB Recipes Simulation

Deterministic-simulation workloads for the recipes shipped with `foundationdb`.
Today that means one workload, `LeaderElectionWorkload`, which drives the leader
election recipe inside FoundationDB's own simulator and then judges the run
against ten invariants.

## What it tests, and what it does not

The workload drives the recipe's **transaction-level primitives**
(`try_claim`, `refresh`, `resign`, `watch_term`) and emulates, on simulated
time, the state machine that the async handle layer (`LeaderElector`,
`LeaseHandle`) implements on real time: campaign, renew, hard-stop at the belief
horizon, resign.

That split is deliberate. The primitives take every instant they use as an
argument, so they are pure functions of `(record, observation state, supplied
time)` and a simulator can drive them exactly. The handle layer takes its time
through a `Clock` trait whose production implementation is backed by `Instant`
and `tokio::time::sleep`, which the simulator cannot control, so the handle is
covered by the live-cluster integration tests in
`foundationdb/tests/leader_election.rs` instead. Backing that `Clock` with
`context.now()` / `context.delay()` and running the real elector in here is a
natural follow-up, and nothing in the design blocks it.

The consequence worth remembering: the belief intervals the simulation checks
are the ones **this driver** computed and logged, not ones the shipped handle
produced.

## Roles

| Role | Who | What it does |
|------|-----|--------------|
| Contender | everyone not below | Campaigns, renews, does fenced work under its ballot, resigns, occasionally "crashes" (stops for longer than its lease without saying so) |
| Sleeper | client 1, when `sleeperEnabled` and there are at least 3 clients | The Kleppmann pause: takes a term, writes under it, stops for `pauseFactor` leases, then tries to use the stale term. Both the write and the renewal must be refused |
| Watcher | client 2, when there are at least 4 clients | Discovers leadership through the term key with a watch rather than by polling |

Every role logs sightings of the leader record, not only the Watcher: attrition
kills clients, and the liveness check needs *somebody* to have been watching.
That is the failover story, and it needs no coordination.

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
misspelled knob is a failed run rather than a silently ignored setting. That is
also why all five configurations carry the same knobs even where a value does
nothing.

| Knob | Meaning |
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

| Configuration | Purpose |
|---------------|---------|
| `test_baseline.toml` | The ordinary path, all three roles, moderate clogging and attrition |
| `test_strict_mutex.toml` | Identical clocks, no faults: the check runs with **zero** tolerance |
| `test_short_lease_stress.toml` | Three second leases, so every margin is the only thing left |
| `test_churn_attrition.toml` | Harshest faults, extreme skew, highest progress thresholds |
| `test_pause_fencing.toml` | The Kleppmann pause, barriered, with no attrition to interrupt it |

In the three configurations that inject faults, the chaos workloads stop at
about two thirds of the run and the workload keeps going to
`testDurationSecs`. `ProgressMade` asks whether the run recovered and made
progress, and a run whose faults never stop has no window to recover *into*:
whether it clears the floor then depends on how the kills happened to land
rather than on the protocol.

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
| 8 | `UuidRecoveryNoDup` | A recovered unknown commit claiming a second time instead of adopting its own record |
| 9 | `ProgressMade` | A run in which nothing happened, which every safety check passes vacuously |
| 10 | `HistoryFaithful` | A history entry escaping the transaction of the transition it describes |

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
    -f foundationdb-recipes-simulation/test_baseline.toml \
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
| `invariants.rs` | The ten checks, their tolerances, and a counterexample test for each |
| `clock.rs` | Per-client skewed clocks |
| `logged_op.rs` | The wrapper that commits a primitive and its log record together |
| `roles.rs` | The role loops, and the belief bookkeeping |
| `workload.rs` | Option parsing, the three phases, and the check phase's reporting |

The first three are pure: they know nothing about FoundationDB beyond the tuple
layer, take their inputs as values, and are tested without a simulator anywhere
in sight.
