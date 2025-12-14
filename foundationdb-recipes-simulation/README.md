# FoundationDB Recipes Simulation

Simulation workloads for testing FoundationDB recipes under chaos conditions.

## Building

```bash
cargo build -p foundationdb-recipes-simulation --release
```

## Running

### Single run

```bash
fdbserver -r simulation -f foundationdb-recipes-simulation/test_leader_election.toml -b on --trace-format json
```

### Multiple iterations

Use the provided script to run multiple iterations:

```bash
# Run 1 iteration (default)
./scripts/run_leader_election_simulation.sh

# Run 10 iterations
./scripts/run_leader_election_simulation.sh 10

# Run infinitely until failure
./scripts/run_leader_election_simulation.sh 0
```

Traces are stored in `./target/traces/`.

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

**Invariants verified:**
1. **Safety (No Overlapping Leadership)**: At most one leader at any time
2. **Ballot Conservation**: Expected ballot count from logs matches actual ballot in FDB
