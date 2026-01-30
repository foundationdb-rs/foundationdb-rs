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
# Run single test configuration (1 iteration by default)
./scripts/run_leader_election_simulation.sh

# Run single test configuration with 10 iterations
./scripts/run_leader_election_simulation.sh 10

# Run ALL test configurations (recommended for thorough testing)
./scripts/run_all_leader_election_tests.sh

# Run all tests with 5 iterations each
./scripts/run_all_leader_election_tests.sh 5
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
- `resignProbability`: Probability of resigning after becoming leader (default: 0.2)

**Test Configurations:**

| Test File | Purpose |
|-----------|---------|
| `test_leader_election.toml` | Basic functionality test with moderate settings |
| `test_ballot_stress.toml` | High contention test with rapid ballot transitions |
| `test_rapid_leadership.toml` | Fast leadership turnover with high resign probability |
| `test_short_lease.toml` | Short lease duration for timing edge cases |

**Invariants Verified (11 total):**

### Core Invariants (Foundational Safety)

**1. DualPathValidation** - The keystone invariant using the "atomic ops pattern". Replays all logged operations in true FDB commit order (versionstamp-ordered keys) to compute expected leader state, then compares with the actual FDB snapshot. This single invariant subsumes both safety (at most one leader at a time) and ballot conservation (ballot matches claims). If the replayed state diverges from the database state, it indicates a bug in the leader election logic or logging.

**2. LeaderIsCandidate** - Validates structural integrity: the current leader must exist in the candidates list. This ensures leadership can only be claimed by registered candidates. Exception: if the leader's lease has expired, candidate eviction is valid (the candidate may have timed out).

**3. CandidateTimestamps** - Clock skew detection: ensures no candidate has a heartbeat timestamp more than 3 seconds in the future. This tolerance accounts for extreme clock skew simulation (up to ±1s offset, 1s drift ahead, 1.1x jitter multiplier). Future timestamps beyond this threshold indicate clock handling bugs.

**4. LeadershipSequence** - Validates log ordering: each client's operation numbers (`op_num`) must be strictly monotonically increasing. With versionstamp-ordered logging, this verifies the log entries are properly sequenced per client.

**5. RegistrationCoverage** - Protocol compliance: every client that successfully claimed leadership must have a prior registration entry in the log. This ensures the registration requirement of the leader election protocol is enforced.

### Split-Brain Detection

**6. NoOverlappingLeadership** - Ensures leadership claims have distinct versionstamps. Since FDB commits are serialized and each successful leadership claim commits atomically, versionstamp ordering guarantees sequential transitions. Duplicate versionstamps would indicate a logging bug. This invariant catches split-brain scenarios where two clients might incorrectly believe they are both leaders.

**7. BallotValueBinding** - Validates ballot progression within each claim: the new ballot must be greater than the previous ballot read in the same transaction. This ensures the fencing token mechanism is working correctly and prevents duplicate elections at the same ballot number.

### Timing and Ballot Bug Detection

**8. FencingTokenMonotonicity** - Ensures ballots never regress: each successful leadership claim must have `ballot > previous_ballot` (or `previous_ballot == 0` for first claims after system reset). This catches stale leader writes where an old leader might attempt to use an outdated fencing token.

**9. GlobalBallotSuccession** - State machine safety: each new leader's ballot must be strictly greater than the predecessor's ballot. This invariant catches state regression bugs where the system might accept a lower ballot number.

### Correctness

**10. MutexLinearizability** - Leadership transfers must happen sequentially with at most one holder at a time in log order. In lease-based systems, a new leader claiming leadership implicitly ends the previous leader's tenure (lease expired or preempted). This tracks both explicit resigns and implicit releases to verify the leadership history linearizes to a valid mutex model.

**11. LeaseOverlapCheck** - Validates each successful leadership claim has a positive, non-zero `lease_expiry_nanos`. This ensures every leader has a bounded lease duration, which is essential for the lease-based leadership protocol to function correctly.

### Architecture Notes

**Versionstamp-Ordered Logging**: All log entries use FDB versionstamp-ordered keys, providing true causal ordering based on actual FDB commit order rather than wall-clock time. This eliminates clock skew issues in log replay.

**Dual-Path Validation Pattern**: The simulation uses a "dual-path" approach where one path replays the operation log to compute expected state, while the other path reads the actual FDB state. Comparing these two paths catches bugs that might only manifest under specific timing or failure conditions.

**Clock Skew Simulation**: The simulation injects extreme clock skew (up to 2.2s total deviation) to stress-test time-dependent logic like lease expiry handling.
