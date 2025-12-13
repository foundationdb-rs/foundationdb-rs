# FoundationDB Recipes Simulation

Simulation workloads for testing FoundationDB recipes under chaos conditions.

## Building

```bash
cargo build -p foundationdb-recipes-simulation --release
```

## Running

```bash
fdbserver -r simulation -f foundationdb-recipes-simulation/test_leader_election.toml -b on --trace-format json
```

## Workloads

### LeaderElectionWorkload

Tests the leader election recipe with:
- Multiple clients registering as processes
- Periodic heartbeats and leadership attempts
- Operation logging for verification

**Configuration (TOML):**
- `operationCount`: Number of heartbeat+leadership cycles per client (default: 50)
- `heartbeatTimeoutSecs`: Election timeout in seconds (default: 10)

## Status

Work in progress. Known issues:
- Check phase `get_range()` fails with error 2000 - verification disabled for now
