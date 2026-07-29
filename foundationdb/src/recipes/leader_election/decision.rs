// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! The pure decision core
//!
//! Every rule about who may take, keep or release a term lives here, as
//! functions of the record that was read, the caller's own observation state,
//! and a caller-supplied instant. Nothing in this module touches a clock, a
//! random source, or the network.
//!
//! That purity is a stability guarantee the simulation depends on: replaying a
//! `db.run` closure re-runs these functions on the same inputs and gets the
//! same answer, so a retry never double-bumps a ballot.

use super::types::*;
use std::time::Duration;

/// Who the caller is claiming as, and what it already did
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClaimIdentity<'a> {
    /// The identifier being claimed under; validated non-empty by the caller
    pub leader_id: &'a str,
    /// The campaign's per-term token; validated non-zero by the caller
    pub token: ClaimToken,
    /// The ballot a previous execution of this attempt wrote, if any
    pub issued_ballot: Option<u64>,
}

/// What [`decide_claim`] concluded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimDecision {
    /// Take the term at `new_ballot`
    Claim {
        new_ballot: u64,
        event: HistoryEventKind,
    },
    /// The record already says we hold this term: an earlier execution of this
    /// attempt committed and the reply was lost. Adopt it, do not bump.
    AlreadyWon,
    /// Somebody else holds the term and has not been still long enough
    Deny { remaining: Duration },
    /// This attempt is spent; see [`ClaimOutcome::Superseded`]
    Superseded,
    /// The ballot space is used up
    Exhausted,
}

/// Decide whether the caller may take the term.
///
/// Rules, in the order they are applied:
///
/// 1. The record matches our full ownership tuple (leader id *and* token):
///    our earlier write committed. Adopt it without bumping the ballot.
/// 2. We wrote a claim whose fate is unknown and the record now sits at or
///    above the ballot we wrote: our claim may have committed and already been
///    taken from us. Terminal.
/// 3. The record is absent: take ballot 1.
/// 4. The record is vacant: take `ballot + 1` with no wait. The previous
///    holder said it was done, which is stronger information than a lease
///    running out.
/// 5. Otherwise time the record. Only when the same `(ballot, generation)` has
///    been observed continuously for at least the lease it advertises (clamped
///    to `max_advertised_lease`) may we take `ballot + 1`.
///
/// `observation` is updated in place and must be threaded back into the next
/// call. A `now` that regressed saturates the measured elapsed time to zero,
/// so a clock going backwards can only delay a steal.
pub(crate) fn decide_claim(
    current: Option<&LeaderRecord>,
    me: &ClaimIdentity<'_>,
    now: Duration,
    max_advertised_lease: Duration,
    observation: &mut LeaseObservation,
) -> ClaimDecision {
    let record = match current {
        None => {
            observation.reset();
            return ClaimDecision::Claim {
                new_ballot: 1,
                event: HistoryEventKind::Claim,
            };
        }
        Some(record) => record,
    };

    // 1. Recovery: this is our own record, written by an execution whose reply
    //    we never saw. Matching the token alone is not enough; a record only
    //    proves ownership when the whole tuple matches.
    if record.is_held_by(me.leader_id, me.token) {
        observation.reset();
        return ClaimDecision::AlreadyWon;
    }

    // 2. A foreign record at or past the ballot we wrote means our own claim
    //    either never landed or landed and was taken from us, and no read can
    //    tell those apart. Retire the attempt.
    if let Some(issued) = me.issued_ballot {
        if record.ballot() >= issued {
            observation.reset();
            return ClaimDecision::Superseded;
        }
    }

    // 4. A resigned term is free immediately.
    if record.is_vacant() {
        observation.reset();
        return next_ballot(record.ballot(), HistoryEventKind::Claim);
    }

    // 5. Somebody else holds it. Time the identity.
    let advertised = match record.lease() {
        Some(lease) => lease.clamped_to(max_advertised_lease),
        // Unreachable: a non-vacant record always advertises a lease.
        None => max_advertised_lease,
    };
    let held_still_for = observation.note(record.identity(), now);

    if held_still_for >= advertised {
        next_ballot(record.ballot(), HistoryEventKind::Steal)
    } else {
        ClaimDecision::Deny {
            remaining: advertised - held_still_for,
        }
    }
}

fn next_ballot(current: u64, event: HistoryEventKind) -> ClaimDecision {
    if current >= MAX_BALLOT {
        ClaimDecision::Exhausted
    } else {
        ClaimDecision::Claim {
            new_ballot: current + 1,
            event,
        }
    }
}

