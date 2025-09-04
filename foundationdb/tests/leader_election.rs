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
        recipes::leader_election::LeaderElection, tuple::Subspace, Database, FdbBindingError,
    };

    #[test]
    fn test_leader_election() {
        let _guard = unsafe { foundationdb::boot() };
        futures::executor::block_on(test_leader_election_basic_async()).expect("failed to run");
        futures::executor::block_on(test_multi_process_leadership_async()).expect("failed to run");
        futures::executor::block_on(test_heartbeat_and_eviction_async()).expect("failed to run");
        futures::executor::block_on(test_leadership_transfer_on_stale_leader_async())
            .expect("failed to run");
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
        db.run(|mut txn, _| async move {
            election_ref.initialize(&mut txn).await?;
            Ok(())
        })
        .await?;

        // Register a process
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.register_process(&mut txn, process_id).await?;
            Ok(())
        })
        .await?;

        // Try to become leader
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref.try_become_leader(&mut txn, process_id).await?;
                Ok(became_leader)
            })
            .await?;
        assert!(
            became_leader,
            "Process should become leader when it's the only one"
        );

        // Verify leadership
        let election_ref = &election;
        let is_leader = db
            .run(|mut txn, _| async move {
                let is_leader = election_ref.is_leader(&mut txn, process_id).await?;
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
        db.run(|mut txn, _| async move {
            election_ref.initialize(&mut txn).await?;
            Ok(())
        })
        .await?;

        // Register all processes
        for process_id in &process_ids {
            let election_ref = &election;
            db.run(|mut txn, _| async move {
                election_ref.register_process(&mut txn, process_id).await?;
                Ok(())
            })
            .await?;
        }

        // All processes try to become leader
        let mut leaders = Vec::new();
        for process_id in &process_ids {
            let election_ref = &election;
            let became_leader = db
                .run(|mut txn, _| async move {
                    let became_leader =
                        election_ref.try_become_leader(&mut txn, process_id).await?;
                    Ok(became_leader)
                })
                .await?;
            if became_leader {
                leaders.push(*process_id);
            }
        }

        // Only one should become leader
        assert_eq!(leaders.len(), 1, "Exactly one process should become leader");

        // Verify the leader
        let leader_id = leaders[0];
        let election_ref = &election;
        let is_leader = db
            .run(|mut txn, _| async move {
                let is_leader = election_ref.is_leader(&mut txn, leader_id).await?;
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
                    .run(|mut txn, _| async move {
                        let is_leader = election_ref.is_leader(&mut txn, process_id).await?;
                        Ok(is_leader)
                    })
                    .await?;
                assert!(!is_leader, "Non-leader process should not be leader");
            }
        }

        Ok(())
    }

    async fn test_heartbeat_and_eviction_async() -> Result<(), FdbBindingError> {
        use foundationdb::recipes::leader_election::ElectionConfig;
        use std::thread::sleep;
        use std::time::Duration;

        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_heartbeat_and_eviction_async").await?;
        let leader_id = "leader-process";
        let follower_id = "follower-process";

        // Initialize with short heartbeat timeout for testing
        let config = ElectionConfig {
            max_missed_heartbeats: 3,
            election_enabled: true,
        };

        let election_ref = &election;
        db.run(|mut txn, _| {
            let config = config.clone();
            async move {
                election_ref
                    .initialize_with_config(&mut txn, config)
                    .await?;
                Ok(())
            }
        })
        .await?;

        // Register leader processes
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.register_process(&mut txn, leader_id).await?;
            Ok(())
        })
        .await?;

        // Register follower processes
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.register_process(&mut txn, follower_id).await?;
            Ok(())
        })
        .await?;

        // Leader becomes leader
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref.try_become_leader(&mut txn, leader_id).await?;
                Ok(became_leader)
            })
            .await?;
        dbg!(became_leader);
        assert!(became_leader, "First process should become leader");

        // Send heartbeats for leader but not follower
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.heartbeat(&mut txn, leader_id).await?;
            Ok(())
        })
        .await?;

        // Wait for follower to become stale
        sleep(Duration::from_secs(6));

        // Send fresh heartbeat for leader before trying to become leader again
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.heartbeat(&mut txn, leader_id).await?;
            Ok(())
        })
        .await?;

        // Leader tries to become leader again (should trigger eviction)
        let election_ref = &election;
        let still_leader = db
            .run(|mut txn, _| async move {
                let still_leader = election_ref.try_become_leader(&mut txn, leader_id).await?;
                Ok(still_leader)
            })
            .await?;
        assert!(still_leader, "Active leader should remain leader");

        // Follower should not be leader since it's stale
        let election_ref = &election;
        let follower_is_leader = db
            .run(|mut txn, _| async move {
                let follower_is_leader = election_ref.is_leader(&mut txn, follower_id).await?;
                Ok(follower_is_leader)
            })
            .await?;
        assert!(!follower_is_leader, "Stale follower should not be leader");

        Ok(())
    }

    async fn test_leadership_transfer_on_stale_leader_async() -> Result<(), FdbBindingError> {
        use foundationdb::recipes::leader_election::ElectionConfig;
        use std::thread::sleep;
        use std::time::Duration;

        let db = crate::common::database().await?;
        let election = setup_test(&db, "test_leadership_transfer_on_stale_leader_async").await?;
        let initial_leader = "initial-leader";
        let new_leader = "new-leader";

        // Initialize with short heartbeat timeout for testing
        let config = ElectionConfig {
            max_missed_heartbeats: 3,
            election_enabled: true,
        };

        let election_ref = &election;
        db.run(|mut txn, _| {
            let config = config.clone();
            async move {
                election_ref
                    .initialize_with_config(&mut txn, config)
                    .await?;
                Ok(())
            }
        })
        .await?;

        // Register both processes
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref
                .register_process(&mut txn, initial_leader)
                .await?;
            Ok(())
        })
        .await?;

        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.register_process(&mut txn, new_leader).await?;
            Ok(())
        })
        .await?;

        // Initial leader becomes leader
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref
                    .try_become_leader(&mut txn, initial_leader)
                    .await?;
                Ok(became_leader)
            })
            .await?;
        assert!(became_leader, "Initial process should become leader");

        // Verify initial leadership
        let election_ref = &election;
        let is_leader = db
            .run(|mut txn, _| async move {
                let is_leader = election_ref.is_leader(&mut txn, initial_leader).await?;
                Ok(is_leader)
            })
            .await?;
        assert!(is_leader, "Initial process should be confirmed as leader");

        // New leader tries to become leader (should fail while initial leader is active)
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref.try_become_leader(&mut txn, new_leader).await?;
                Ok(became_leader)
            })
            .await?;
        assert!(
            !became_leader,
            "New process should not become leader while initial leader is active"
        );

        // Send heartbeat for new_leader to keep it fresh
        let election_ref = &election;
        db.run(|mut txn, _| async move {
            election_ref.heartbeat(&mut txn, new_leader).await?;
            Ok(())
        })
        .await?;

        // Generate heartbeats from new_leader to advance version counter and make initial leader stale
        for _ in 0..50 {
            let election_ref = &election;
            db.run(|mut txn, _| async move {
                election_ref.heartbeat(&mut txn, new_leader).await?;
                Ok(())
            })
            .await?;
            sleep(Duration::from_millis(100));
        }

        // New leader tries to become leader again after initial leader is stale
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref.try_become_leader(&mut txn, new_leader).await?;
                Ok(became_leader)
            })
            .await?;
        assert!(
            became_leader,
            "New process should become leader after initial leader becomes stale"
        );

        // Verify new leadership
        let election_ref = &election;
        let is_leader = db
            .run(|mut txn, _| async move {
                let is_leader = election_ref.is_leader(&mut txn, new_leader).await?;
                Ok(is_leader)
            })
            .await?;
        assert!(is_leader, "New process should be confirmed as leader");

        // Initial leader should no longer be leader
        let election_ref = &election;
        let is_leader = db
            .run(|mut txn, _| async move {
                let is_leader = election_ref.is_leader(&mut txn, initial_leader).await?;
                Ok(is_leader)
            })
            .await?;
        assert!(
            !is_leader,
            "Initial stale process should no longer be leader"
        );

        // Initial leader tries to become leader again (should fail as it's stale)
        let election_ref = &election;
        let became_leader = db
            .run(|mut txn, _| async move {
                let became_leader = election_ref
                    .try_become_leader(&mut txn, initial_leader)
                    .await?;
                Ok(became_leader)
            })
            .await?;
        assert!(
            !became_leader,
            "Stale initial process should not become leader again"
        );

        Ok(())
    }
}
