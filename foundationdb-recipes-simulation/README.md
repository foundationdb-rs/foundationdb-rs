# FoundationDB Recipes Simulation

Simulation workloads for testing FoundationDB recipes under chaos conditions.

## Building

```bash
cargo build -p foundationdb-recipes-simulation --release
```

## Running

### Searching for bugs

Use the provided scripts to run simulations:

```bash
# Run all test configurations (1 iteration each by default)
./scripts/run_leader_election_simulation.sh

# Run all test configurations with 10 iterations each
./scripts/run_leader_election_simulation.sh 10
```

Traces are stored in `./target/traces/`.

### Debugging

```bash
# Don't forget to rm old traces to facilitate the search inside the traces
fdbserver -r simulation -f foundationdb-recipes-simulation/test_leader_election.toml -b on --trace-format json -L ./target/traces --logsize 1GiB --seed <SEED>
```

## Workloads

### LeaderElectionWorkload

Tests the leader election recipe with:
- Multiple clients registering as candidates
- Periodic heartbeats and leadership attempts
- Ballot-based leadership claims with lease expiry
- Operation logging for verification

**Configuration (TOML):**
- `operationCount`: Number of heartbeat+leadership cycles per client (default: 50)
- `heartbeatTimeoutSecs`: Lease duration in seconds (default: 10)
- `resignProbability`: Probability of resigning after becoming leader (default: 0.1)
- `allowPreemption`: Integer `0`/`1` (default: `1`). Set `0` to disable priority
  preemption, which is required for the strict `NoBeliefOverlap` assertion.
- `maxClockSkewSecs`: Override the randomized per-client clock skew with a fixed
  one-sided bound in seconds. `0.0` disables skew entirely (local time equals
  simulation time). When unset, each client gets a random skew tier.
- `minLeadershipClaims` / `minHeartbeats`: Minimum successful operations a run
  must produce for `ProgressMade` to pass (default: `1`, floored at `1`).

**Test Configurations:**

| Test File | Purpose |
|-----------|---------|
| `test_leader_election.toml` | Basic functionality test with moderate settings |
| `test_ballot_stress.toml` | High contention test with rapid ballot transitions |
| `test_rapid_leadership.toml` | Fast leadership turnover with high resign probability |
| `test_short_lease.toml` | Short lease duration for timing edge cases |
| `test_strict_mutex.toml` | Preemption off + zero skew: strict belief-overlap assertion under chaos |

Note: a check-phase violation fails the simulation by emitting a
`Severity::Error` trace (FDB tallies `SevError` events as failures); the check
phase's return value alone does not signal failure. Every client runs the check
against the shared operation log, so verification survives Attrition killing any
single client.

**Invariants Verified:**

**1. DualPathValidation** - The keystone invariant. Replays all logged operations in true FDB commit order (versionstamp-ordered keys) to compute the expected leader identity and running ballot, then compares with the actual FDB snapshot. Subsumes both safety (at most one leader record) and ballot conservation. A resignation leaves a *vacant* record (no holder, ballot preserved), which the replay models as leaderless-but-ballot-preserved.

**2. LeaderIsCandidate** - The current active leader must exist in the candidate list. A vacant record (resigned) is skipped. If the leader's lease has expired, candidate eviction is valid.

**3. CandidateTimestamps** - No candidate heartbeat timestamp is implausibly in the future. The tolerance tracks the configured clock-skew bound (plus a small margin) and collapses toward zero in the strict configuration.

**4. LeadershipSequence** - Each client's operation numbers (`op_num`) are strictly increasing.

**5. RegistrationCoverage** - Every client that claimed leadership has a prior registration entry.

**6. BallotSuccession** - Every successful leadership claim satisfies `ballot == previous_ballot + 1`, unconditionally. Because ballots are globally monotonic and never reset (resign preserves the ballot in a vacant record), this holds for first claims (`0 -> 1`), steals, preemptions, refreshes, and post-resign claims alike. Replaces the older per-tenure ballot invariants, which existed only to work around the ballot reset that the recipe no longer does.

**7. OneValuePerBallot** - Each ballot number maps to exactly one client, globally. Since ballots strictly increase across the whole run, a ballot claimed by two clients indicates broken conflict ranges or ballot-assignment logic.

**8. LeaseExpiryAfterClaim** - Every successful claim has a lease that expires after its claim timestamp (catches lease-duration or clock bugs).

**9. NoBeliefOverlap** - Real mutual exclusion. A belief overlap can only be created by a *steal* (one client taking the leader record from a different client that neither resigned nor lost it), so the check walks the log in commit order and asserts that every steal lands after the victim's lease horizon (`victim_last_refresh + lease_duration`), within `2 * maxClockSkewSecs` plus a small floor. Resign -> vacant -> claim handoffs create no overlap and are not flagged. Only meaningful with preemption disabled (preemption deliberately steals a live lease; the fencing ballot, not the lease, provides safety there), so it reports an informational skip when `allowPreemption` is on. In `test_strict_mutex.toml` (zero skew, read-anchored timestamps) steals reliably land after the lease expires.

**10. ProgressMade** - Guards against a vacuous pass: the log must be non-empty and the run must produce at least `minLeadershipClaims` successful claims and `minHeartbeats` successful heartbeats.

### Architecture Notes

**Versionstamp-Ordered Logging**: All log entries use FDB versionstamp-ordered keys, providing true causal ordering based on actual FDB commit order rather than wall-clock time. Each entry also records the unskewed simulation time (`context.now()`) at commit, used to reconstruct belief intervals without clock-skew contamination.

**Dual-Path Validation Pattern**: The simulation replays the operation log to compute expected state and compares it against the actual FDB state, catching bugs that only manifest under specific timing or failure conditions.

**Read-Anchored Timestamps**: The recipe measures its lease-observation window against a caller-supplied `current_time`. The workload samples that clock *inside* the claim transaction, after the leader read, rather than at loop-top. This matters because a stale, pre-transaction sample would let heartbeat/retry/clogging latency inflate the observed duration and cause spurious belief overlaps. This is also a real deployment note for the recipe: sample `current_time` close to the transaction, or belief-level exclusion can be under-counted (record-level exclusion and fencing are unaffected).

**Clock Skew Simulation**: Each client gets a randomized skew tier (offset + drift + jitter, up to ~2.2s worst-case one-sided deviation) unless `maxClockSkewSecs` overrides it. Skew perturbs measured durations, so time-based invariants carry a matching tolerance; the strict configuration disables skew so `NoBeliefOverlap` asserts that every steal lands after the victim's lease has expired.
