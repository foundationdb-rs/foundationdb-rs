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
        recipes::leader_election::{ElectionConfig, LeaderElection},
        tuple::Subspace,
        Database, FdbBindingError,
    };
    use std::time::Duration;

    #[test]
    fn test_leader_election() {
        let _guard = unsafe { foundationdb::boot() };
        futures::executor::block_on(test_leader_election_basic_async()).expect("failed to run");
        futures::executor::block_on(test_multi_process_leadership_async()).expect("failed to run");
        futures::executor::block_on(test_heartbeat_and_lease_async()).expect("failed to run");
        futures::executor::block_on(test_leadership_transfer_on_expired_lease_async())
            .expect("failed to run");
        futures::executor::block_on(test_config_change_async()).expect("failed to run");
        futures::executor::block_on(test_resign_leadership_async()).expect("failed to run");
        futures::executor::block_on(test_preemption_async()).expect("failed to run");
    }

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
            Ok(())
        })
        .await?;

        let election = LeaderElection::new(subspace);
        Ok(election)
    }

    async fn test_leader_election_basic_async() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_leader_election_basic_async").await?;
        let process_id = "test-process-1";

        // Initialize election system
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok(())
        })
        .await?;

        // Register as a candidate
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, process_id, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Read candidate info to get versionstamp
        let election_ref = &election;
        let candidate = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, process_id).await?;
                Ok(candidate)
            })
            .await?;

        let versionstamp = candidate.expect("candidate should exist").versionstamp;

        // Try to claim leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, process_id, 0, versionstamp, current_time())
                    .await?;
                Ok(result.is_some())
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
                Ok(is_leader)
            })
            .await?;
        assert!(is_leader, "Process should be the leader");

        Ok(())
    }

    async fn test_multi_process_leadership_async() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_multi_process_leadership_async").await?;
        let process_ids = ["process-1", "process-2", "process-3"];

        // Initialize election system
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok(())
        })
        .await?;

        // Register all processes as candidates
        let mut versionstamps = Vec::new();
        for process_id in &process_ids {
            let election_ref = &election;
            db.run(|txn, _| async move {
                election_ref
                    .register_candidate(&txn, process_id, 0, current_time())
                    .await?;
                Ok(())
            })
            .await?;

            // Get versionstamp
            let election_ref = &election;
            let candidate = db
                .run(|txn, _| async move {
                    let candidate = election_ref.get_candidate(&txn, process_id).await?;
                    Ok(candidate)
                })
                .await?;
            versionstamps.push(candidate.expect("candidate should exist").versionstamp);
        }

        // All processes try to claim leadership
        let mut leaders = Vec::new();
        for (i, process_id) in process_ids.iter().enumerate() {
            let election_ref = &election;
            let vs = versionstamps[i];
            let became_leader = db
                .run(|txn, _| async move {
                    let result = election_ref
                        .try_claim_leadership(&txn, process_id, 0, vs, current_time())
                        .await?;
                    Ok(result.is_some())
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
                Ok(is_leader)
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
                        Ok(is_leader)
                    })
                    .await?;
                assert!(!is_leader, "Non-leader process should not be leader");
            }
        }

        Ok(())
    }

    async fn test_heartbeat_and_lease_async() -> Result<(), FdbBindingError> {
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
                Ok(())
            }
        })
        .await?;

        // Register both candidates
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, leader_id, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, follower_id, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Get leader's versionstamp
        let election_ref = &election;
        let leader_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, leader_id).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        // Leader claims leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, leader_id, 0, leader_vs, current_time())
                    .await?;
                Ok(result.is_some())
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
                Ok(result.is_some())
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
                Ok(result.is_some())
            })
            .await?;
        assert!(still_leader, "Leader should still be able to refresh");

        Ok(())
    }

    async fn test_leadership_transfer_on_expired_lease_async() -> Result<(), FdbBindingError> {
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
                Ok(())
            }
        })
        .await?;

        // Register both processes
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, initial_leader, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, new_leader, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Get versionstamps
        let election_ref = &election;
        let initial_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, initial_leader).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        let election_ref = &election;
        let new_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, new_leader).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        // Initial leader claims leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, initial_leader, 0, initial_vs, current_time())
                    .await?;
                Ok(result.is_some())
            })
            .await?;
        assert!(became_leader, "Initial process should become leader");

        // New leader tries to claim (should fail - lease is valid)
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, new_leader, 0, new_vs, current_time())
                    .await?;
                Ok(result.is_some())
            })
            .await?;
        assert!(
            !became_leader,
            "New process should not become leader while initial lease is valid"
        );

        // Wait for lease to expire
        sleep(Duration::from_secs(4));

        // Keep new_leader alive via heartbeat
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .heartbeat_candidate(&txn, new_leader, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // New leader tries to claim again (should succeed - lease expired)
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, new_leader, 0, new_vs, current_time())
                    .await?;
                Ok(result.is_some())
            })
            .await?;
        assert!(
            became_leader,
            "New process should become leader after initial lease expires"
        );

        // Verify new leadership
        let election_ref = &election;
        let is_leader = db
            .run(|txn, _| async move {
                let is_leader = election_ref
                    .is_leader(&txn, new_leader, current_time())
                    .await?;
                Ok(is_leader)
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
                Ok(is_leader)
            })
            .await?;
        assert!(!is_leader, "Initial process should no longer be leader");

        Ok(())
    }

    async fn test_config_change_async() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_config_change_async").await?;

        // Initialize with default config
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok(())
        })
        .await?;

        // Change config to disable elections
        let mut new_config = ElectionConfig::default();
        new_config.election_enabled = false;

        let election_ref = &election;
        db.run(|txn, _| {
            let config = new_config.clone();
            async move {
                election_ref.write_config(&txn, &config).await?;
                Ok(())
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
                    Err(_) => Ok(true), // Registration failed as expected
                    Ok(_) => Ok(false), // Registration succeeded unexpectedly
                }
            })
            .await?;

        assert!(
            registration_failed,
            "Registration should fail when elections are disabled"
        );

        // Re-enable elections
        let mut enabled_config = ElectionConfig::default();
        enabled_config.election_enabled = true;

        let election_ref = &election;
        db.run(|txn, _| {
            let config = enabled_config.clone();
            async move {
                election_ref.write_config(&txn, &config).await?;
                Ok(())
            }
        })
        .await?;

        // Now registration should work
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, "test-process", 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        Ok(())
    }

    async fn test_resign_leadership_async() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_resign_leadership_async").await?;
        let leader_id = "leader-process";
        let follower_id = "follower-process";

        // Initialize
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref.initialize(&txn).await?;
            Ok(())
        })
        .await?;

        // Register both
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, leader_id, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, follower_id, 0, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Get versionstamps
        let election_ref = &election;
        let leader_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, leader_id).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        let election_ref = &election;
        let follower_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, follower_id).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        // Leader claims leadership
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .try_claim_leadership(&txn, leader_id, 0, leader_vs, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Leader resigns
        let election_ref = &election;
        let resigned = db
            .run(|txn, _| async move {
                let resigned = election_ref.resign_leadership(&txn, leader_id).await?;
                Ok(resigned)
            })
            .await?;
        assert!(resigned, "Leader should be able to resign");

        // Follower can now claim leadership
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, follower_id, 0, follower_vs, current_time())
                    .await?;
                Ok(result.is_some())
            })
            .await?;
        assert!(
            became_leader,
            "Follower should become leader after resignation"
        );

        Ok(())
    }

    async fn test_preemption_async() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_preemption_async").await?;
        let low_priority = "low-priority";
        let high_priority = "high-priority";

        // Initialize with preemption enabled
        let mut config = ElectionConfig::default();
        config.allow_preemption = true;

        let election_ref = &election;
        db.run(|txn, _| {
            let config = config.clone();
            async move {
                election_ref.initialize_with_config(&txn, config).await?;
                Ok(())
            }
        })
        .await?;

        // Register both with different priorities
        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, low_priority, 1, current_time())
                .await?;
            Ok(())
        })
        .await?;

        let election_ref = &election;
        db.run(|txn, _| async move {
            election_ref
                .register_candidate(&txn, high_priority, 10, current_time())
                .await?;
            Ok(())
        })
        .await?;

        // Get versionstamps
        let election_ref = &election;
        let low_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, low_priority).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        let election_ref = &election;
        let high_vs = db
            .run(|txn, _| async move {
                let candidate = election_ref.get_candidate(&txn, high_priority).await?;
                Ok(candidate.expect("candidate should exist").versionstamp)
            })
            .await?;

        // Low priority claims leadership first
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, low_priority, 1, low_vs, current_time())
                    .await?;
                Ok(result.is_some())
            })
            .await?;
        assert!(became_leader, "Low priority should become leader initially");

        // High priority preempts
        let election_ref = &election;
        let became_leader = db
            .run(|txn, _| async move {
                let result = election_ref
                    .try_claim_leadership(&txn, high_priority, 10, high_vs, current_time())
                    .await?;
                Ok(result.is_some())
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
                Ok(is_leader)
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
                Ok(is_leader)
            })
            .await?;
        assert!(!is_leader, "Low priority should not be the leader anymore");

        Ok(())
    }
}
