//! Invariant verification methods for leader election simulation.
//!
//! The check phase replays the versionstamp-ordered operation log and compares
//! it against the committed FDB state. Log entries are in true FDB commit order
//! (versionstamp-ordered keys), so replay reflects real causal ordering.
//!
//! Invariants (all report failures via `Severity::Error` traces, which is what
//! actually fails an FDB simulation):
//!
//! 1. **DualPathValidation** - Replayed log must match committed leader state
//!    (subsumes safety + ballot conservation).
//! 2. **LeaderIsCandidate** - An active leader must be a registered candidate.
//! 3. **CandidateTimestamps** - No candidate heartbeat is implausibly in the future.
//! 4. **LeadershipSequence** - Per-client op_nums are strictly increasing.
//! 5. **RegistrationCoverage** - Every leader registered first.
//! 6. **BallotSuccession** - Every successful claim has `ballot == previous_ballot + 1`.
//! 7. **OneValuePerBallot** - Each ballot maps to exactly one client (globally).
//! 8. **LeaseExpiryAfterClaim** - Each claim's lease expires after its claim time.
//! 9. **NoBeliefOverlap** - With preemption disabled, no two clients' leadership
//!    belief intervals overlap (in unskewed sim time) beyond the skew tolerance.
//! 10. **ProgressMade** - The run actually elected leaders and heartbeated.

use std::collections::BTreeMap;

use super::LeaderElectionWorkload;
use super::types::{
    CheckResult, DatabaseSnapshot, LogEntries, OP_HEARTBEAT, OP_REGISTER, OP_RESIGN,
    OP_TRY_BECOME_LEADER,
};

impl LeaderElectionWorkload {
    /// Invariant 1: Dual-Path Validation (AtomicOps pattern)
    ///
    /// Replay log entries (in true FDB commit order via versionstamp keys)
    /// to compute expected leader state, then compare with actual FDB state.
    ///
    /// This invariant subsumes both safety (at most one leader) and ballot
    /// conservation (ballot matches claims) by verifying exact state equality.
    ///
    /// Ballots are globally monotonic: a resignation does NOT reset the ballot,
    /// it leaves a vacant record that preserves it. So the replay tracks the
    /// leader identity and the running ballot independently.
    pub(crate) fn verify_dual_path(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        // Path 1: Replay logs in versionstamp order (true commit order)
        let mut expected_leader: Option<String> = None;
        let mut expected_ballot: u64 = 0;

        for entry in entries {
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                expected_ballot = entry.ballot;
                expected_leader = Some(format!("process_{}", entry.client_id));
            }
            if entry.op_type == OP_RESIGN && entry.success {
                let resigning = format!("process_{}", entry.client_id);
                if expected_leader.as_ref() == Some(&resigning) {
                    // Vacant record: no holder, but the ballot is preserved.
                    expected_leader = None;
                }
            }
        }

        // Path 2: Actual FDB state. A vacant record has no holder but a
        // preserved ballot; a live record binds both.
        let (actual_leader, actual_ballot) = match &snapshot.leader_state {
            Some(l) if l.is_vacant() => (None, l.ballot),
            Some(l) => (Some(l.leader_id.clone()), l.ballot),
            None => (None, 0),
        };

        // Compare
        let leader_matches = expected_leader == actual_leader;
        let ballot_matches = expected_ballot == actual_ballot;

