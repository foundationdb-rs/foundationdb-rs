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

Tests the leader election recipe (Active Disk Paxos algorithm) with:
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

Core Invariants:
1. **DualPathValidation**: Replay logs in FDB commit order matches actual state
2. **LeaderIsCandidate**: Current leader must be a registered candidate
3. **CandidateTimestamps**: No candidate has future timestamps
4. **LeadershipSequence**: Per-client operation numbers are monotonic
5. **RegistrationCoverage**: All leaders were previously registered

Split-Brain Detection:
6. **NoOverlappingLeadership**: No two clients have overlapping leadership periods
7. **BallotValueBinding**: Each ballot number maps to exactly one leader

Timing and Ballot Bug Detection:
8. **FencingTokenMonotonicity**: Ballot numbers increase monotonically across claims
9. **GlobalBallotSuccession**: Each new leader has ballot > previous leader's ballot

Correctness:
10. **MutexLinearizability**: Leadership history linearizes to valid mutex model
11. **LeaseOverlapCheck**: No active lease overlaps between leaders
