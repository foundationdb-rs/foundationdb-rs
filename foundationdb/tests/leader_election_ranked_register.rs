// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://opensource.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT> or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

mod common;

#[cfg(feature = "recipes-leader-election")]
mod leader_election_tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use foundationdb::{
        Database, FdbBindingError,
        options::TransactionOption,
        recipes::{
            leader_election::{
                LeaderElection, Leadership, LocalState, NextState, ObservationTimer, ParticipantId,
                PollInput, PollOutcome, PollResult, PollTransition, ResignOutcome,
            },
            ranked_register::{RankedRegister, RankedRegisterError, WriteResult},
        },
        tuple::Subspace,
    };
    use tokio::sync::Barrier;

    fn participant(value: &str) -> ParticipantId {
        ParticipantId::new(value).expect("test participant ID is valid")
    }

    fn register_error(error: RankedRegisterError) -> FdbBindingError {
        FdbBindingError::new_custom_error(Box::new(error))
    }

    async fn setup_election(
        db: &Database,
        test_name: &str,
        lease_duration: Duration,
    ) -> Result<LeaderElection, FdbBindingError> {
        let subspace = Subspace::all().subspace(&("leader_election_ranked_register", test_name));
        let (begin, end) = subspace.range();
        db.run(|txn, _| {
            let begin = begin.clone();
            let end = end.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                txn.clear_range(&begin, &end);
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;
        Ok(LeaderElection::new(subspace, lease_duration)?)
    }

    async fn setup_register(
        db: &Database,
        test_name: &str,
    ) -> Result<RankedRegister, FdbBindingError> {
        let subspace =
            Subspace::all().subspace(&("leader_election_ranked_register_state", test_name));
        let (begin, end) = subspace.range();
        db.run(|txn, _| {
            let begin = begin.clone();
            let end = end.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                txn.clear_range(&begin, &end);
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;
        Ok(RankedRegister::new(subspace))
    }

    struct CompletedPoll {
        result: PollResult,
        next_state: LocalState,
    }

    #[derive(Clone)]
    struct TestTime(Arc<Mutex<Duration>>);

    impl TestTime {
        fn new(now: Duration) -> Self {
            Self(Arc::new(Mutex::new(now)))
        }

        fn monotonic(&self) -> Duration {
            *self
                .0
                .lock()
                .expect("test clock mutex must not be poisoned")
        }

        fn set(&self, now: Duration) {
            *self
                .0
                .lock()
                .expect("test clock mutex must not be poisoned") = now;
        }
    }

    async fn poll(
        db: &Database,
        election: &LeaderElection,
        participant: &ParticipantId,
        local_state: &LocalState,
        attempt_started_at: Duration,
        adopted_at: Duration,
    ) -> Result<CompletedPoll, FdbBindingError> {
        let election = election.clone();
        let participant = participant.clone();
        let input = local_state.input(attempt_started_at);
        db.run(|txn, _| {
            let election = election.clone();
            let participant = participant.clone();
            let input = input.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election.poll(&txn, &participant, &input).await?)
            }
        })
        .await
        .map(|result| CompletedPoll {
            next_state: local_state
                .clone()
                .adopt(attempt_started_at, adopted_at, result.next()),
            result,
        })
    }

    async fn resign(
        db: &Database,
        election: &LeaderElection,
        participant: &ParticipantId,
        token: &Leadership,
    ) -> Result<ResignOutcome, FdbBindingError> {
        let election = election.clone();
        let participant = participant.clone();
        let (rank, lease_duration) = (token.rank(), token.lease_duration());
        db.run(|txn, _| {
            let election = election.clone();
            let participant = participant.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(
                    election
                        .resign(&txn, &participant, rank, lease_duration)
                        .await?,
                )
            }
        })
        .await
    }

    async fn state(
        db: &Database,
        election: &LeaderElection,
    ) -> Result<foundationdb::recipes::leader_election::ElectionState, FdbBindingError> {
        let election = election.clone();
        db.run(|txn, _| {
            let election = election.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election.state(&txn).await?)
            }
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn service_poll(
        db: &Database,
        election: &LeaderElection,
        register: &RankedRegister,
        participant: &ParticipantId,
        local_state: &LocalState,
        attempt_started_at: Duration,
        adopted_at: Duration,
        value: &[u8],
    ) -> Result<(CompletedPoll, Option<WriteResult>), FdbBindingError> {
        let election = election.clone();
        let register = register.clone();
        let participant = participant.clone();
        let input = local_state.input(attempt_started_at);
        let value = value.to_vec();
        db.run(|txn, _| {
            let election = election.clone();
            let register = register.clone();
            let participant = participant.clone();
            let input = input.clone();
            let value = value.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                let result = election.poll(&txn, &participant, &input).await?;
                let write = if let PollOutcome::Leader { rank, .. } = result.outcome() {
                    register.read(&txn, *rank).await.map_err(register_error)?;
                    Some(
                        register
                            .write(&txn, *rank, &value)
                            .await
                            .map_err(register_error)?,
                    )
                } else {
                    None
                };
                Ok::<_, FdbBindingError>((result, write))
            }
        })
        .await
        .map(|(result, write)| {
            (
                CompletedPoll {
                    next_state: local_state.clone().adopt(
                        attempt_started_at,
                        adopted_at,
                        result.next(),
                    ),
                    result,
                },
                write,
            )
        })
    }

    fn leadership(state: &LocalState) -> Leadership {
        state
            .leadership()
            .expect("leader result carries a leadership token")
            .clone()
    }

    #[tokio::test]
    async fn zero_duration_constructor_is_rejected() {
        let subspace = Subspace::all().subspace(&("leader_election_ranked_register", "zero"));
        assert!(LeaderElection::new(subspace, Duration::ZERO).is_err());
    }

    #[tokio::test]
    async fn vacant_state_is_acquired_immediately() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "vacant", Duration::from_secs(5)).await?;
        let alice = participant("alice-vacant");

        let result = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        assert!(result.result.outcome().is_leader());
        assert_eq!(
            result.result.outcome().transition(),
            PollTransition::Acquired
        );
        assert_eq!(result.result.outcome().rank().as_u64(), 1);
        assert_eq!(
            leadership(&result.next_state).lease_duration(),
            Duration::from_secs(5)
        );

        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank().as_u64(), 1);
        assert_eq!(durable.lease_duration(), Some(Duration::from_secs(5)));
        Ok(())
    }

    #[tokio::test]
    async fn exact_unexpired_renewal_publishes_fresh_rank_and_duration()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let initial = setup_election(&db, "renewal", Duration::from_secs(5)).await?;
        let renewal = LeaderElection::new(
            Subspace::all().subspace(&("leader_election_ranked_register", "renewal")),
            Duration::from_secs(9),
        )?;
        let alice = participant("alice-renewal");
        let acquired = poll(
            &db,
            &initial,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let renewed = poll(
            &db,
            &renewal,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(
            renewed.result.outcome().transition(),
            PollTransition::Renewed
        );
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);
        assert_eq!(
            leadership(&renewed.next_state).lease_duration(),
            Duration::from_secs(9)
        );

        let durable = state(&db, &renewal).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank().as_u64(), 2);
        assert_eq!(durable.lease_duration(), Some(Duration::from_secs(9)));
        Ok(())
    }

    #[tokio::test]
    async fn first_foreign_observation_cannot_steal() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "foreign_observation", Duration::from_secs(5)).await?;
        let alice = participant("alice-foreign");
        let bob = participant("bob-foreign");
        poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let observed = poll(
            &db,
            &election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        assert!(!observed.result.outcome().is_leader());
        assert_eq!(
            observed.result.outcome().transition(),
            PollTransition::Followed
        );
        assert_eq!(
            observed.result.outcome().lease_duration(),
            Some(Duration::from_secs(5))
        );
        assert!(observed.next_state.observation().is_some());

        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank().as_u64(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn takeover_requires_exact_unchanged_observation_and_persisted_duration()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let alice_election = setup_election(&db, "takeover", Duration::from_secs(5)).await?;
        let bob_election = LeaderElection::new(
            Subspace::all().subspace(&("leader_election_ranked_register", "takeover")),
            Duration::from_secs(3),
        )?;
        let alice = participant("alice-takeover");
        let bob = participant("bob-takeover");
        poll(
            &db,
            &alice_election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &bob_election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let early = poll(
            &db,
            &bob_election,
            &bob,
            &observed.next_state,
            Duration::from_secs(4),
            Duration::from_secs(4),
        )
        .await?;
        assert!(!early.result.outcome().is_leader());

        let takeover = poll(
            &db,
            &bob_election,
            &bob,
            &early.next_state,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await?;
        assert!(takeover.result.outcome().is_leader());
        assert_eq!(
            takeover.result.outcome().transition(),
            PollTransition::TookOver
        );
        assert_eq!(takeover.result.outcome().rank().as_u64(), 2);
        assert_eq!(
            leadership(&takeover.next_state).lease_duration(),
            Duration::from_secs(3)
        );
        Ok(())
    }

    #[tokio::test]
    async fn delayed_read_does_not_age_a_new_observation() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "delayed_observation", Duration::from_secs(5)).await?;
        let alice = participant("alice-delayed-observation");
        let bob = participant("bob-delayed-observation");
        poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let observed = poll(
            &db,
            &election,
            &bob,
            &LocalState::unknown(),
            Duration::from_secs(1),
            Duration::from_secs(100),
        )
        .await?;
        assert_eq!(
            observed
                .next_state
                .observation()
                .expect("follower result carries an observation")
                .first_observed_at(),
            Duration::from_secs(100)
        );

        let early = poll(
            &db,
            &election,
            &bob,
            &observed.next_state,
            Duration::from_secs(104),
            Duration::from_secs(104),
        )
        .await?;
        assert!(!early.result.outcome().is_leader());

        let takeover = poll(
            &db,
            &election,
            &bob,
            &early.next_state,
            Duration::from_secs(105),
            Duration::from_secs(105),
        )
        .await?;
        assert_eq!(
            takeover.result.outcome().transition(),
            PollTransition::TookOver
        );
        Ok(())
    }

    #[tokio::test]
    async fn leadership_expires_from_attempt_start_not_later_adoption()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election =
            setup_election(&db, "leadership_attempt_start", Duration::from_secs(5)).await?;
        let alice = participant("alice-leadership-attempt-start");

        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::from_secs(100),
        )
        .await?;
        assert_eq!(
            leadership(&acquired.next_state).last_renewed_at(),
            Duration::ZERO
        );

        let expired = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(100),
            Duration::from_secs(100),
        )
        .await?;
        assert!(!expired.result.outcome().is_leader());
        assert_eq!(
            expired.result.outcome().transition(),
            PollTransition::Followed
        );
        assert_eq!(expired.result.outcome().owner(), Some(&alice));
        Ok(())
    }

    #[tokio::test]
    async fn changed_revision_resets_observation() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "observation_reset", Duration::from_secs(5)).await?;
        let alice = participant("alice-reset");
        let bob = participant("bob-reset");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);

        let reset = poll(
            &db,
            &election,
            &bob,
            &observed.next_state,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .await?;
        assert!(!reset.result.outcome().is_leader());
        let reset_observation = reset.next_state.observation().expect("observation reset");
        assert_eq!(reset_observation.rank().as_u64(), 2);
        assert_eq!(
            reset_observation.first_observed_at(),
            Duration::from_secs(10)
        );
        Ok(())
    }

    #[tokio::test]
    async fn shorter_local_duration_honors_incumbent_persisted_duration()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let alice_election =
            setup_election(&db, "persisted_duration", Duration::from_secs(10)).await?;
        let bob_election = LeaderElection::new(
            Subspace::all().subspace(&("leader_election_ranked_register", "persisted_duration")),
            Duration::from_secs(1),
        )?;
        let alice = participant("alice-duration");
        let bob = participant("bob-duration");
        poll(
            &db,
            &alice_election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &bob_election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let early = poll(
            &db,
            &bob_election,
            &bob,
            &observed.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert!(!early.result.outcome().is_leader());
        assert_eq!(
            early.result.outcome().lease_duration(),
            Some(Duration::from_secs(10))
        );

        let takeover = poll(
            &db,
            &bob_election,
            &bob,
            &early.next_state,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .await?;
        assert_eq!(
            takeover.result.outcome().transition(),
            PollTransition::TookOver
        );
        assert_eq!(
            leadership(&takeover.next_state).lease_duration(),
            Duration::from_secs(1)
        );
        Ok(())
    }

    #[tokio::test]
    async fn expired_same_owner_first_observes_then_reacquires() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "reacquisition", Duration::from_secs(5)).await?;
        let alice = participant("alice-reacquisition");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let expired = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await?;
        assert!(!expired.result.outcome().is_leader());
        assert_eq!(
            expired.result.outcome().transition(),
            PollTransition::Followed
        );
        assert_eq!(expired.result.outcome().owner(), Some(&alice));

        let reacquired = poll(
            &db,
            &election,
            &alice,
            &expired.next_state,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .await?;
        assert!(reacquired.result.outcome().is_leader());
        assert_eq!(
            reacquired.result.outcome().transition(),
            PollTransition::Reacquired
        );
        assert_eq!(reacquired.result.outcome().rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn stale_renewal_token_cannot_change_state() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "stale_renewal", Duration::from_secs(5)).await?;
        let alice = participant("alice-stale-renewal");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);

        let stale = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .await?;
        assert!(!stale.result.outcome().is_leader());
        assert_eq!(stale.result.outcome().rank().as_u64(), 2);
        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank().as_u64(), 2);

        let renewed_token = leadership(&renewed.next_state);
        let resigned = resign(&db, &election, &alice, &renewed_token).await?;
        assert_eq!(resigned, ResignOutcome::Resigned);

        let reacquired_from_stale = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(3),
            Duration::from_secs(3),
        )
        .await?;
        assert!(reacquired_from_stale.result.outcome().is_leader());
        assert_eq!(
            reacquired_from_stale.result.outcome().transition(),
            PollTransition::Acquired
        );
        assert_eq!(reacquired_from_stale.result.outcome().rank().as_u64(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn exact_resignation_preserves_rank_and_duration_and_stale_is_rejected()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let alice_election = setup_election(&db, "resignation", Duration::from_secs(5)).await?;
        let bob_election = LeaderElection::new(
            Subspace::all().subspace(&("leader_election_ranked_register", "resignation")),
            Duration::from_secs(3),
        )?;
        let alice = participant("alice-resignation");
        let bob = participant("bob-resignation");
        let acquired = poll(
            &db,
            &alice_election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let alice_token = leadership(&acquired.next_state);

        let resigned = resign(&db, &alice_election, &alice, &alice_token).await?;
        assert_eq!(resigned, ResignOutcome::Resigned);
        let released = state(&db, &alice_election).await?;
        assert_eq!(released.owner(), None);
        assert_eq!(released.rank().as_u64(), 1);
        assert_eq!(released.lease_duration(), Some(Duration::from_secs(5)));

        let _bob_acquired = poll(
            &db,
            &bob_election,
            &bob,
            &LocalState::unknown(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        let stale = resign(&db, &alice_election, &alice, &alice_token).await?;
        assert_eq!(stale, ResignOutcome::Rejected);
        let durable = state(&db, &bob_election).await?;
        assert_eq!(durable.owner(), Some(&bob));
        assert_eq!(durable.rank().as_u64(), 2);
        assert_eq!(durable.lease_duration(), Some(Duration::from_secs(3)));
        Ok(())
    }

    #[tokio::test]
    async fn takeover_conflicting_with_resignation_retries_safely() -> Result<(), FdbBindingError> {
        let db = Arc::new(crate::common::database().await?);
        let election = setup_election(&db, "resign_conflict", Duration::from_secs(5)).await?;
        let alice = participant("alice-resign-conflict");
        let bob = participant("bob-resign-conflict");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let alice_token = leadership(&acquired.next_state);

        let staged = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));
        let resignation_db = db.clone();
        let resignation_election = election.clone();
        let resignation_participant = alice.clone();
        let resignation_rank = alice_token.rank();
        let resignation_lease_duration = alice_token.lease_duration();
        let resignation_staged = staged.clone();
        let resignation_release = release.clone();
        let resignation_attempts = attempts.clone();
        let resignation = tokio::spawn(async move {
            resignation_db
                .run(|txn, _| {
                    let election = resignation_election.clone();
                    let participant = resignation_participant.clone();
                    let staged = resignation_staged.clone();
                    let release = resignation_release.clone();
                    let first_attempt = resignation_attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let outcome = election
                            .resign(
                                &txn,
                                &participant,
                                resignation_rank,
                                resignation_lease_duration,
                            )
                            .await?;
                        if first_attempt {
                            staged.wait().await;
                            release.wait().await;
                        }
                        Ok::<_, FdbBindingError>(outcome)
                    }
                })
                .await
        });

        staged.wait().await;
        let takeover = poll(
            &db,
            &election,
            &bob,
            &observed.next_state,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await?;
        assert_eq!(
            takeover.result.outcome().transition(),
            PollTransition::TookOver
        );
        release.wait().await;

        assert_eq!(
            resignation
                .await
                .expect("resignation task must not panic")?,
            ResignOutcome::Rejected
        );
        assert!(attempts.load(Ordering::SeqCst) >= 2);
        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&bob));
        assert_eq!(durable.rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_unknown_polls_acquire_exactly_once() -> Result<(), FdbBindingError> {
        let db = Arc::new(crate::common::database().await?);
        let election = setup_election(&db, "concurrent_unknown", Duration::from_secs(5)).await?;
        let alice = participant("alice-concurrent-unknown");
        let bob = participant("bob-concurrent-unknown");
        let staged = Arc::new(Barrier::new(3));
        let alice_attempts = Arc::new(AtomicUsize::new(0));
        let bob_attempts = Arc::new(AtomicUsize::new(0));

        let alice_poll = {
            let db = db.clone();
            let election = election.clone();
            let participant = alice.clone();
            let staged = staged.clone();
            let attempts = alice_attempts.clone();
            tokio::spawn(async move {
                db.run(|txn, _| {
                    let election = election.clone();
                    let participant = participant.clone();
                    let staged = staged.clone();
                    let first_attempt = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let result = election
                            .poll(&txn, &participant, &PollInput::Unknown)
                            .await?;
                        if first_attempt {
                            staged.wait().await;
                        }
                        Ok::<_, FdbBindingError>(result)
                    }
                })
                .await
            })
        };
        let bob_poll = {
            let db = db.clone();
            let election = election.clone();
            let participant = bob.clone();
            let staged = staged.clone();
            let attempts = bob_attempts.clone();
            tokio::spawn(async move {
                db.run(|txn, _| {
                    let election = election.clone();
                    let participant = participant.clone();
                    let staged = staged.clone();
                    let first_attempt = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let result = election
                            .poll(&txn, &participant, &PollInput::Unknown)
                            .await?;
                        if first_attempt {
                            staged.wait().await;
                        }
                        Ok::<_, FdbBindingError>(result)
                    }
                })
                .await
            })
        };

        staged.wait().await;
        let alice_result = alice_poll.await.expect("alice task must not panic")?;
        let bob_result = bob_poll.await.expect("bob task must not panic")?;
        let acquired = [(&alice, &alice_result), (&bob, &bob_result)]
            .into_iter()
            .find_map(|(participant, result)| {
                (result.outcome().transition() == PollTransition::Acquired).then_some(participant)
            })
            .expect("one concurrent unknown poll must acquire");

        assert_eq!(
            [alice_result.outcome(), bob_result.outcome()]
                .into_iter()
                .filter(|outcome| outcome.transition() == PollTransition::Acquired)
                .count(),
            1
        );
        assert_eq!(
            [alice_result.outcome(), bob_result.outcome()]
                .into_iter()
                .filter(|outcome| outcome.transition() == PollTransition::Followed)
                .count(),
            1
        );
        assert!(
            alice_attempts.load(Ordering::SeqCst) >= 2 || bob_attempts.load(Ordering::SeqCst) >= 2,
            "one conflicting poll must retry"
        );
        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(acquired));
        assert_eq!(durable.rank().as_u64(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn retried_poll_adopts_only_committed_attempt_result() -> Result<(), FdbBindingError> {
        let db = Arc::new(crate::common::database().await?);
        let election = setup_election(&db, "retry_adoption", Duration::from_secs(5)).await?;
        let alice = participant("alice-retry-adoption");
        let bob = participant("bob-retry-adoption");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let staged = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));
        let time = TestTime::new(Duration::ZERO);
        let bob_poll = {
            let db = db.clone();
            let election = election.clone();
            let participant = bob.clone();
            let staged = staged.clone();
            let release = release.clone();
            let attempts = attempts.clone();
            tokio::spawn(async move {
                db.run(|txn, _| {
                    let election = election.clone();
                    let participant = participant.clone();
                    let staged = staged.clone();
                    let release = release.clone();
                    let first_attempt = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let result = election
                            .poll(&txn, &participant, &PollInput::Unknown)
                            .await?;
                        // A read-only transaction does not conflict-check its election read at
                        // commit. This isolated marker makes the staged attempt a write
                        // transaction, so Alice's renewal deterministically forces its retry.
                        txn.set(
                            b"leader_election_ranked_register/retry_adoption_marker",
                            b"",
                        );
                        if first_attempt {
                            staged.wait().await;
                            release.wait().await;
                        }
                        Ok::<_, FdbBindingError>(result)
                    }
                })
                .await
            })
        };

        staged.wait().await;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);
        time.set(Duration::from_secs(10));
        release.wait().await;

        let result = bob_poll.await.expect("bob task must not panic")?;
        assert!(attempts.load(Ordering::SeqCst) >= 2);
        assert_eq!(result.outcome().transition(), PollTransition::Followed);
        assert_eq!(result.outcome().rank().as_u64(), 2);
        let next_state =
            LocalState::unknown().adopt(Duration::ZERO, time.monotonic(), result.next());
        let observation = next_state
            .observation()
            .expect("the committed retry must produce an observation");
        assert_eq!(observation.rank().as_u64(), 2);
        assert_eq!(observation.first_observed_at(), Duration::from_secs(10));
        Ok(())
    }

    #[tokio::test]
    async fn leader_poll_fences_ranked_register_and_stale_rank_is_rejected()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let alice_election = setup_election(&db, "ranked_register", Duration::from_secs(5)).await?;
        let bob_election = LeaderElection::new(
            Subspace::all().subspace(&("leader_election_ranked_register", "ranked_register")),
            Duration::from_secs(5),
        )?;
        let register = setup_register(&db, "ranked_register").await?;
        let alice = participant("alice-register");
        let bob = participant("bob-register");

        let (first, first_write) = service_poll(
            &db,
            &alice_election,
            &register,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
            b"alice",
        )
        .await?;
        assert_eq!(
            first.result.outcome().transition(),
            PollTransition::Acquired
        );
        assert_eq!(first_write, Some(WriteResult::Committed));
        let observed = poll(
            &db,
            &bob_election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;

        let (takeover, takeover_write) = service_poll(
            &db,
            &bob_election,
            &register,
            &bob,
            &observed.next_state,
            Duration::from_secs(5),
            Duration::from_secs(5),
            b"bob",
        )
        .await?;
        assert_eq!(
            takeover.result.outcome().transition(),
            PollTransition::TookOver
        );
        assert_eq!(takeover_write, Some(WriteResult::Committed));
        assert!(takeover.result.outcome().rank() > first.result.outcome().rank());

        let stale_rank = first.result.outcome().rank();
        let register_ref = register.clone();
        let stale_write = db
            .run(|txn, _| {
                let register = register_ref.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    register
                        .write(&txn, stale_rank, b"stale")
                        .await
                        .map_err(register_error)
                }
            })
            .await?;
        assert_eq!(stale_write, WriteResult::Aborted);

        let register_ref = register.clone();
        let current_rank = takeover.result.outcome().rank();
        let (write_rank, value) = db
            .run(|txn, _| {
                let register = register_ref.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    let read = register
                        .read(&txn, current_rank)
                        .await
                        .map_err(register_error)?;
                    Ok::<_, FdbBindingError>((read.write_rank(), read.into_value()))
                }
            })
            .await?;
        assert_eq!(write_rank, current_rank);
        assert_eq!(value.as_deref(), Some(b"bob".as_slice()));
        Ok(())
    }

    #[tokio::test]
    async fn same_participant_renewal_self_fences_old_rank_writes() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "self_fencing", Duration::from_secs(5)).await?;
        let register = setup_register(&db, "self_fencing").await?;
        let alice = participant("alice-self-fencing");

        let (acquired, acquired_write) = service_poll(
            &db,
            &election,
            &register,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
            b"rank-one",
        )
        .await?;
        assert_eq!(
            acquired.result.outcome().transition(),
            PollTransition::Acquired
        );
        assert_eq!(acquired_write, Some(WriteResult::Committed));

        let (renewed, renewed_write) = service_poll(
            &db,
            &election,
            &register,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
            b"rank-two",
        )
        .await?;
        assert_eq!(
            renewed.result.outcome().transition(),
            PollTransition::Renewed
        );
        assert_eq!(renewed_write, Some(WriteResult::Committed));
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);

        let stale_rank = acquired.result.outcome().rank();
        let register_ref = register.clone();
        let stale_write = db
            .run(|txn, _| {
                let register = register_ref.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    register
                        .write(&txn, stale_rank, b"stale")
                        .await
                        .map_err(register_error)
                }
            })
            .await?;
        assert_eq!(stale_write, WriteResult::Aborted);

        let register_ref = register.clone();
        let current_rank = renewed.result.outcome().rank();
        let (write_rank, value) = db
            .run(|txn, _| {
                let register = register_ref.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    let read = register
                        .read(&txn, current_rank)
                        .await
                        .map_err(register_error)?;
                    Ok::<_, FdbBindingError>((read.write_rank(), read.into_value()))
                }
            })
            .await?;
        assert_eq!(write_rank, current_rank);
        assert_eq!(value.as_deref(), Some(b"rank-two".as_slice()));
        Ok(())
    }

    async fn poll_input(
        db: &Database,
        election: &LeaderElection,
        participant: &ParticipantId,
        input: &PollInput,
    ) -> Result<PollResult, FdbBindingError> {
        let election = election.clone();
        let participant = participant.clone();
        let input = input.clone();
        db.run(|txn, _| {
            let election = election.clone();
            let participant = participant.clone();
            let input = input.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election.poll(&txn, &participant, &input).await?)
            }
        })
        .await
    }

    #[tokio::test]
    async fn retried_takeover_follows_the_changed_tuple_with_a_reset_timer()
    -> Result<(), FdbBindingError> {
        let db = Arc::new(crate::common::database().await?);
        let election = setup_election(&db, "retry_takeover", Duration::from_secs(5)).await?;
        let alice = participant("alice-retry-takeover");
        let bob = participant("bob-retry-takeover");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        // Expired on the first attempt, so it stages a takeover write that
        // conflicts with Alice's renewal and retries with the same input.
        let input = observed.next_state.input(Duration::from_secs(5));
        let staged = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));
        let bob_poll = {
            let db = db.clone();
            let election = election.clone();
            let participant = bob.clone();
            let staged = staged.clone();
            let release = release.clone();
            let attempts = attempts.clone();
            tokio::spawn(async move {
                db.run(|txn, _| {
                    let election = election.clone();
                    let participant = participant.clone();
                    let input = input.clone();
                    let staged = staged.clone();
                    let release = release.clone();
                    let first_attempt = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let result = election.poll(&txn, &participant, &input).await?;
                        if first_attempt {
                            assert_eq!(result.outcome().transition(), PollTransition::TookOver);
                            staged.wait().await;
                            release.wait().await;
                        }
                        Ok::<_, FdbBindingError>(result)
                    }
                })
                .await
            })
        };

        staged.wait().await;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(renewed.result.outcome().rank().as_u64(), 2);
        release.wait().await;

        let result = bob_poll.await.expect("bob task must not panic")?;
        assert!(attempts.load(Ordering::SeqCst) >= 2);
        assert!(!result.outcome().is_leader());
        assert_eq!(result.outcome().transition(), PollTransition::Followed);
        assert_eq!(result.outcome().rank().as_u64(), 2);
        assert_eq!(
            result.next(),
            &NextState::Observation {
                owner: alice.clone(),
                rank: renewed.result.outcome().rank(),
                lease_duration: Duration::from_secs(5),
                timer: ObservationTimer::Reset,
            }
        );
        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn forged_inputs_cannot_renew_take_over_or_resign() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let lease = Duration::from_secs(5);
        let election = setup_election(&db, "forged_inputs", lease).await?;
        let alice = participant("alice-forged");
        let bob = participant("bob-forged");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;
        let current = renewed.result.outcome().rank();
        let stale = acquired.result.outcome().rank();
        let longer = lease + Duration::from_nanos(1);
        let huge = Duration::from_secs(1_000_000);

        let forged = [
            // Wrong owner claiming leadership of the current record.
            (
                &bob,
                PollInput::Leadership {
                    rank: current,
                    lease_duration: lease,
                    elapsed: Duration::ZERO,
                },
            ),
            // Superseded rank.
            (
                &alice,
                PollInput::Leadership {
                    rank: stale,
                    lease_duration: lease,
                    elapsed: Duration::ZERO,
                },
            ),
            // Wrong duration.
            (
                &alice,
                PollInput::Leadership {
                    rank: current,
                    lease_duration: longer,
                    elapsed: Duration::ZERO,
                },
            ),
            // Changed tuple with a huge elapsed time.
            (
                &bob,
                PollInput::Observation {
                    owner: alice.clone(),
                    rank: stale,
                    lease_duration: lease,
                    elapsed: huge,
                },
            ),
            (
                &bob,
                PollInput::Observation {
                    owner: bob.clone(),
                    rank: current,
                    lease_duration: lease,
                    elapsed: huge,
                },
            ),
            (
                &bob,
                PollInput::Observation {
                    owner: alice.clone(),
                    rank: current,
                    lease_duration: longer,
                    elapsed: huge,
                },
            ),
        ];
        for (poller, input) in &forged {
            let result = poll_input(&db, &election, poller, input).await?;
            assert_eq!(
                result.outcome().transition(),
                PollTransition::Followed,
                "{input:?}"
            );
            assert_eq!(
                result.next(),
                &NextState::Observation {
                    owner: alice.clone(),
                    rank: current,
                    lease_duration: lease,
                    timer: ObservationTimer::Reset,
                },
                "{input:?}"
            );
        }

        let current_token = leadership(&renewed.next_state);
        assert_eq!(
            resign(&db, &election, &bob, &current_token).await?,
            ResignOutcome::Rejected
        );
        let election_ref = election.clone();
        let alice_ref = alice.clone();
        let wrong_duration = db
            .run(|txn, _| {
                let election = election_ref.clone();
                let alice = alice_ref.clone();
                async move {
                    txn.set_option(TransactionOption::AutomaticIdempotency)?;
                    Ok::<_, FdbBindingError>(election.resign(&txn, &alice, current, longer).await?)
                }
            })
            .await?;
        assert_eq!(wrong_duration, ResignOutcome::Rejected);

        let durable = state(&db, &election).await?;
        assert_eq!(durable.owner(), Some(&alice));
        assert_eq!(durable.rank(), current);
        assert_eq!(durable.lease_duration(), Some(lease));
        Ok(())
    }

    #[tokio::test]
    async fn resign_with_a_rank_superseded_by_renewal_is_rejected() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_election(&db, "superseded_resign", Duration::from_secs(5)).await?;
        let alice = participant("alice-superseded-resign");
        let acquired = poll(
            &db,
            &election,
            &alice,
            &LocalState::unknown(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await?;
        let renewed = poll(
            &db,
            &election,
            &alice,
            &acquired.next_state,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await?;

        assert_eq!(
            resign(&db, &election, &alice, &leadership(&acquired.next_state)).await?,
            ResignOutcome::Rejected
        );
        assert_eq!(state(&db, &election).await?.owner(), Some(&alice));
        assert_eq!(
            resign(&db, &election, &alice, &leadership(&renewed.next_state)).await?,
            ResignOutcome::Resigned
        );
        let released = state(&db, &election).await?;
        assert_eq!(released.owner(), None);
        assert_eq!(released.rank().as_u64(), 2);
        Ok(())
    }
}
