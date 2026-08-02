# FoundationDB recipes simulation

This crate runs deterministic FoundationDB simulator workloads for recipes.
Its leader-election workload exercises the poll-based `LeaderElection` recipe
composed with `RankedRegister` fencing.

## Running

```bash
./scripts/run_leader_election_simulation.sh
./scripts/run_leader_election_simulation.sh 10
```

The script builds the release workload and runs every polling configuration.
Traces are written to `target/traces`. To reproduce a seed directly:

```bash
fdbserver -r simulation \
  -f foundationdb-recipes-simulation/test_poll_leader_election.toml \
  -b on --trace-format json -L target/traces --logsize 1GiB --seed <SEED>
```

## Leader-election workload

The durable election state is one serializable key containing a generation and
an optional owner. Generations never reset. The workload supplies an immutable
caller-local observation and simulated monotonic time to each `poll`. A changed
observation is recorded locally, while an unchanged owner may be suspected only
after `suspicionSecs`. Time is neither persisted nor used to order commits.

Every `db.run` attempt sets `AutomaticIdempotency` at the caller boundary.
Returned observations and local ranks are adopted only after the outer run
succeeds. A failed run can have committed, so its co-committed versionstamp log
entry remains an oracle witness rather than a reason to update local state.

Each client performs bounded polling rounds. A deterministic `context.rnd()`
bitset enables a subset of optional conditional resigns, deliberate stale
resigns and writes, observers, and pauses. The core path always polls. A leader
must call `RankedRegister::read(rank)` and successful `write(rank, payload)` in
the same transaction as its poll. Leadership alone is not authority.

After an incarnation has committed a newer leader poll, the core path attempts
a bounded protected write with that incarnation's earlier rank. This is a
strictly older fencing token, not merely a duplicate-rank rejection, and needs
no cross-client scheduling coordination.

`RandomClogging` and `Attrition` provide fault injection. Transaction errors
during start are diagnostics. A committed leader write rejection or stale write
commit is reported as a protocol failure.

## Commit-order oracle

Every simulated poll, resign, observer, stale write, and protected write is
co-logged in the same transaction. Log keys begin with an incomplete
versionstamp, so the final range is ordered by actual commit order. Values carry
the prior and resulting election state, requested rank, classification, write
outcome, and protected payload identity.

During `check`, client 0 reads the entire operation log, public election state,
and ranked-register write rank and value in one transaction and one read
version. It replays from generation zero, no owner, and no protected value. The
oracle rejects:

- duplicate logical `(actor incarnation, op)` entries;
- leader polls that do not create exactly the next generation for their owner,
  or whose incumbent, unowned, or takeover classification disagrees with replay;
- follower or observer records that change or misreport replay state;
- resign results that do not exactly match the owner and rank while preserving
  generation;
- stale ranked-register writes that commit;
- leader polls without a same-transaction fenced protected write; and
- runs that never exercise a strictly stale fenced write; and
- any mismatch between replayed final state and the public state, protected
  write rank, or protected value read at the shared read version.
