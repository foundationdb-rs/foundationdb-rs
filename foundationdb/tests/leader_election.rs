// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

mod common;

#[cfg(feature = "recipes-leader-election")]
mod leader_election_tests {
    use foundationdb::{
        Database, FdbBindingError,
        recipes::leader_election::{ElectionConfig, LeaderElection, LeaseObservation},
        tuple::Subspace,
    };
    use std::time::Duration;

    fn current_time() -> Duration {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
    }

    async fn setup_test(db: &Database, test_name: &str) -> Result<LeaderElection, FdbBindingError> {
        let subspace = Subspace::all().subspace(&test_name);
        let (from, to) = subspace.range();

        // Clear test data
        let from_ref = &from;
        let to_ref = &to;
        db.run(|txn, _| async move {
            txn.clear_range(from_ref, to_ref);
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election = LeaderElection::new(subspace);
        Ok(election)
    }

    #[tokio::test]
    async fn test_leader_election_basic() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_leader_election_basic_async").await?;
        let process_id = "test-process-1";

        // Initialize election system
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Register as a candidate
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, process_id, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Try to claim leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        process_id,
                        0,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(
            became_leader,
            "Process should become leader when it's the only one"
        );

        // Verify leadership
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, process_id, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(is_leader, "Process should be the leader");

        Ok(())
    }

    #[tokio::test]
    async fn test_multi_process_leadership() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_multi_process_leadership_async").await?;
        let process_ids = ["process-1", "process-2", "process-3"];

        // Initialize election system
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Register all processes as candidates
        for process_id in &process_ids {
            let election_ref = &election;
            db.run(|txn, _| async move {
                election_ref
                    .register_candidate(&txn, process_id, 0, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(())
            })
            .await?;
        }

        // All processes try to claim leadership
        let mut leaders = Vec::new();
        for process_id in process_ids.iter() {
            let election_ref = &election;
            let became_leader = db
                .run(|txn, _| async move {
                    let (result, _) = election_ref
                        .try_claim_leadership(
                            &txn,
                            process_id,
                            0,
                            current_time(),
                            LeaseObservation::default(),
                        )
                        .await?;
                    Ok::<_, FdbBindingError>(result.is_some())
                })
                .await?;
            if became_leader {
                leaders.push(*process_id);
            }
        }

        // Only one should become leader (the first one to claim)
        assert_eq!(leaders.len(), 1, "Exactly one process should become leader");

        // Verify the leader
        let leader_id = leaders[0];
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, leader_id, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(
            is_leader,
            "The elected process should be confirmed as leader"
        );

        // Other processes should not be leaders
        for process_id in &process_ids {
            if *process_id != leader_id {
                let election_ref = &election;
                let is_leader = db
                    .run(|txn, _| async move {
                        let is_leader = election_ref
                            .is_leader(&txn, process_id, current_time())
                            .await?;
                        Ok::<_, FdbBindingError>(is_leader)
                    })
                    .await?;
                assert!(!is_leader, "Non-leader process should not be leader");
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_heartbeat_and_lease() -> Result<(), FdbBindingError> {
        use std::thread::sleep;

        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_heartbeat_and_lease_async").await?;
        let leader_id = "leader-process";
        let follower_id = "follower-process";

        // Initialize with short lease for testing
        let config = ElectionConfig::with_lease_duration(Duration::from_secs(5));

        let election_ref = &election;
        db.run(|txn, _| {
            let config = config.clone();
            async move {
                election_ref.initialize_with_config(&txn, config).await?;
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;

        // Register both candidates
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, leader_id, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, follower_id, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Leader claims leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        leader_id,
                        0,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(became_leader, "First process should become leader");

        // Leader refreshes lease
        let election_ref = &election;
        let refreshed = db
            .run(|txn, _| async move {
                let result = election_ref
                    .refresh_lease(&txn, leader_id, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(refreshed, "Leader should be able to refresh lease");

        // Wait a bit (not long enough for lease to expire)
        sleep(Duration::from_secs(2));

        // Leader refreshes again
        let election_ref = &election;
        let still_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .refresh_lease(&txn, leader_id, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(still_leader, "Leader should still be able to refresh");

        Ok(())
    }

    #[tokio::test]
    async fn test_leadership_transfer_on_expired_lease() -> Result<(), FdbBindingError> {
        use std::thread::sleep;

        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_leadership_transfer_on_expired_lease_async").await?;
        let initial_leader = "initial-leader";
        let new_leader = "new-leader";

        // Initialize with short lease for testing
        let config = ElectionConfig::with_lease_duration(Duration::from_secs(3));

        let election_ref = &election;
        db.run(|txn, _| {
            let config = config.clone();
            async move {
                election_ref.initialize_with_config(&txn, config).await?;
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;

        // Register both processes
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, initial_leader, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, new_leader, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Initial leader claims leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        initial_leader,
                        0,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(became_leader, "Initial process should become leader");

        // new_leader owns an observation that it threads across claim attempts.
        // Stealing an expired lease now requires observing the same leader
        // record for a full lease_duration, so the first attempt only starts
        // the timer.
        let mut obs = LeaseObservation::default();

        // New leader tries to claim (should fail - lease is valid, and this is
        // the first observation of the incumbent record anyway)
        let election_ref = &election;
        let (became_leader, next_obs) = db
            .run(|txn, _| {
                let obs = obs.clone();
                async move {
                    let (result, next_obs) = election_ref
                        .try_claim_leadership(&txn, new_leader, 0, current_time(), obs)
                        .await?;
                    Ok::<_, FdbBindingError>((result.is_some(), next_obs))
                }
            })
            .await?;
        obs = next_obs;
        assert!(
            !became_leader,
            "New process should not become leader on first observation of the incumbent"
        );

        // Wait longer than the lease so the observation window is satisfied
        sleep(Duration::from_secs(4));

        // Keep new_leader alive via heartbeat
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .heartbeat_candidate(&txn, new_leader, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // New leader tries to claim again with the same observation (should
        // succeed - it has now observed the unrefreshed incumbent for >= lease)
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| {
                let obs = obs.clone();
                async move {
                    let (result, _) = election_ref
                        .try_claim_leadership(&txn, new_leader, 0, current_time(), obs)
                        .await?;
                    Ok::<_, FdbBindingError>(result.is_some())
                }
            })
            .await?;
        assert!(
            became_leader,
            "New process should become leader after observing the stale lease for a full lease_duration"
        );

        // Verify new leadership
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, new_leader, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(is_leader, "New process should be confirmed as leader");

        // Initial leader should no longer be leader
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, initial_leader, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(!is_leader, "Initial process should no longer be leader");

        Ok(())
    }

    #[tokio::test]
    async fn test_config_change() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_config_change_async").await?;

        // Initialize with default config
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Change config to disable elections
        let new_config = ElectionConfig {
            election_enabled: false,
            ..Default::default()
        };

        let election_ref = &election;
        db.run(|txn, _| {
            let config = new_config.clone();
            async move {
                election_ref.write_config(&txn, &config).await?;
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;

        // Try to register - should fail due to disabled elections
        let election_ref = &election;
        let registration_failed = db
            .run(|txn, _| async move {
                match election_ref
                    .register_candidate(&txn, "test-process", 0, current_time())
                    .await
                {
                    Err(_) => Ok::<_, FdbBindingError>(true), // Registration failed as expected
                    Ok(_) => Ok(false),                       // Registration succeeded unexpectedly
                }
            })
            .await?;

        assert!(
            registration_failed,
            "Registration should fail when elections are disabled"
        );

        // Re-enable elections
        let enabled_config = ElectionConfig {
            election_enabled: true,
            ..Default::default()
        };

        let election_ref = &election;
        db.run(|txn, _| {
            let config = enabled_config.clone();
            async move {
                election_ref.write_config(&txn, &config).await?;
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;

        // Now registration should work
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, "test-process", 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_resign_leadership() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_resign_leadership_async").await?;
        let leader_id = "leader-process";
        let follower_id = "follower-process";

        // Initialize
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Register both
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, leader_id, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, follower_id, 0, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Leader claims leadership (ballot 1)
        let election_ref = &election;
        let leader_ballot = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        leader_id,
                        0,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.map(|s| s.ballot))
            })
            .await?;
        assert_eq!(leader_ballot, Some(1), "First claim should be ballot 1");

        // Leader resigns
        let election_ref = &election;
        let resigned = db
            .run(|txn, _| async move {
                let resigned = election_ref.resign_leadership(&txn, leader_id).await?;
                Ok::<_, FdbBindingError>(resigned)
            })
            .await?;
        assert!(resigned, "Leader should be able to resign");

        // Follower can now claim leadership immediately (vacant record, no wait).
        // The ballot must continue from the resigned record, not reset.
        let election_ref = &election;
        let follower_ballot = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        follower_id,
                        0,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.map(|s| s.ballot))
            })
            .await?;
        assert_eq!(
            follower_ballot,
            Some(2),
            "Ballot must continue across resignation (2, not reset to 1)"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_preemption() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_preemption_async").await?;
        let low_priority = "low-priority";
        let high_priority = "high-priority";

        // Initialize with preemption enabled
        let config = ElectionConfig {
            allow_preemption: true,
            ..Default::default()
        };

        let election_ref = &election;
        db.run(|txn, _| {
            let config = config.clone();
            async move {
                election_ref.initialize_with_config(&txn, config).await?;
                Ok::<_, FdbBindingError>(())
            }
        })
        .await?;

        // Register both with different priorities
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, low_priority, 1, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, high_priority, 10, current_time())
                .await?;
            Ok::<_, FdbBindingError>(())
        })
        .await?;

        // Low priority claims leadership first
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        low_priority,
                        1,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(became_leader, "Low priority should become leader initially");

        // High priority preempts (bypasses the observation wait)
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let (result, _) = election_ref
                    .try_claim_leadership(
                        &txn,
                        high_priority,
                        10,
                        current_time(),
                        LeaseObservation::default(),
                    )
                    .await?;
                Ok::<_, FdbBindingError>(result.is_some())
            })
            .await?;
        assert!(
            became_leader,
            "High priority should preempt low priority leader"
        );

        // Verify high priority is now leader
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, high_priority, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(is_leader, "High priority should be the leader");

        // Low priority should not be leader
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, low_priority, current_time())
                    .await?;
                Ok::<_, FdbBindingError>(is_leader)
            })
            .await?;
        assert!(!is_leader, "Low priority should not be the leader anymore");

        Ok(())
    }
}