/// What [`decide_refresh`] concluded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshDecision {
    /// Write generation + 1 at the same ballot
    Bump,
    /// The bump is already in the record: our earlier renewal committed and
    /// the reply was lost
    AlreadyBumped,
    /// The term is no longer ours
    Lost,
}

/// Decide whether the caller may renew its term.
///
/// A renewal is a compare-and-set on the full ownership tuple: ballot, leader
/// id, token *and* the generation the renewal was issued against. Matching
/// generation + 1 instead means our own write landed and only the reply was
/// lost, which is not a reason to write again.
pub(crate) fn decide_refresh(
    current: Option<&LeaderRecord>,
    grant: &LeaseGrant,
    expected_generation: u64,
) -> RefreshDecision {
    let record = match current {
        None => return RefreshDecision::Lost,
        Some(record) => record,
    };

    if record.ballot() != grant.ballot() || !record.is_held_by(grant.leader_id(), grant.token()) {
        return RefreshDecision::Lost;
    }

    if record.generation() == expected_generation {
        RefreshDecision::Bump
    } else if record.generation() == expected_generation.wrapping_add(1) {
        RefreshDecision::AlreadyBumped
    } else {
        RefreshDecision::Lost
    }
}

/// What [`decide_resign`] concluded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResignDecision {
    /// Write the vacant record, preserving ballot and generation
    Vacate,
    /// The term is already vacant at our own ballot: our earlier resign
    /// committed
    AlreadyVacant,
    /// The term is not ours to give up
    NotHolder,
}

/// Decide whether the caller may vacate its term.
///
/// The generation is deliberately not part of the match: a renewal may have
/// moved it since the grant was taken, and that does not change who holds the
/// term.
pub(crate) fn decide_resign(current: Option<&LeaderRecord>, grant: &LeaseGrant) -> ResignDecision {
    let record = match current {
        None => return ResignDecision::NotHolder,
        Some(record) => record,
    };

    if record.is_held_by(grant.leader_id(), grant.token()) && record.ballot() == grant.ballot() {
        return ResignDecision::Vacate;
    }

    // A ballot identifies a term uniquely, so a vacant record at our ballot
    // can only be one we wrote ourselves.
    if record.is_vacant() && record.ballot() == grant.ballot() {
        return ResignDecision::AlreadyVacant;
    }

    ResignDecision::NotHolder
}

#[cfg(test)]
mod tests {
    use super::super::LeaderElection;
    use super::super::codec;
    use super::super::errors::LeaderElectionError;
    use super::*;

    const LEASE: Duration = Duration::from_secs(10);
    const MAX: Duration = Duration::from_secs(600);

    fn token(byte: u8) -> ClaimToken {
        ClaimToken::from_bytes([byte; 16])
    }

    fn lease(secs: u64) -> LeaseDuration {
        LeaseDuration::new(Duration::from_secs(secs)).unwrap()
    }

    fn held(ballot: u64, generation: u64, id: &str, tok: u8) -> LeaderRecord {
        codec::claimed_record(ballot, generation, id, token(tok), lease(10))
    }

