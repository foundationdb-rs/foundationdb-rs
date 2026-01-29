//! Invariant verification methods for leader election simulation.
//!
//! Contains the core Active Disk Paxos invariants:
//! 1. Dual-Path Validation - Replay logs vs FDB state must match (safety + conservation)
//! 2. Leader Is Candidate - Leader must be a registered candidate (structural integrity)
//! 3. Candidate Timestamps - No future timestamps in candidates
//! 4. Leadership Sequence - Per-client monotonic op_nums
//! 5. Registration Coverage - All leadership claims come from registered processes
//!
//! Note: Log entries are now in FDB commit order (versionstamp-ordered keys),
//! so we have true causal ordering without clock skew issues.

use std::collections::BTreeMap;

use super::types::{
    CheckResult, DatabaseSnapshot, LogEntries, OP_REGISTER, OP_RESIGN, OP_TRY_BECOME_LEADER,
};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Invariant 1: Dual-Path Validation (AtomicOps pattern)
    ///
    /// Replay log entries (in true FDB commit order via versionstamp keys)
    /// to compute expected leader state, then compare with actual FDB state.
    ///
    /// This is the key insight: with versionstamp-ordered keys, we have TRUE
    /// causal ordering, so replay gives us the exact expected state.
    ///
    /// This invariant subsumes both safety (at most one leader) and ballot
    /// conservation (ballot matches claims) by verifying exact state equality.
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
                expected_ballot += 1;
                expected_leader = Some(format!("process_{}", entry.client_id));
            }
            if entry.op_type == OP_RESIGN && entry.success {
                let resigning = format!("process_{}", entry.client_id);
                if expected_leader.as_ref() == Some(&resigning) {
                    expected_leader = None;
                    expected_ballot = 0; // Ballot resets after resign
                }
            }
        }

        // Path 2: Actual FDB state
        let (actual_leader, actual_ballot) = match &snapshot.leader_state {
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
    /// The current leader must be in the candidate list.
    /// This verifies the data structure invariant that leadership
    /// can only be claimed by registered candidates.
    ///
    /// NOTE: If the leader's lease has expired, the candidate may have been
    /// evicted due to timeout. This is valid - we only enforce this invariant
    /// for leaders with active leases.
    pub(crate) fn verify_leader_is_candidate(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        match &snapshot.leader_state {
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
    /// All candidate heartbeat timestamps should be in the past or present,
    /// not in the future. Future timestamps would indicate clock skew issues
    /// or bugs in timestamp handling.
    ///
    /// With clock skew simulation enabled, we allow tolerance for:
    ///   - clock_offset: up to ±1.0s (Extreme level)
    ///   - drift ahead: up to 1.0s (max_ahead)
    ///   - jitter: up to 1.1x multiplier on offset
    ///
    /// Max theoretical offset: (1.0 + 1.0) * 1.1 = 2.2s, use 3s for margin.
    pub(crate) fn verify_candidate_timestamps(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Tolerance accounts for clock skew simulation (Extreme level + jitter)
        const MAX_CLOCK_SKEW_TOLERANCE: f64 = 3.0;

        for candidate in &snapshot.candidates {
            let heartbeat_secs = candidate.last_heartbeat_nanos as f64 / 1_000_000_000.0;

            if heartbeat_secs > current_time + MAX_CLOCK_SKEW_TOLERANCE {
                issues.push(format!(
                    "{} has future timestamp: {:.3} > {:.3}",
                    candidate.process_id, heartbeat_secs, current_time
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
    /// For each client, their op_nums should be monotonically increasing.
    /// This verifies the log entries are properly ordered per client.
    /// (Note: With versionstamp ordering, we have true commit order, so this
    /// is a simpler check now - just verify op_nums are increasing per client)
    pub(crate) fn verify_leadership_sequence(&self, entries: &LogEntries) -> (bool, String) {
        let mut client_last_op: BTreeMap<i32, u64> = BTreeMap::new();
        let mut leadership_count: BTreeMap<i32, usize> = BTreeMap::new();
        let mut violations = Vec::new();

        for entry in entries {
            // Track last op_num per client
            if let Some(last_op) = client_last_op.get(&entry.client_id) {
                if entry.op_num <= *last_op {
                    violations.push(format!(
                        "Client {} op_num {} not greater than previous {}",
                        entry.client_id, entry.op_num, last_op
                    ));
                }
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
                format!("Leadership sequence valid ({clients_with_leadership} clients claimed leadership)"),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 5: Registration Coverage - All Leaders Were Registered
    ///
    /// Every client that successfully claimed leadership must have
    /// previously registered (logged a Register operation).
    /// This verifies the registration requirement of the protocol.
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
                format!("All {leadership_count} clients with leadership have registration entries ({registered_count} total registered)"),
            )
        } else {
            let unregistered_count = unregistered_leaders.len();
            let unregistered_list = unregistered_leaders.join(", ");
            (
                false,
                format!("VIOLATION: {unregistered_count} clients claimed leadership without registration: {unregistered_list}"),
            )
        }
    }

    /// Run all invariant checks and return results
    pub(crate) fn run_all_invariant_checks(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> CheckResult {
        let current_time = self.context.now();

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // Invariant 1: Dual-Path Validation (most important!)
        // Replay logs in true commit order, compare with FDB state
        // Subsumes safety (at most one leader) and ballot conservation
        let (pass, detail) = self.verify_dual_path(entries, snapshot);
        results.push(("DualPathValidation", pass, detail));

        // Invariant 2: Leader Is Candidate (structural integrity)
        let (pass, detail) = self.verify_leader_is_candidate(snapshot, current_time);
        results.push(("LeaderIsCandidate", pass, detail));

        // Invariant 3: Candidate Timestamps (no future timestamps)
        let (pass, detail) = self.verify_candidate_timestamps(snapshot, current_time);
        results.push(("CandidateTimestamps", pass, detail));

        // Invariant 4: Leadership Sequence (per-client monotonic op_nums)
        let (pass, detail) = self.verify_leadership_sequence(entries);
        results.push(("LeadershipSequence", pass, detail));

        // Invariant 5: Registration Coverage (all leaders were registered)
        let (pass, detail) = self.verify_registration_coverage(entries);
        results.push(("RegistrationCoverage", pass, detail));

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