        if leader_matches && ballot_matches {
            (
                true,
                format!("Dual-path OK: leader={actual_leader:?}, ballot={actual_ballot}"),
            )
        } else {
            (
                false,
                format!(
                    "Dual-path MISMATCH: expected ({expected_leader:?}, {expected_ballot}), actual ({actual_leader:?}, {actual_ballot})"
                ),
            )
        }
    }

    /// Invariant 2: Leader Is Candidate - Structural Integrity
    ///
    /// An active leader must be in the candidate list. A vacant record (left by
    /// a resignation) has no holder and is skipped. If the leader's lease has
    /// expired, the candidate may have been evicted, which is valid.
    pub(crate) fn verify_leader_is_candidate(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        match &snapshot.leader_state {
            Some(leader) if leader.is_vacant() => (
                true,
                "Vacant record (resigned), no active leader".to_string(),
            ),
            Some(leader) => {
                // Check if lease has expired - if so, candidate eviction is valid
                let lease_expiry_secs = leader.lease_expiry_nanos as f64 / 1_000_000_000.0;
                let lease_expired = current_time > lease_expiry_secs;

                if lease_expired {
                    // Lease expired - candidate may have been evicted, this is OK
                    return (
                        true,
                        format!(
                            "Leader {} lease expired ({:.3} > {:.3}), candidate eviction valid",
                            leader.leader_id, current_time, lease_expiry_secs
                        ),
                    );
                }

                let is_candidate = snapshot
                    .candidates
                    .iter()
                    .any(|c| c.process_id == leader.leader_id);

                if is_candidate {
                    (
                        true,
                        format!("Leader {} is registered candidate", leader.leader_id),
                    )
                } else {
                    let candidate_ids: Vec<_> = snapshot
                        .candidates
                        .iter()
                        .map(|c| c.process_id.as_str())
                        .collect();
                    (
                        false,
                        format!(
                            "Leader {} not in candidate list (candidates: {:?})",
                            leader.leader_id, candidate_ids
                        ),
                    )
                }
            }
            None => (true, "No leader, invariant trivially holds".to_string()),
        }
    }

    /// Invariant 3: Candidate Timestamps - No Future Timestamps
    ///
    /// No candidate heartbeat timestamp should be implausibly far in the future.
    /// The tolerance is the global worst-case one-sided clock deviation plus a
    /// small margin for scheduling/rounding; it collapses toward zero in the
    /// strict (zero-skew) configuration.
    pub(crate) fn verify_candidate_timestamps(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Tolerance tracks the configured clock-skew bound (+ small margin).
        let tolerance = self.max_clock_skew_secs + 0.5;

        for candidate in &snapshot.candidates {
            let heartbeat_secs = candidate.last_heartbeat_nanos as f64 / 1_000_000_000.0;

            if heartbeat_secs > current_time + tolerance {
                issues.push(format!(
                    "{} has future timestamp: {:.3} > {:.3} (+{:.3} tol)",
                    candidate.process_id, heartbeat_secs, current_time, tolerance
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} candidates have valid timestamps",
                    snapshot.candidates.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 4: Leadership Sequence - Monotonic Per-Client Operations
    ///
    /// For each client, their op_nums must be strictly increasing.
    pub(crate) fn verify_leadership_sequence(&self, entries: &LogEntries) -> (bool, String) {
        let mut client_last_op: BTreeMap<i32, u64> = BTreeMap::new();
        let mut leadership_count: BTreeMap<i32, usize> = BTreeMap::new();
        let mut violations = Vec::new();

        for entry in entries {
            // Track last op_num per client
            if let Some(last_op) = client_last_op.get(&entry.client_id)
                && entry.op_num <= *last_op
            {
                violations.push(format!(
                    "Client {} op_num {} not greater than previous {}",
                    entry.client_id, entry.op_num, last_op
                ));
            }
            client_last_op.insert(entry.client_id, entry.op_num);

            // Count leadership claims
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                *leadership_count.entry(entry.client_id).or_default() += 1;
            }
        }

        let clients_with_leadership = leadership_count.len();

        if violations.is_empty() {
            (
                true,
                format!(
                    "Leadership sequence valid ({clients_with_leadership} clients claimed leadership)"
                ),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 5: Registration Coverage - All Leaders Were Registered
    ///
    /// Every client that successfully claimed leadership must have a prior
    /// registration entry in the log.
    pub(crate) fn verify_registration_coverage(&self, entries: &LogEntries) -> (bool, String) {
        // Track which clients have registered (either success or attempted)
        let mut registered_clients: BTreeMap<i32, bool> = BTreeMap::new();
        let mut clients_with_leadership: BTreeMap<i32, usize> = BTreeMap::new();

        for entry in entries {
            // Count any registration attempt (success or failure indicates the client tried)
            if entry.op_type == OP_REGISTER {
                registered_clients.insert(entry.client_id, entry.success);
            }

            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                *clients_with_leadership.entry(entry.client_id).or_default() += 1;
            }
        }

        // Check for clients that became leader but have no registration entry at all
        let mut unregistered_leaders = Vec::new();
        for (client_id, count) in &clients_with_leadership {
            if !registered_clients.contains_key(client_id) {
                unregistered_leaders.push(format!("Client {client_id} ({count} claims)"));
            }
        }

        if unregistered_leaders.is_empty() {
            let leadership_count = clients_with_leadership.len();
            let registered_count = registered_clients.len();
            (
                true,
                format!(
                    "All {leadership_count} clients with leadership have registration entries ({registered_count} total registered)"
                ),
            )
        } else {
            let unregistered_count = unregistered_leaders.len();
            let unregistered_list = unregistered_leaders.join(", ");
            (
                false,
                format!(
                    "VIOLATION: {unregistered_count} clients claimed leadership without registration: {unregistered_list}"
                ),
            )
        }
    }

    /// Invariant 6: Ballot Succession
    ///
    /// Every successful leadership claim must have `ballot == previous_ballot + 1`.
    /// `previous_ballot` is the ballot read in the same transaction, so this is
    /// the authoritative predecessor. Because ballots are globally monotonic and
    /// never reset (resign leaves a vacant record that preserves the ballot),
    /// this holds unconditionally for every claim: first claim (`0 -> 1`), steal,
    /// preemption, refresh, and post-resign claim alike.
    pub(crate) fn verify_ballot_succession(&self, entries: &LogEntries) -> (bool, String) {
        let mut violations = Vec::new();
        let mut claims = 0;

        for entry in entries {
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                claims += 1;
                if entry.ballot != entry.previous_ballot + 1 {
                    violations.push(format!(
                        "Client {} got ballot {} but previous was {} (expected {})",
                        entry.client_id,
                        entry.ballot,
                        entry.previous_ballot,
                        entry.previous_ballot + 1
                    ));
                }
            }
        }

        if violations.is_empty() {
            (
                true,
                format!("Ballot succession OK ({claims} claims, each previous+1)"),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 7: One Value Per Ballot (Paxos Safety)
    ///
    /// Each ballot number must map to exactly one client, globally. Since
    /// ballots are strictly increasing across the whole run, a ballot claimed
    /// by two different clients would indicate broken conflict ranges or ballot
    /// assignment logic.
    pub(crate) fn verify_one_value_per_ballot(&self, entries: &LogEntries) -> (bool, String) {
        let mut ballot_to_client: BTreeMap<u64, i32> = BTreeMap::new();
        let mut violations = Vec::new();

        for entry in entries {
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                if entry.ballot == 0 {
                    continue; // Skip ballot 0 (initial state)
                }

                if let Some(&prev_client) = ballot_to_client.get(&entry.ballot)
                    && prev_client != entry.client_id
                {
                    violations.push(format!(
                        "Ballot {} claimed by client {} and client {}",
                        entry.ballot, prev_client, entry.client_id
                    ));
                }
                ballot_to_client.insert(entry.ballot, entry.client_id);
            }
        }

        if violations.is_empty() {
            (
                true,
                format!("Paxos safety OK: {} unique ballots", ballot_to_client.len()),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 8: Lease Expiry After Claim Time
    ///
    /// Every successful leadership claim must have a lease that expires AFTER
    /// the claim was made.
    pub(crate) fn verify_lease_expiry_after_claim(&self, entries: &LogEntries) -> (bool, String) {
        let mut violations = Vec::new();
        let mut valid_leases = 0;

        for entry in entries {
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                let claim_time = entry.claim_timestamp_nanos;
                let lease_expiry = entry.lease_expiry_nanos;

                if claim_time <= 0 {
                    // Skip entries without claim timestamp (shouldn't happen after update)
                    continue;
                }

                if lease_expiry <= claim_time {
                    violations.push(format!(
                        "Client {} claimed at {} but lease expires at {} (already expired or invalid)",
                        entry.client_id,
                        claim_time as f64 / 1_000_000_000.0,
                        lease_expiry as f64 / 1_000_000_000.0
                    ));
                } else {
                    valid_leases += 1;
                }
            }
        }

        if violations.is_empty() {
            (
                true,
                format!("Lease timing OK: {valid_leases} claims with valid future leases"),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 9: No Belief Overlap (real mutual exclusion)
    ///
    /// A belief overlap - two clients simultaneously thinking they hold a valid
    /// lease - can only be created by a *steal*: one client taking the leader
    /// record from a different client that neither resigned nor lost it
    /// legitimately. Resign -> vacant -> claim handoffs create no overlap (the
    /// resigning client stops believing when it resigns), so only steals are
    /// checked.
    ///
    /// Walking the log in commit (versionstamp) order, track the current record
    /// holder and its last-refresh time. When a *different* client becomes
    /// leader over a still-held (non-vacant) record, that is a steal: the victim
    /// keeps believing until `victim_last_refresh + lease_duration` (it is
    /// unaware), so the stealer must not claim before then. A legitimate steal
    /// only happens after observing the record unchanged for a full
    /// `lease_duration`, so `stealer_claim >= victim_last_refresh + lease` and
    /// the overlap is <= 0.
    ///
    /// Times are each client's own clock samples. The workload samples them
    /// *inside* the transaction, after the leader read (see `workload.rs`), so
    /// they are read-anchored: they reflect real record-existence time rather
    /// than a stale op-start sample that retry/clogging latency would inflate.
    /// For a steal the victim genuinely refreshed, then time passed, then the
    /// stealer claimed, so commit order and sampled order agree.
    ///
    /// Only meaningful with preemption disabled: preemption deliberately steals
    /// a live lease, so with it enabled the invariant reports an informational
    /// skip (fencing ballots provide safety there). Tolerance is
    /// `2 * max_clock_skew_secs` plus a small sampling-slack floor; in practice
    /// steals land well after the lease expires (negative overlap), and a
    /// genuinely broken observation leaks close to a full `lease_duration`.
    pub(crate) fn verify_no_belief_overlap(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        let config = match &snapshot.config {
            Some(c) => c,
            None => {
                return (
                    true,
                    "No config available, skipping belief-overlap check".to_string(),
                );
            }
        };

        if config.allow_preemption {
            return (
                true,
                "Skipped: preemption enabled - belief-level exclusion intentionally not enforced; \
                 safety relies on fencing ballots"
                    .to_string(),
            );
        }

        let lease_secs = config.lease_duration.as_secs_f64();

        // Belief times come from `context.now()` sampled at each op's *start*,
        // not its commit. A stealer's observation window is anchored to its own
        // op-start samples, which can slightly precede the victim's record
        // commit, so a legitimate steal can appear to land a fraction of a
        // second before the victim's lease horizon. Absorb that sampling slack.
        // A genuinely broken observation would leak close to a full
        // `lease_duration`, far above this floor, so real violations still fail.
        const SAMPLING_SLACK_SECS: f64 = 0.5;
        let tolerance = 2.0 * self.max_clock_skew_secs + SAMPLING_SLACK_SECS;

        // Current record holder in commit order: (client_id, last_refresh_secs).
        // None means the record is vacant (after a resign) or never claimed.
        let mut holder: Option<(i32, f64)> = None;
        let mut steals = 0usize;
        // Largest overlap seen (negative = margin: the steal landed this long
        // after the victim's lease expired). Reported for reviewer visibility.
        let mut max_overlap = f64::NEG_INFINITY;

        for entry in entries {
            let s = entry.sim_time_nanos as f64 / 1_000_000_000.0;

            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                if let Some((victim, victim_last_refresh)) = holder
                    && victim != entry.client_id
                {
                    // Steal: the victim believes it holds the lease until its
                    // lease horizon; the stealer must not claim before then.
                    steals += 1;
                    let victim_belief_end = victim_last_refresh + lease_secs;
                    let overlap = victim_belief_end - s;
                    if overlap > max_overlap {
                        max_overlap = overlap;
                    }
                    if overlap > tolerance {
                        return (
                            false,
                            format!(
                                "Belief overlap {overlap:.3}s (> {tolerance:.3}s tol): client {} \
                                 stole leadership at {s:.3} while client {victim}'s lease was valid \
                                 until {victim_belief_end:.3} (last refresh {victim_last_refresh:.3})",
                                entry.client_id
                            ),
                        );
                    }
                }
                holder = Some((entry.client_id, s));
            }

            if entry.op_type == OP_RESIGN
                && entry.success
                && let Some((h, _)) = holder
                && h == entry.client_id
            {
                // Clean handoff: the record is now vacant, no belief carries over.
                holder = None;
            }
        }

        let margin = if steals == 0 {
            "no steals".to_string()
        } else {
            format!("worst approach {max_overlap:+.3}s vs {tolerance:.3}s tol")
        };
        (
            true,
            format!("No belief overlap across {steals} steals ({margin})"),
        )
    }

    /// Invariant 10: Progress Made
    ///
    /// Guards against a vacuous pass: a run where nothing happened (empty log,
    /// or no leader ever elected) must not be reported as success. Minimums are
    /// configurable (`minLeadershipClaims`, `minHeartbeats`) but floored at 1.
    pub(crate) fn verify_progress_made(&self, entries: &LogEntries) -> (bool, String) {
        if entries.is_empty() {
            return (
                false,
                "No log entries: the run made no observable progress".to_string(),
            );
        }

        let mut claims = 0usize;
        let mut heartbeats = 0usize;
        for entry in entries {
            if entry.op_type == OP_TRY_BECOME_LEADER && entry.success && entry.became_leader {
                claims += 1;
            }
            if entry.op_type == OP_HEARTBEAT && entry.success {
                heartbeats += 1;
            }
        }

        if claims < self.min_leadership_claims {
            return (
                false,
                format!(
                    "Only {claims} successful leadership claims (need >= {})",
                    self.min_leadership_claims
                ),
            );
        }
        if heartbeats < self.min_heartbeats {
            return (
                false,
                format!(
                    "Only {heartbeats} successful heartbeats (need >= {})",
                    self.min_heartbeats
                ),
            );
        }

        (
            true,
            format!("Progress OK: {claims} leadership claims, {heartbeats} heartbeats"),
        )
    }

    /// Run all invariant checks and return results
    pub(crate) fn run_all_invariant_checks(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> CheckResult {
        let current_time = self.context.now();

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // 1: Dual-Path Validation (keystone: safety + ballot conservation)
        let (pass, detail) = self.verify_dual_path(entries, snapshot);
        results.push(("DualPathValidation", pass, detail));

        // 2: Leader Is Candidate (structural integrity)
        let (pass, detail) = self.verify_leader_is_candidate(snapshot, current_time);
        results.push(("LeaderIsCandidate", pass, detail));

        // 3: Candidate Timestamps (no implausible future timestamps)
        let (pass, detail) = self.verify_candidate_timestamps(snapshot, current_time);
        results.push(("CandidateTimestamps", pass, detail));

        // 4: Leadership Sequence (per-client monotonic op_nums)
        let (pass, detail) = self.verify_leadership_sequence(entries);
        results.push(("LeadershipSequence", pass, detail));

        // 5: Registration Coverage (all leaders were registered)
        let (pass, detail) = self.verify_registration_coverage(entries);
        results.push(("RegistrationCoverage", pass, detail));

        // 6: Ballot Succession (each claim is exactly previous+1, globally)
        let (pass, detail) = self.verify_ballot_succession(entries);
        results.push(("BallotSuccession", pass, detail));

        // 7: One Value Per Ballot (Paxos safety)
        let (pass, detail) = self.verify_one_value_per_ballot(entries);
        results.push(("OneValuePerBallot", pass, detail));

        // 8: Lease Expiry After Claim (lease validity)
        let (pass, detail) = self.verify_lease_expiry_after_claim(entries);
        results.push(("LeaseExpiryAfterClaim", pass, detail));

        // 9: No Belief Overlap (real mutual exclusion, preemption disabled)
        let (pass, detail) = self.verify_no_belief_overlap(entries, snapshot);
        results.push(("NoBeliefOverlap", pass, detail));

        // 10: Progress Made (no vacuous passes)
        let (pass, detail) = self.verify_progress_made(entries);
        results.push(("ProgressMade", pass, detail));

        // Log each result
        for (name, passed, detail) in &results {
            if *passed {
                self.trace_invariant_pass(name, detail);
            } else {
                self.trace_invariant_fail(name, "invariant holds", detail);
            }
        }

        let passed = results.iter().filter(|(_, p, _)| *p).count();
        let failed = results.len() - passed;

        self.trace_check_summary(passed, failed);

        CheckResult {
            passed,
            failed,
            results,
        }
    }
}