    fn me(id: &str, tok: u8) -> ClaimIdentity<'_> {
        ClaimIdentity {
            leader_id: id,
            token: token(tok),
            issued_ballot: None,
        }
    }

    fn grant_for(record: &LeaderRecord, acquired_at: Duration) -> LeaseGrant {
        LeaseGrant {
            ballot: record.ballot(),
            generation: record.generation(),
            leader_id: record.leader_id().unwrap().to_string(),
            token: record.token(),
            lease: record.lease().unwrap(),
            acquired_at,
        }
    }

    // ------------------------------------------------------------------
    // decide_claim
    // ------------------------------------------------------------------

    #[test]
    fn absent_record_claims_ballot_one() {
        let mut obs = LeaseObservation::new();
        let decision = decide_claim(None, &me("a", 1), Duration::ZERO, MAX, &mut obs);
        assert_eq!(
            decision,
            ClaimDecision::Claim {
                new_ballot: 1,
                event: HistoryEventKind::Claim
            }
        );
    }

    #[test]
    fn vacant_record_is_reclaimed_instantly_at_next_ballot() {
        // A resigned term carries no wait: the previous holder told us it was
        // done, which a lease expiry only ever guesses at.
        let record = codec::vacant_record(7, 3);
        let mut obs = LeaseObservation::new();
        let decision = decide_claim(
            Some(&record),
            &me("a", 1),
            Duration::from_secs(1),
            MAX,
            &mut obs,
        );
        assert_eq!(
            decision,
            ClaimDecision::Claim {
                new_ballot: 8,
                event: HistoryEventKind::Claim
            }
        );
        assert_eq!(obs.observed_identity(), None);
    }

    #[test]
    fn foreign_record_is_denied_until_a_full_lease_has_been_observed() {
        let record = held(4, 0, "other", 9);
        let mut obs = LeaseObservation::new();

        // First sighting starts the clock; it can never itself steal.
        let first = decide_claim(Some(&record), &me("a", 1), Duration::ZERO, MAX, &mut obs);
        assert_eq!(
            first,
            ClaimDecision::Deny {
                remaining: Duration::from_secs(10)
            }
        );

        let mid = decide_claim(
            Some(&record),
            &me("a", 1),
            Duration::from_secs(9),
            MAX,
            &mut obs,
        );
        assert_eq!(
            mid,
            ClaimDecision::Deny {
                remaining: Duration::from_secs(1)
            }
        );

        let steal = decide_claim(Some(&record), &me("a", 1), LEASE, MAX, &mut obs);
        assert_eq!(
            steal,
            ClaimDecision::Claim {
                new_ballot: 5,
                event: HistoryEventKind::Steal
            }
        );
    }

    #[test]
    fn a_renewal_resets_the_observation_window() {
        // The victim bumping its generation is exactly what a live leader looks
        // like, so the contender's timer must start over.
        let mut obs = LeaseObservation::new();
        let before = held(4, 0, "other", 9);
        decide_claim(Some(&before), &me("a", 1), Duration::ZERO, MAX, &mut obs);
        decide_claim(
            Some(&before),
            &me("a", 1),
            Duration::from_secs(9),
            MAX,
            &mut obs,
        );

        let renewed = held(4, 1, "other", 9);
        let decision = decide_claim(
            Some(&renewed),
            &me("a", 1),
            Duration::from_secs(9),
            MAX,
            &mut obs,
        );
        assert_eq!(decision, ClaimDecision::Deny { remaining: LEASE });

        // And the deadline is now measured from the renewal, not the original
        // sighting.
        let still_denied = decide_claim(
            Some(&renewed),
            &me("a", 1),
            Duration::from_secs(18),
            MAX,
            &mut obs,
        );
        assert_eq!(
            still_denied,
            ClaimDecision::Deny {
                remaining: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn a_new_term_resets_the_observation_window() {
        let mut obs = LeaseObservation::new();
        let first = held(4, 0, "other", 9);
        decide_claim(Some(&first), &me("a", 1), Duration::ZERO, MAX, &mut obs);

        let second = held(5, 0, "third", 8);
        let decision = decide_claim(
            Some(&second),
            &me("a", 1),
            Duration::from_secs(9),
            MAX,
            &mut obs,
        );
        assert_eq!(decision, ClaimDecision::Deny { remaining: LEASE });
    }

    #[test]
    fn a_regressed_clock_can_only_delay_a_steal() {
        let record = held(4, 0, "other", 9);
        let mut obs = LeaseObservation::new();
        decide_claim(
            Some(&record),
            &me("a", 1),
            Duration::from_secs(100),
            MAX,
            &mut obs,
        );

        // Time goes backwards: elapsed saturates to zero rather than wrapping
        // into a huge value that would authorize an immediate steal.
        let decision = decide_claim(
            Some(&record),
            &me("a", 1),
            Duration::from_secs(1),
            MAX,
            &mut obs,
        );
        assert_eq!(decision, ClaimDecision::Deny { remaining: LEASE });
    }

    #[test]
    fn an_over_long_advertised_lease_is_clamped_by_the_observer() {
        // Otherwise one misconfigured claimant could sterilize the election.
        let record = codec::claimed_record(4, 0, "other", token(9), lease(3600));
        let mut obs = LeaseObservation::new();
        let max = Duration::from_secs(30);

        decide_claim(Some(&record), &me("a", 1), Duration::ZERO, max, &mut obs);
        let decision = decide_claim(
            Some(&record),
            &me("a", 1),
            Duration::from_secs(30),
            max,
            &mut obs,
        );
        assert_eq!(
            decision,
            ClaimDecision::Claim {
                new_ballot: 5,
                event: HistoryEventKind::Steal
            }
        );
    }

    #[test]
    fn our_own_record_is_adopted_without_a_second_bump() {
        // The unknown-commit case: the write landed, the reply did not.
        let record = held(5, 0, "a", 1);
        let mut obs = LeaseObservation::new();
        let identity = ClaimIdentity {
            issued_ballot: Some(5),
            ..me("a", 1)
        };
        let decision = decide_claim(Some(&record), &identity, Duration::ZERO, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::AlreadyWon);
    }

    #[test]
    fn recovery_requires_the_whole_ownership_tuple() {
        // Same token under a different identifier is not our record. Treating
        // it as ours would hand leadership to a process that never won it.
        let record = held(5, 0, "somebody-else", 1);
        let mut obs = LeaseObservation::new();
        let identity = ClaimIdentity {
            issued_ballot: None,
            ..me("a", 1)
        };
        let decision = decide_claim(Some(&record), &identity, Duration::ZERO, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::Deny { remaining: LEASE });
    }

    #[test]
    fn a_maybe_committed_claim_overtaken_by_a_stranger_is_terminal() {
        // We wrote ballot 5 and lost the reply; the record now shows ballot 6
        // under somebody else. Whether we were ever leader is unknowable.
        let record = held(6, 0, "other", 9);
        let mut obs = LeaseObservation::new();
        let identity = ClaimIdentity {
            issued_ballot: Some(5),
            ..me("a", 1)
        };
        let decision = decide_claim(Some(&record), &identity, Duration::ZERO, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::Superseded);
    }

    #[test]
    fn a_maybe_committed_claim_is_not_superseded_by_an_older_record() {
        // Ballot 4 is strictly below the 5 we wrote, so our write demonstrably
        // never committed and the campaign can continue normally.
        let record = held(4, 0, "other", 9);
        let mut obs = LeaseObservation::new();
        let identity = ClaimIdentity {
            issued_ballot: Some(5),
            ..me("a", 1)
        };
        let decision = decide_claim(Some(&record), &identity, Duration::ZERO, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::Deny { remaining: LEASE });
    }

    #[test]
    fn replaying_the_same_inputs_yields_the_same_decision() {
        // `db.run` re-executes its closure; the decision core must not drift
        // between executions or a retry would double-bump the ballot.
        let record = held(4, 0, "other", 9);
        let mut a = LeaseObservation::new();
        let mut b = LeaseObservation::new();
        for now in [0u64, 3, 7, 10, 10] {
            let now = Duration::from_secs(now);
            let first = decide_claim(Some(&record), &me("a", 1), now, MAX, &mut a);
            let second = decide_claim(Some(&record), &me("a", 1), now, MAX, &mut b);
            assert_eq!(first, second);
        }
    }

    #[test]
    fn the_ballot_space_is_refused_before_it_overflows_a_rank() {
        // Ranks pack the ballot into 32 bits, so the protocol stops handing
        // out ballots before that encoding would lose information.
        let record = held(MAX_BALLOT, 0, "other", 9);
        let mut obs = LeaseObservation::new();
        decide_claim(Some(&record), &me("a", 1), Duration::ZERO, MAX, &mut obs);
        let decision = decide_claim(Some(&record), &me("a", 1), LEASE, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::Exhausted);

        let vacant = codec::vacant_record(MAX_BALLOT, 0);
        let mut obs = LeaseObservation::new();
        let decision = decide_claim(Some(&vacant), &me("a", 1), Duration::ZERO, MAX, &mut obs);
        assert_eq!(decision, ClaimDecision::Exhausted);

        // One below the cap still works, and its rank round-trips.
        let record = held(MAX_BALLOT - 1, 0, "other", 9);
        let mut obs = LeaseObservation::new();
        decide_claim(Some(&record), &me("a", 1), Duration::ZERO, MAX, &mut obs);
        assert_eq!(
            decide_claim(Some(&record), &me("a", 1), LEASE, MAX, &mut obs),
            ClaimDecision::Claim {
                new_ballot: MAX_BALLOT,
                event: HistoryEventKind::Steal
            }
        );
    }

    // ------------------------------------------------------------------
    // decide_refresh
    // ------------------------------------------------------------------

    #[test]
    fn a_renewal_bumps_the_generation_at_a_fixed_ballot() {
        let record = held(5, 2, "a", 1);
        let grant = grant_for(&record, Duration::ZERO);
        assert_eq!(
            decide_refresh(Some(&record), &grant, 2),
            RefreshDecision::Bump
        );
    }

    #[test]
    fn a_renewal_whose_reply_was_lost_is_recognized_not_repeated() {
        let record = held(5, 3, "a", 1);
        let grant = grant_for(&held(5, 2, "a", 1), Duration::ZERO);
        assert_eq!(
            decide_refresh(Some(&record), &grant, 2),
            RefreshDecision::AlreadyBumped
        );
    }

    #[test]
    fn a_renewal_is_lost_to_any_break_in_the_ownership_tuple() {
        let grant = grant_for(&held(5, 2, "a", 1), Duration::ZERO);

        // Somebody else took the term.
        assert_eq!(
            decide_refresh(Some(&held(6, 0, "other", 9)), &grant, 2),
            RefreshDecision::Lost
        );
        // Same ballot, different holder: cannot happen, but must not renew.
        assert_eq!(
            decide_refresh(Some(&held(5, 2, "other", 1)), &grant, 2),
            RefreshDecision::Lost
        );
        // Same holder, different token: a later term of the same process.
        assert_eq!(
            decide_refresh(Some(&held(5, 2, "a", 7)), &grant, 2),
            RefreshDecision::Lost
        );
        // A generation further ahead than our own write could explain.
        assert_eq!(
            decide_refresh(Some(&held(5, 9, "a", 1)), &grant, 2),
            RefreshDecision::Lost
        );
        // The term was resigned, or the record cleared entirely.
        assert_eq!(
            decide_refresh(Some(&codec::vacant_record(5, 2)), &grant, 2),
            RefreshDecision::Lost
        );
        assert_eq!(decide_refresh(None, &grant, 2), RefreshDecision::Lost);
    }

    // ------------------------------------------------------------------
    // decide_resign
    // ------------------------------------------------------------------

    #[test]
    fn resigning_a_held_term_vacates_it() {
        // Any generation of our own term resigns: a renewal may have moved it
        // since the grant was taken.
        let grant = grant_for(&held(5, 2, "a", 1), Duration::ZERO);
        assert_eq!(
            decide_resign(Some(&held(5, 2, "a", 1)), &grant),
            ResignDecision::Vacate
        );
        assert_eq!(
            decide_resign(Some(&held(5, 6, "a", 1)), &grant),
            ResignDecision::Vacate
        );
    }

    #[test]
    fn resigning_an_already_vacated_term_is_recognized() {
        let grant = grant_for(&held(5, 2, "a", 1), Duration::ZERO);
        assert_eq!(
            decide_resign(Some(&codec::vacant_record(5, 2)), &grant),
            ResignDecision::AlreadyVacant
        );
        // A vacancy at a later ballot belongs to somebody else's term.
        assert_eq!(
            decide_resign(Some(&codec::vacant_record(6, 0)), &grant),
            ResignDecision::NotHolder
        );
    }

    #[test]
    fn resigning_a_term_we_no_longer_hold_reports_not_holder() {
        let grant = grant_for(&held(5, 2, "a", 1), Duration::ZERO);
        assert_eq!(
            decide_resign(Some(&held(6, 0, "other", 9)), &grant),
            ResignDecision::NotHolder
        );
        assert_eq!(decide_resign(None, &grant), ResignDecision::NotHolder);
    }

    // ------------------------------------------------------------------
    // codec
    // ------------------------------------------------------------------

    #[test]
    fn an_occupied_record_round_trips() {
        let record = held(42, 7, "leader-a", 3);
        let decoded = codec::decode_record(&codec::encode_record(&record)).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.leader_id(), Some("leader-a"));
        assert_eq!(decoded.lease(), Some(lease(10)));
        assert!(!decoded.is_vacant());
        assert_eq!(
            decoded.identity(),
            RecordIdentity {
                ballot: 42,
                generation: 7
            }
        );
    }

    #[test]
    fn a_vacant_record_round_trips_and_keeps_its_ballot() {
        let record = codec::vacant_record(42, 7);
        let decoded = codec::decode_record(&codec::encode_record(&record)).unwrap();
        assert_eq!(decoded, record);
        assert!(decoded.is_vacant());
        assert_eq!(decoded.ballot(), 42);
        assert_eq!(decoded.leader_id(), None);
        assert_eq!(decoded.lease(), None);
    }

    #[test]
    fn a_resign_changes_the_term_marker_even_though_it_keeps_the_identity() {
        // Watches park on the term key; if a resign did not move its value, no
        // contender would ever wake up for it.
        let held_record = held(5, 2, "a", 1);
        let vacated = codec::vacant_record(5, 2);
        assert_ne!(
            codec::encode_term(&held_record),
            codec::encode_term(&vacated)
        );
    }

    #[test]
    fn a_renewal_is_invisible_to_the_term_marker() {
        // The opposite requirement: renewals must not wake every contender.
        assert_eq!(
            codec::encode_term(&held(5, 2, "a", 1)),
            codec::encode_term(&held(5, 2, "a", 1))
        );
        assert_ne!(
            codec::encode_term(&held(5, 2, "a", 1)),
            codec::encode_term(&held(5, 3, "a", 1))
        );
    }

    #[test]
    fn corrupt_records_fail_loudly() {
        use crate::tuple::pack;

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty value", Vec::new()),
            ("not a tuple", b"\xff\xff not a tuple".to_vec()),
            (
                // A well-formed tuple carrying the right schema version and
                // nothing else this protocol recognizes.
                "a tuple of another shape",
                pack(&(1u64, "not-a-ballot", 0i64, &[0u8; 12][..])),
            ),
            (
                "unknown schema version",
                pack(&(2u64, 1u64, 0u64, "a", &[1u8; 16][..], 10u64)),
            ),
            (
                "ballot zero",
                pack(&(1u64, 0u64, 0u64, "a", &[1u8; 16][..], 10u64)),
            ),
            (
                "token of the wrong width",
                pack(&(1u64, 1u64, 0u64, "a", &[1u8; 8][..], 10u64)),
            ),
            (
                "occupied but leaseless",
                pack(&(1u64, 1u64, 0u64, "a", &[1u8; 16][..], 0u64)),
            ),
            (
                "named but tokenless",
                pack(&(1u64, 1u64, 0u64, "a", &[0u8; 16][..], 10u64)),
            ),
            (
                "tokened but nameless",
                pack(&(1u64, 1u64, 0u64, "", &[1u8; 16][..], 10u64)),
            ),
        ];

        for (what, bytes) in cases {
            let err = codec::decode_record(&bytes);
            assert!(
                matches!(err, Err(LeaderElectionError::CorruptRecord(_))),
                "{what} should have been rejected, got {err:?}"
            );
        }
    }

    // ------------------------------------------------------------------
    // validated types
    // ------------------------------------------------------------------

    #[test]
    fn lease_durations_are_validated_on_the_way_to_the_wire() {
        assert!(LeaseDuration::new(Duration::ZERO).is_err());
        assert!(LeaseDuration::new(Duration::MAX).is_err());
        assert_eq!(lease(10).as_nanos(), 10_000_000_000);
        assert_eq!(
            lease(3600).clamped_to(Duration::from_secs(60)),
            Duration::from_secs(60)
        );
        assert_eq!(
            lease(10).clamped_to(Duration::from_secs(60)),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn an_attempt_can_be_shared_into_a_retried_closure() {
        // `db.run` takes an `Fn` closure and may re-execute it on another
        // thread, so a campaign anchor that was not `Sync` could not be held
        // across retries at all.
        fn assert_shareable<T: Send + Sync>() {}
        assert_shareable::<ClaimAttempt>();
        assert_shareable::<LeaseObservation>();
        assert_shareable::<LeaseGrant>();
        assert_shareable::<LeaderElection>();
    }

    #[test]
    fn a_zero_token_cannot_anchor_a_claim() {
        // Zero is the vacancy sentinel; claiming under it would write a record
        // that reads back as "nobody holds this".
        assert!(ClaimAttempt::new(ClaimToken::ZERO, Duration::ZERO).is_err());
        assert!(ClaimAttempt::new(token(1), Duration::ZERO).is_ok());
    }

    #[test]
    fn a_grant_expires_a_full_lease_after_its_pre_issuance_anchor() {
        let grant = grant_for(&held(5, 0, "a", 1), Duration::from_secs(100));
        assert_eq!(grant.expires_at(), Duration::from_secs(110));
    }

    #[cfg(feature = "recipes-ranked-register")]
    #[test]
    fn every_rank_of_a_term_dominates_every_rank_of_the_previous_one() {
        let older = grant_for(&held(5, 0, "a", 1), Duration::ZERO);
        let newer = grant_for(&held(6, 0, "b", 2), Duration::ZERO);
        assert!(newer.rank(0) > older.rank(u32::MAX));
        assert!(older.rank(1) > older.rank(0));
    }
}
