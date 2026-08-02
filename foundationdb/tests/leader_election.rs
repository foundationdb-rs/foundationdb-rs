// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

mod common;

#[cfg(feature = "recipes-leader-election")]
mod leader_election_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use foundationdb::{
        Database, FdbBindingError,
        options::TransactionOption,
        recipes::{
            leader_election::{LeaderElection, Observation, ParticipantId, PollResult},
            ranked_register::{RankedRegister, WriteResult},
        },
        tuple::{Subspace, pack},
    };
    use tokio::sync::Barrier;

    fn participant(value: &str) -> ParticipantId {
        ParticipantId::new(value).expect("test participant ID is valid")
    }

    async fn setup_test(
        db: &Database,
        test_name: &str,
        suspicion_duration: Duration,
    ) -> Result<LeaderElection, FdbBindingError> {
        let subspace = Subspace::all().subspace(&(test_name,));
        let (from, to) = subspace.range();
        let from_ref = &from;
        let to_ref = &to;
        db.run(|txn, _| async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            txn.clear_range(from_ref, to_ref);
            Ok::<_, FdbBindingError>(())
        })
        .await?;
        Ok(LeaderElection::new(subspace, suspicion_duration))
    }

    async fn poll(
        db: &Database,
        election: &LeaderElection,
        participant: &ParticipantId,
        observation: &Observation,
        now: Duration,
    ) -> Result<PollResult, FdbBindingError> {
        let election = election.clone();
        let participant = participant.clone();
        let observation = observation.clone();
        db.run(|txn, _| {
            let election = election.clone();
            let participant = participant.clone();
            let observation = observation.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(
                    election.poll(&txn, &participant, &observation, now).await?,
                )
            }
        })
        .await
    }

    async fn service_poll(
        db: &Database,
        election: &LeaderElection,
        register: &RankedRegister,
        participant: &ParticipantId,
        observation: &Observation,
        now: Duration,
        value: &[u8],
    ) -> Result<(PollResult, Option<WriteResult>), FdbBindingError> {
        let election = election.clone();
        let register = register.clone();
        let participant = participant.clone();
        let observation = observation.clone();
        let value = value.to_vec();
        db.run(|txn, _| {
            let election = election.clone();
            let register = register.clone();
            let participant = participant.clone();
            let observation = observation.clone();
            let value = value.clone();
            async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                let result = election.poll(&txn, &participant, &observation, now).await?;
                let write = if result.outcome().is_leader() {
                    let rank = result.outcome().rank();
                    register
                        .read(&txn, rank)
                        .await
                        .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))?;
                    Some(
                        register
                            .write(&txn, rank, &value)
                            .await
                            .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))?,
                    )
                } else {
                    None
                };
                Ok::<_, FdbBindingError>((result, write))
            }
        })
        .await
    }

    #[tokio::test]
    async fn first_poll_acquires_immediately() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_first_poll", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");

        let election_ref = &election;
        let initial_state = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await?;
        assert_eq!(initial_state.owner(), None);
        assert_eq!(initial_state.rank().as_u64(), 0);

        let result = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        assert!(result.outcome().is_leader());
        assert!(!result.outcome().is_takeover());
        assert_eq!(result.outcome().rank().as_u64(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn incumbent_renewal_receives_fresh_rank() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_renewal", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        let renewed = poll(
            &db,
            &election,
            &alice,
            first.next_observation(),
            Duration::from_secs(1),
        )
        .await?;
        assert!(renewed.outcome().is_leader());
        assert_eq!(renewed.outcome().rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn overdue_incumbent_renewal_is_not_a_takeover() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_overdue_renewal", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        let renewed = poll(
            &db,
            &election,
            &alice,
            first.next_observation(),
            Duration::from_secs(6),
        )
        .await?;
        assert!(renewed.outcome().is_leader());
        assert!(!renewed.outcome().is_takeover());
        assert_eq!(renewed.outcome().rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn follower_observes_then_takes_over_after_local_suspicion() -> Result<(), FdbBindingError>
    {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_takeover", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        let observed = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        assert!(!observed.outcome().is_leader());

        let still_follower = poll(
            &db,
            &election,
            &bob,
            observed.next_observation(),
            Duration::from_secs(4),
        )
        .await?;
        assert!(!still_follower.outcome().is_leader());

        let takeover = poll(
            &db,
            &election,
            &bob,
            still_follower.next_observation(),
            Duration::from_secs(9),
        )
        .await?;
        assert!(takeover.outcome().is_leader());
        assert!(takeover.outcome().is_takeover());
        assert_eq!(takeover.outcome().rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn changed_generation_resets_follower_observation() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_observation_reset", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        poll(
            &db,
            &election,
            &alice,
            first.next_observation(),
            Duration::from_secs(1),
        )
        .await?;
        let reset = poll(
            &db,
            &election,
            &bob,
            observed.next_observation(),
            Duration::from_secs(10),
        )
        .await?;
        assert!(!reset.outcome().is_leader());
        assert_eq!(reset.outcome().rank().as_u64(), 2);

        let takeover = poll(
            &db,
            &election,
            &bob,
            reset.next_observation(),
            Duration::from_secs(15),
        )
        .await?;
        assert!(takeover.outcome().is_leader());
        assert_eq!(takeover.outcome().rank().as_u64(), 3);

        let owner_changed = poll(
            &db,
            &election,
            &alice,
            first.next_observation(),
            Duration::from_secs(16),
        )
        .await?;
        assert!(!owner_changed.outcome().is_leader());
        assert_eq!(owner_changed.outcome().rank().as_u64(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_first_claims_linearize_to_one_leader() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_concurrent_claim", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let observation = Observation::initial(Duration::ZERO);

        let (left, right) = tokio::try_join!(
            poll(&db, &election, &alice, &observation, Duration::ZERO),
            poll(&db, &election, &bob, &observation, Duration::ZERO),
        )?;
        assert_eq!(
            usize::from(left.outcome().is_leader()) + usize::from(right.outcome().is_leader()),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn resignation_preserves_generation_for_reacquisition() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_resign_generation", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        let rank = first.outcome().rank();

        let election_ref = &election;
        let alice_ref = &alice;
        let resigned = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.resign(&txn, alice_ref, rank).await?)
            })
            .await?;
        assert!(resigned.is_resigned());

        let election_ref = &election;
        let resigned_state = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await?;
        assert_eq!(resigned_state.owner(), None);
        assert_eq!(resigned_state.rank().as_u64(), 1);

        let reacquired = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::from_secs(1)),
            Duration::from_secs(1),
        )
        .await?;
        assert_eq!(reacquired.outcome().rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn stale_resignation_cannot_clear_newer_owner() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_stale_resign", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        poll(
            &db,
            &election,
            &bob,
            observed.next_observation(),
            Duration::from_secs(5),
        )
        .await?;

        let stale_rank = first.outcome().rank();
        let election_ref = &election;
        let alice_ref = &alice;
        let rejected = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.resign(&txn, alice_ref, stale_rank).await?)
            })
            .await?;
        assert!(!rejected.is_resigned());

        let election_ref = &election;
        let current = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await?;
        assert_eq!(current.owner(), Some(&bob));
        assert_eq!(current.rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn retried_stale_resignation_cannot_clear_newer_owner() -> Result<(), FdbBindingError> {
        let db = Arc::new(crate::common::database().await?);
        let election =
            setup_test(&db, "leader_retried_stale_resign", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let first = poll(
            &db,
            &election,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;
        let observed = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        let staged = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let attempts = Arc::new(AtomicUsize::new(0));
        let alice_db = db.clone();
        let alice_election = election.clone();
        let alice_id = alice.clone();
        let alice_staged = Arc::clone(&staged);
        let alice_release = Arc::clone(&release);
        let alice_attempts = Arc::clone(&attempts);
        let rank = first.outcome().rank();
        let alice_resignation = tokio::spawn(async move {
            alice_db
                .run(|txn, _| {
                    let election = alice_election.clone();
                    let participant = alice_id.clone();
                    let staged = Arc::clone(&alice_staged);
                    let release = Arc::clone(&alice_release);
                    let first_attempt = alice_attempts.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        txn.set_option(TransactionOption::AutomaticIdempotency)?;
                        let outcome = election.resign(&txn, &participant, rank).await?;
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
            observed.next_observation(),
            Duration::from_secs(5),
        )
        .await?;
        assert!(takeover.outcome().is_leader());
        assert_eq!(takeover.outcome().rank().as_u64(), 2);
        release.wait().await;

        let resignation = alice_resignation
            .await
            .expect("resignation task must not panic")?;
        assert!(!resignation.is_resigned());
        assert!(attempts.load(Ordering::SeqCst) >= 2);

        let election_ref = &election;
        let state = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await?;
        assert_eq!(state.owner(), Some(&bob));
        assert_eq!(state.rank().as_u64(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn stale_ranked_register_write_is_rejected_after_takeover() -> Result<(), FdbBindingError>
    {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_ranked_register", Duration::from_secs(5)).await?;
        let register_subspace = Subspace::all().subspace(&("leader_ranked_register_state",));
        let (from, to) = register_subspace.range();
        let from_ref = &from;
        let to_ref = &to;
        db.run(|txn, _| async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            txn.clear_range(from_ref, to_ref);
            Ok::<_, FdbBindingError>(())
        })
        .await?;
        let register = RankedRegister::new(register_subspace);
        let alice = participant("alice-incarnation-1");
        let bob = participant("bob-incarnation-1");
        let (first, first_write) = service_poll(
            &db,
            &election,
            &register,
            &alice,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
            b"alice",
        )
        .await?;
        assert_eq!(first_write, Some(WriteResult::Committed));
        let observed = poll(
            &db,
            &election,
            &bob,
            &Observation::initial(Duration::ZERO),
            Duration::ZERO,
        )
        .await?;

        let (takeover, takeover_write) = service_poll(
            &db,
            &election,
            &register,
            &bob,
            observed.next_observation(),
            Duration::from_secs(5),
            b"bob",
        )
        .await?;
        assert!(takeover.outcome().is_leader());
        assert_eq!(takeover_write, Some(WriteResult::Committed));
        assert!(takeover.outcome().rank() > first.outcome().rank());

        let stale_rank = first.outcome().rank();
        let register_ref = &register;
        let stale_write = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                register_ref
                    .write(&txn, stale_rank, b"stale")
                    .await
                    .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))
            })
            .await?;
        assert_eq!(stale_write, WriteResult::Aborted);

        let final_rank = takeover.outcome().rank();
        let register_ref = &register;
        let final_state = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                let read = register_ref
                    .read(&txn, final_rank)
                    .await
                    .map_err(|error| FdbBindingError::new_custom_error(Box::new(error)))?;
                Ok::<_, FdbBindingError>((read.write_rank(), read.into_value()))
            })
            .await?;
        assert_eq!(final_state.0, final_rank);
        assert_eq!(final_state.1.as_deref(), Some(b"bob".as_slice()));
        Ok(())
    }

    #[tokio::test]
    async fn poll_does_not_mutate_caller_observation_and_rejects_empty_ids()
    -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "leader_immutable_input", Duration::from_secs(5)).await?;
        let alice = participant("alice-incarnation-1");
        let input = Observation::initial(Duration::ZERO);
        let original = input.clone();
        poll(&db, &election, &alice, &input, Duration::ZERO).await?;
        assert_eq!(input, original);
        assert!(ParticipantId::new("").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_durable_state_is_reported_as_an_error() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let test_name = "leader_malformed_state";
        let election = setup_test(&db, test_name, Duration::from_secs(5)).await?;
        let state_key = Subspace::all().subspace(&(test_name,)).pack(&("state",));
        let state_key_ref = &state_key;
        db.run(|txn, _| async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            txn.set(state_key_ref, b"not a tuple");
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        let result: Result<_, FdbBindingError> = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn zero_generation_owner_is_rejected() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let test_name = "leader_zero_generation_owner";
        let election = setup_test(&db, test_name, Duration::from_secs(5)).await?;
        let state_key = Subspace::all().subspace(&(test_name,)).pack(&("state",));
        let malformed_state = pack(&(0_u64, true, "invalid-owner"));
        let state_key_ref = &state_key;
        let malformed_state_ref = &malformed_state;
        db.run(|txn, _| async move {
            txn.set_option(TransactionOption::AutomaticIdempotency)?;
            txn.set(state_key_ref, malformed_state_ref);
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        let result: Result<_, FdbBindingError> = db
            .run(|txn, _| async move {
                txn.set_option(TransactionOption::AutomaticIdempotency)?;
                Ok::<_, FdbBindingError>(election_ref.state(&txn).await?)
            })
            .await;
        assert!(result.is_err());
        Ok(())
    }
}
