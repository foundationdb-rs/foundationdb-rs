// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Leader election against a live cluster
//!
//! The decision core is unit-tested in the crate itself; what these tests add
//! is everything that only a real database can show: that the compare-and-set
//! really serializes concurrent claimants, that a follower polling the record
//! sees every transition it has to, that the handle layer takes over after a
//! leader stops running, and that the fencing composition rejects a wedged
//! leader's writes.
//!
//! Timing assertions are one-sided on purpose. A test never asserts that
//! something happened *within* a short window, only that a steal could not
//! happen before its observation window closed, or that a handover happened in
//! much less than a lease.

mod common;

#[cfg(feature = "recipes-leader-election")]
mod leader_election_tests {
    use foundationdb::{
        Database,
        env::{Clock, Environment, SeededRng},
        recipes::leader_election::{
            ClaimAttempt, ClaimOutcome, ClaimToken, ElectorConfig, HistoryEventKind, LeadOutcome,
            LeaderElection, LeaderElectionError, LeaderElector, LeaderRecord, LeaseDuration,
            LeaseGrant, LeaseObservation, LeaseStatus, RefreshAttempt, RefreshOutcome,
            ResignOutcome, Result, Timer,
        },
        tuple::Subspace,
    };
    use futures::future::BoxFuture;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::time::Instant;

    // ========================================================================
    // HARNESS
    // ========================================================================

    /// A [`Clock`] over the tokio timeline, one instance per simulated process
    ///
    /// The production `TokioClock` lives behind the
    /// `recipes-leader-election-tokio` feature, which the default test build
    /// does not enable; implementing the trait here also keeps the tests honest
    /// about every process measuring time on its own epoch.
    #[derive(Debug)]
    struct TestClock {
        epoch: Instant,
    }

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                epoch: Instant::now(),
            })
        }
    }

    impl Clock for TestClock {
        fn monotonic(&self) -> Duration {
            self.epoch.elapsed()
        }

        /// Never read by the handle layer, which only ever measures durations.
        fn wall(&self) -> Duration {
            unreachable!("the handle layer must never read a wall clock")
        }
    }

    /// The waiting half, on the same timeline.
    #[derive(Debug)]
    struct TestTimer;

    impl Timer for TestTimer {
        fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
            Box::pin(tokio::time::sleep(duration))
        }
    }

    /// A cleared subspace of its own, so tests are order-independent even when
    /// the harness runs them in parallel.
    async fn setup(test_name: &str) -> Result<(Arc<Database>, Subspace)> {
        let db = Arc::new(crate::common::database().await?);
        let subspace = Subspace::all().subspace(&test_name);
        let (from, to) = subspace.range();

        let from = &from;
        let to = &to;
        db.run(|txn, _| async move {
            txn.clear_range(from, to);
            Ok::<_, LeaderElectionError>(())
        })
        .await?;

        Ok((db, subspace))
    }

    /// One process driving the transaction-level primitives
    ///
    /// It owns the two pieces of state the protocol says must survive across
    /// transactions: the observation window, and a clock that is only ever
    /// compared with itself.
    struct Contender {
        db: Arc<Database>,
        election: LeaderElection,
        id: String,
        lease: LeaseDuration,
        clock: Arc<TestClock>,
        observation: Mutex<LeaseObservation>,
    }

    impl Contender {
        fn new(db: &Arc<Database>, subspace: &Subspace, id: &str, lease: Duration) -> Result<Self> {
            Ok(Self {
                db: Arc::clone(db),
                election: LeaderElection::new(subspace.clone()),
                id: id.to_string(),
                lease: LeaseDuration::new(lease)?,
                clock: TestClock::new(),
                observation: Mutex::new(LeaseObservation::new()),
            })
        }

        /// One claim transaction with a fresh single-use attempt
        async fn claim(&self) -> Result<ClaimOutcome> {
            let attempt = ClaimAttempt::new(ClaimToken::generate(), self.clock.monotonic())?;
            self.claim_with(&attempt).await
        }

        /// One claim transaction reusing an attempt, which is what a retry after
        /// an unknown commit does.
        async fn claim_with(&self, attempt: &ClaimAttempt) -> Result<ClaimOutcome> {
            self.db
                .run(|txn, _| async move {
                    let seen = *self.observation.lock().unwrap();
                    let (outcome, updated) = self
                        .election
                        .try_claim(&txn, &self.id, self.lease, attempt, seen, || {
                            self.clock.monotonic()
                        })
                        .await?;
                    *self.observation.lock().unwrap() = updated;
                    Ok::<_, LeaderElectionError>(outcome)
                })
                .await
        }

        async fn refresh(&self, grant: &LeaseGrant) -> Result<RefreshOutcome> {
            let attempt = RefreshAttempt::new(grant, self.clock.monotonic());
            let attempt = &attempt;
            self.db
                .run(|txn, _| async move { self.election.refresh(&txn, grant, attempt).await })
                .await
        }

        async fn resign(&self, grant: &LeaseGrant) -> Result<ResignOutcome> {
            self.db
                .run(|txn, _| async move { self.election.resign(&txn, grant).await })
                .await
        }
    }

    fn won(outcome: ClaimOutcome) -> LeaseGrant {
        match outcome {
            ClaimOutcome::Won(grant) => grant,
            other => panic!("expected to win the term, got {other:?}"),
        }
    }

    fn denied(outcome: ClaimOutcome) -> Duration {
        match outcome {
            ClaimOutcome::Denied { retry_after, .. } => retry_after,
            other => panic!("expected to be denied the term, got {other:?}"),
        }
    }

    fn elector_for(
        db: &Arc<Database>,
        subspace: &Subspace,
        id: &str,
        lease: Duration,
    ) -> Result<LeaderElector> {
        LeaderElector::new(
            Arc::clone(db),
            subspace.clone(),
            id,
            ElectorConfig::new(lease)?,
            // One clock per process, as in production, and a generator seeded
            // from the id so two electors of a test never jitter in lockstep
            // yet each run is reproducible.
            Environment::new(
                TestClock::new(),
                Arc::new(SeededRng::new(id.bytes().map(u64::from).sum())),
            ),
            Arc::new(TestTimer),
        )
    }

    /// One round of a follower's discovery loop: read the record, nothing else.
    async fn poll_leader(db: &Database, election: &LeaderElection) -> Result<Option<LeaderRecord>> {
        db.run(|txn, _| async move { election.leader(&txn).await })
            .await
    }

    /// What a poller compares between two rounds
    ///
    /// Occupancy is part of it because a resign preserves both ballot and
    /// generation, and would otherwise be invisible.
    fn identity(record: &LeaderRecord) -> (u64, u64, bool) {
        (record.ballot(), record.generation(), record.is_vacant())
    }

    // ========================================================================
    // PRIMITIVES
    // ========================================================================

    /// A first claim takes ballot 1 and leaves a record an observer can read
    /// back, with one matching entry in the transition history.
    #[tokio::test]
    async fn claim_vacant_then_state() -> Result<()> {
        let (db, subspace) = setup("le_claim_vacant_then_state").await?;
        let leader = Contender::new(&db, &subspace, "leader-a", Duration::from_secs(5))?;

        let grant = won(leader.claim().await?);
        assert_eq!(grant.ballot(), 1, "a never-claimed term starts at ballot 1");
        assert_eq!(grant.generation(), 0);
        assert_eq!(grant.leader_id(), "leader-a");

        let election = &leader.election;
        let (record, history) = db
            .run(|txn, _| async move {
                let record = election.leader(&txn).await?;
                let history = election.history(&txn, 10).await?;
                Ok::<_, LeaderElectionError>((record, history))
            })
            .await?;

        let record = record.expect("the claim wrote a record");
        assert_eq!(record.ballot(), 1);
        assert_eq!(record.leader_id(), Some("leader-a"));
        assert_eq!(
            record.lease(),
            Some(LeaseDuration::new(Duration::from_secs(5))?)
        );
        assert!(!record.is_vacant());
        assert_eq!(record.identity().ballot, grant.ballot());
        assert_eq!(record.identity().generation, grant.generation());
        assert_eq!(record.token(), grant.token());

        assert_eq!(history.len(), 1, "one transition, one history entry");
        assert_eq!(history[0].kind(), HistoryEventKind::Claim);
        assert_eq!(history[0].ballot(), 1);
        assert_eq!(history[0].leader_id(), "leader-a");

        Ok(())
    }

    /// Many processes claiming a vacant term at once: the read conflict on the
    /// leader key is the whole mutual exclusion argument, so exactly one may
    /// come back holding it, and no ballot may be handed out twice.
    #[tokio::test]
    async fn concurrent_claim_race() -> Result<()> {
        const RACERS: usize = 8;

        let (db, subspace) = setup("le_concurrent_claim_race").await?;

        let mut tasks = Vec::with_capacity(RACERS);
        for racer in 0..RACERS {
            let db = Arc::clone(&db);
            let subspace = subspace.clone();
            tasks.push(tokio::spawn(async move {
                let id = format!("racer-{racer}");
                let contender = Contender::new(&db, &subspace, &id, Duration::from_secs(30))?;
                contender.claim().await.map(|outcome| (id, outcome))
            }));
        }

        let mut winners = Vec::new();
        for task in tasks {
            let (id, outcome) = task.await.expect("the racing task panicked")?;
            match outcome {
                ClaimOutcome::Won(grant) => winners.push((id, grant)),
                // A loser either conflicted on a claim it had already issued
                // (Superseded, terminally spent) or read the winner's record
                // before writing anything (Denied). Both are correct; winning
                // twice is not.
                ClaimOutcome::Denied { .. } | ClaimOutcome::Superseded => {}
            }
        }

        assert_eq!(
            winners.len(),
            1,
            "exactly one contender may hold the term, got {winners:?}"
        );
        let (winner_id, grant) = &winners[0];
        assert_eq!(
            grant.ballot(),
            1,
            "the first term of a subspace is ballot 1"
        );

        let election = LeaderElection::new(subspace);
        let record = db
            .run(|txn, _| {
                let election = &election;
                async move { election.leader(&txn).await }
            })
            .await?
            .expect("the winner wrote a record");
        assert_eq!(record.leader_id(), Some(winner_id.as_str()));
        assert_eq!(record.ballot(), 1);

        Ok(())
    }

    /// Resigning preserves the ballot, so the successor takes `ballot + 1`
    /// immediately: the fencing token never goes backwards, and an orderly
    /// handover costs no waiting at all.
    #[tokio::test]
    async fn resign_reclaim_ballot_continuity() -> Result<()> {
        let (db, subspace) = setup("le_resign_reclaim_ballot_continuity").await?;
        let first = Contender::new(&db, &subspace, "leader-a", Duration::from_secs(30))?;
        let second = Contender::new(&db, &subspace, "leader-b", Duration::from_secs(30))?;

        let grant = won(first.claim().await?);
        assert_eq!(grant.ballot(), 1);
        assert_eq!(first.resign(&grant).await?, ResignOutcome::Resigned);

        let election = &first.election;
        let vacated = db
            .run(|txn, _| async move { election.leader(&txn).await })
            .await?
            .expect("a resign writes a vacant record, it does not clear the key");
        assert!(vacated.is_vacant());
        assert_eq!(vacated.ballot(), 1, "a resign preserves the ballot");
        assert_eq!(vacated.leader_id(), None);

        // No observation window is owed: the previous holder said it was done,
        // which a lease running out only ever guesses at. A lease of 30s here
        // makes the point: waiting one out would blow the test's runtime.
        let successor = won(second.claim().await?);
        assert_eq!(
            successor.ballot(),
            2,
            "the successor continues the ballot sequence"
        );

        // Resigning a term that is no longer ours must not vacate somebody
        // else's.
        assert_eq!(first.resign(&grant).await?, ResignOutcome::NotHolder);

        let history = db
            .run(|txn, _| async move { election.history(&txn, 10).await })
            .await?;
        let trail: Vec<_> = history
            .iter()
            .rev()
            .map(|event| (event.kind(), event.ballot()))
            .collect();
        assert_eq!(
            trail,
            vec![
                (HistoryEventKind::Claim, 1),
                (HistoryEventKind::Resign, 1),
                (HistoryEventKind::Claim, 2),
            ],
            "the history is the commit-ordered trail of transitions"
        );

        Ok(())
    }

    /// A contender may not take a live term before it has watched the record
    /// hold still for the lease it advertises, and must be able to take it once
    /// it has.
    #[tokio::test]
    async fn lease_expiry_steal_timing() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(3);

        let (db, subspace) = setup("le_lease_expiry_steal_timing").await?;
        let holder = Contender::new(&db, &subspace, "leader-a", LEASE)?;
        let thief = Contender::new(&db, &subspace, "leader-b", LEASE)?;

        let grant = won(holder.claim().await?);
        assert_eq!(grant.ballot(), 1);

        // First sighting: the window opens now, so this call can never steal
        // however long the record has actually been there.
        let remaining = denied(thief.claim().await?);
        assert_eq!(
            remaining, LEASE,
            "a first sighting owes the whole advertised lease"
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
        let remaining = denied(thief.claim().await?);
        assert!(
            remaining < LEASE,
            "the window should have advanced, {remaining:?} still owed"
        );

        // Comfortably past the lease as measured from the first sighting.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let stolen = won(thief.claim().await?);
        assert_eq!(stolen.ballot(), 2, "a steal takes the next ballot");

        let election = &holder.election;
        let history = db
            .run(|txn, _| async move { election.history(&txn, 10).await })
            .await?;
        assert_eq!(
            history[0].kind(),
            HistoryEventKind::Steal,
            "taking a live term is recorded as a steal, not a claim"
        );

        // The dispossessed holder learns about it on its next renewal.
        assert!(matches!(
            holder.refresh(&grant).await?,
            RefreshOutcome::Lost { .. }
        ));

        Ok(())
    }

    /// A renewal changes the record's identity, which restarts every observer's
    /// window. That is what makes a live leader safe without any process ever
    /// comparing its clock with another's.
    #[tokio::test]
    async fn renewal_extends_observation_reset() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(4);

        let (db, subspace) = setup("le_renewal_extends_observation_reset").await?;
        let holder = Contender::new(&db, &subspace, "leader-a", LEASE)?;
        let watcher = Contender::new(&db, &subspace, "leader-b", LEASE)?;

        let grant = won(holder.claim().await?);
        assert_eq!(denied(watcher.claim().await?), LEASE);

        tokio::time::sleep(Duration::from_secs(3)).await;

        let renewed = match holder.refresh(&grant).await? {
            RefreshOutcome::Refreshed(renewed) => renewed,
            other => panic!("the holder should still hold its term, got {other:?}"),
        };
        assert_eq!(renewed.ballot(), grant.ballot(), "a renewal keeps the term");
        assert_eq!(
            renewed.generation(),
            grant.generation() + 1,
            "a renewal bumps the generation"
        );

        // The window restarts here, at the renewal.
        let remaining = denied(watcher.claim().await?);
        assert_eq!(
            remaining, LEASE,
            "an identity change owes the whole lease again"
        );

        // Past the original lease, but not past the restarted window.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let remaining = denied(watcher.claim().await?);
        assert!(
            remaining > Duration::ZERO,
            "the renewal must have pushed the steal out, {remaining:?} owed"
        );

        Ok(())
    }

    /// Discovery is polling, so everything a follower can learn it learns by
    /// re-reading the record and comparing what it found with what it found
    /// last time. This is that loop, one round per transition, and it pins the
    /// property the loop rests on: every applied write moves the identity a
    /// poller compares, including a resign, which preserves both ballot and
    /// generation.
    #[tokio::test]
    async fn poll_discovery() -> Result<()> {
        let (db, subspace) = setup("le_poll_discovery").await?;
        let holder = Contender::new(&db, &subspace, "leader-a", Duration::from_secs(30))?;
        // A follower of its own: it shares nothing with the holder but the
        // subspace, which is all a real one would have.
        let follower = LeaderElection::new(subspace.clone());

        assert!(
            poll_leader(&db, &follower).await?.is_none(),
            "a never-claimed term has nothing to discover"
        );

        let grant = won(holder.claim().await?);
        let claimed = poll_leader(&db, &follower)
            .await?
            .expect("a claimed term must be readable by anybody");
        assert_eq!(claimed.ballot(), grant.ballot());
        assert_eq!(claimed.leader_id(), Some("leader-a"));

        // A renewal moves the identity too, and that is not noise: it is the
        // signal that keeps a contender's observation window alive rather than
        // authorizing a steal.
        let mut grant = grant;
        for _ in 0..2 {
            grant = match holder.refresh(&grant).await? {
                RefreshOutcome::Refreshed(renewed) => renewed,
                other => panic!("the holder should still hold its term, got {other:?}"),
            };
        }
        let renewed = poll_leader(&db, &follower).await?.expect("still held");
        assert_ne!(
            identity(&claimed),
            identity(&renewed),
            "a poller must be able to tell that the holder is still alive"
        );
        assert_eq!(
            renewed.ballot(),
            claimed.ballot(),
            "a renewal is the same term, so the ballot must not move"
        );

        assert_eq!(holder.resign(&grant).await?, ResignOutcome::Resigned);
        let vacated = poll_leader(&db, &follower).await?.expect("still a record");
        assert_ne!(
            identity(&renewed),
            identity(&vacated),
            "a resign preserves ballot and generation, so occupancy is the only thing \
             that can carry it to a poller"
        );
        assert!(vacated.is_vacant());

        Ok(())
    }

    /// A renewal and a steal racing at the boundary: both transactions read and
    /// write the leader key, so one of them has to lose, and the loser must
    /// find out rather than proceed.
    #[tokio::test]
    async fn concurrent_renew_vs_steal() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(3);

        let (db, subspace) = setup("le_concurrent_renew_vs_steal").await?;
        let holder = Contender::new(&db, &subspace, "leader-a", LEASE)?;
        let thief = Contender::new(&db, &subspace, "leader-b", LEASE)?;

        let grant = won(holder.claim().await?);
        // Opens the thief's window; no renewal happens until the race, so the
        // window runs uninterrupted.
        assert_eq!(denied(thief.claim().await?), LEASE);
        tokio::time::sleep(LEASE + Duration::from_millis(500)).await;

        let (refreshed, claimed) = tokio::join!(holder.refresh(&grant), thief.claim());

        match (refreshed?, claimed?) {
            // The renewal got there first: the thief's window restarted on the
            // new generation and it is owed a full lease again.
            (RefreshOutcome::Refreshed(renewed), ClaimOutcome::Denied { .. }) => {
                assert_eq!(renewed.ballot(), grant.ballot());
                assert_eq!(renewed.generation(), grant.generation() + 1);
            }
            // The steal got there first: the renewal reads a record that is no
            // longer its own.
            (RefreshOutcome::Lost { observed }, ClaimOutcome::Won(stolen)) => {
                assert_eq!(stolen.ballot(), grant.ballot() + 1);
                let observed = observed.expect("the thief's record is there to be read");
                assert_eq!(observed.ballot(), stolen.ballot());
            }
            (refresh, claim) => panic!(
                "a renewal and a steal must not both take effect: refresh {refresh:?}, claim {claim:?}"
            ),
        }

        Ok(())
    }

    /// A claim whose reply was lost: the retry recognizes the record it wrote
    /// itself and adopts it, instead of spending a second ballot on a term it
    /// already holds.
    #[tokio::test]
    async fn maybe_committed_idempotence() -> Result<()> {
        let (db, subspace) = setup("le_maybe_committed_idempotence").await?;
        let leader = Contender::new(&db, &subspace, "leader-a", Duration::from_secs(30))?;

        // The attempt outlives the transaction on purpose: this is exactly the
        // object a `commit_unknown_result` retry would still be holding.
        let attempt = ClaimAttempt::new(ClaimToken::generate(), leader.clock.monotonic())?;
        let first = won(leader.claim_with(&attempt).await?);
        assert_eq!(first.ballot(), 1);
        assert!(
            attempt.maybe_committed(),
            "issuing the write is what makes the attempt recoverable"
        );

        // Replay the same attempt against the record it already committed.
        let second = won(leader.claim_with(&attempt).await?);
        assert_eq!(
            second.ballot(),
            first.ballot(),
            "a retry must not re-ballot"
        );
        assert_eq!(second.generation(), first.generation());
        assert_eq!(second.token(), first.token());
        assert!(
            !attempt.is_retired(),
            "recovering its own record is not a supersession"
        );

        let election = &leader.election;
        let (record, history) = db
            .run(|txn, _| async move {
                let record = election.leader(&txn).await?;
                let history = election.history(&txn, 10).await?;
                Ok::<_, LeaderElectionError>((record, history))
            })
            .await?;
        let record = record.expect("the claim wrote a record");
        assert_eq!(record.ballot(), 1);
        assert_eq!(record.generation(), 0, "recovery writes nothing at all");
        assert_eq!(
            history.len(),
            1,
            "one term, one history entry, however many times the claim was replayed"
        );

        Ok(())
    }

    // ========================================================================
    // HANDLE LAYER
    // ========================================================================

    /// The ordinary path: campaign, run work that can see it is leading, hand
    /// the term back, leave the record vacant for the next process.
    #[tokio::test]
    async fn elector_end_to_end() -> Result<()> {
        let (db, subspace) = setup("le_elector_end_to_end").await?;
        let elector = elector_for(&db, &subspace, "leader-a", Duration::from_secs(4))?;

        let outcome = elector
            .lead(|handle| async move { (handle.status(), handle.ballot()) })
            .await?;

        let (status, ballot) = match outcome {
            LeadOutcome::Completed { value, released } => {
                assert!(released, "an elector whose work returned should resign");
                value
            }
            LeadOutcome::LeaseLost => panic!("the term ended before the work did"),
        };
        assert_eq!(
            status,
            LeaseStatus::Leading,
            "the work runs under a live term"
        );
        assert_eq!(ballot, 1);

        let record = elector
            .current_record()
            .await?
            .expect("a resign writes a vacant record");
        assert!(record.is_vacant(), "the term is free for the next process");
        assert_eq!(record.ballot(), 1, "and the ballot is preserved");

        Ok(())
    }

    /// A leader that stops running, rather than resigning, costs its successor
    /// exactly one lease. The abandoned handle finds out on its own clock, with
    /// nobody to tell it.
    #[tokio::test]
    async fn handle_layer_lifecycle() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(3);

        let (db, subspace) = setup("le_handle_layer_lifecycle").await?;
        let first = elector_for(&db, &subspace, "leader-a", LEASE)?;
        let second = elector_for(&db, &subspace, "leader-b", LEASE)?;

        let (leading_tx, leading_rx) = tokio::sync::oneshot::channel();
        let leading = tokio::spawn(async move {
            first
                .lead(|handle| async move {
                    leading_tx.send(handle.clone()).expect("the test went away");
                    // Never returns: the only way out is losing the term.
                    futures::future::pending::<()>().await;
                })
                .await
        });

        let handle = leading_rx.await.expect("the first elector never led");
        assert_eq!(handle.status(), LeaseStatus::Leading);
        assert_eq!(handle.ballot(), 1);

        // An ungraceful release: the future is dropped, so no resign, and no
        // more renewals either.
        leading.abort();

        let started = Instant::now();
        let outcome = second.lead(|handle| async move { handle.ballot() }).await?;
        let waited = started.elapsed();

        match outcome {
            LeadOutcome::Completed { value, .. } => {
                assert_eq!(value, 2, "the successor takes the next ballot");
            }
            LeadOutcome::LeaseLost => panic!("the successor lost a term it had just taken"),
        }
        assert!(
            waited >= LEASE,
            "a crashed leader must cost a full lease of observation, waited only {waited:?}"
        );

        // Nothing told this handle anything. Its own clock did.
        assert_eq!(
            handle.status(),
            LeaseStatus::Lost,
            "an abandoned handle must go stale on its own"
        );
        assert!(handle.check().is_err());

        Ok(())
    }

    /// The other half of the resign asymmetry, at the handle layer: a follower
    /// polling its campaign takes over as soon as the leader is done, in far
    /// less than the lease it would have had to wait out.
    #[tokio::test]
    async fn elector_handoff_on_completion() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(6);

        let (db, subspace) = setup("le_elector_handoff_on_completion").await?;
        let first = elector_for(&db, &subspace, "leader-a", LEASE)?;
        let second = elector_for(&db, &subspace, "leader-b", LEASE)?;

        let (leading_tx, leading_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let leading = tokio::spawn(async move {
            first
                .lead(|handle| async move {
                    leading_tx
                        .send(handle.ballot())
                        .expect("the test went away");
                    let _ = finish_rx.await;
                })
                .await
        });
        assert_eq!(
            leading_rx.await.expect("the first elector never led"),
            1,
            "the first term of a subspace is ballot 1"
        );

        let follower = tokio::spawn(async move {
            let started = Instant::now();
            let outcome = second.lead(|handle| async move { handle.ballot() }).await;
            (started.elapsed(), outcome)
        });

        // Long enough for the follower to have been denied and parked between
        // two campaign polls, short enough to stay far from the lease.
        tokio::time::sleep(Duration::from_millis(500)).await;
        finish_tx.send(()).expect("the leader went away");

        assert!(matches!(
            leading.await.expect("the leading task panicked")?,
            LeadOutcome::Completed { released: true, .. }
        ));

        let (waited, outcome) = follower.await.expect("the follower task panicked");
        match outcome? {
            LeadOutcome::Completed { value, .. } => {
                assert_eq!(value, 2, "a vacated term is reclaimed at the next ballot");
            }
            LeadOutcome::LeaseLost => panic!("the successor lost a term it had just taken"),
        }
        assert!(
            waited < LEASE - Duration::from_secs(2),
            "a resigned term must be handed over, not waited out: took {waited:?} of a {LEASE:?} lease"
        );

        Ok(())
    }

    /// Leading is not reentrant. A second `lead` on the same elector, with the
    /// same id in the same subspace, is a contender like any other: it queues
    /// behind the running term and takes the next ballot once that term is
    /// handed back. Sharing a term is what cloning the handle is for.
    #[tokio::test]
    async fn concurrent_lead_queues_as_successor() -> Result<()> {
        const LEASE: Duration = Duration::from_secs(3);

        let (db, subspace) = setup("le_concurrent_lead_queues_as_successor").await?;
        let elector = elector_for(&db, &subspace, "leader-a", LEASE)?;

        let (leading_tx, leading_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();

        // Everything an application would have to share for this to look like
        // re-entering its own term: one elector, one id, one subspace.
        let holding = elector.lead(|handle| async move {
            leading_tx
                .send(handle.ballot())
                .expect("the test went away");
            let _ = finish_rx.await;
        });
        let queued = elector.lead(|handle| async move { handle.ballot() });
        futures::pin_mut!(holding);
        futures::pin_mut!(queued);

        let ballot_a = tokio::select! {
            outcome = &mut holding => panic!("the term ended before the work did: {outcome:?}"),
            ballot = leading_rx => ballot.expect("the first call never led"),
        };
        assert_eq!(ballot_a, 1, "the first term of a subspace is ballot 1");

        // One-sided: while the first call holds the term and keeps renewing it,
        // the second cannot get in. Both are polled, so the first really is
        // renewing rather than merely parked.
        let raced = tokio::time::timeout(Duration::from_millis(500), async {
            tokio::select! {
                outcome = &mut holding => panic!("the term ended before the work did: {outcome:?}"),
                outcome = &mut queued => outcome,
            }
        })
        .await;
        assert!(
            raced.is_err(),
            "a second lead must queue behind the running term, got {raced:?}"
        );

        finish_tx.send(()).expect("the leader went away");
        assert!(matches!(
            (&mut holding).await?,
            LeadOutcome::Completed { released: true, .. }
        ));

        let ballot_b = match tokio::time::timeout(Duration::from_secs(30), queued)
            .await
            .expect("the queued campaign never won the term")?
        {
            LeadOutcome::Completed { value, .. } => value,
            LeadOutcome::LeaseLost => panic!("the successor lost a term it had just taken"),
        };
        assert!(
            ballot_b > ballot_a,
            "the second call must run under a later term, got {ballot_b} after {ballot_a}"
        );

        let election = elector.election();
        let history = db
            .run(|txn, _| async move { election.history(&txn, 10).await })
            .await?;
        let trail: Vec<_> = history
            .iter()
            .rev()
            .map(|event| (event.kind(), event.ballot()))
            .collect();
        assert_eq!(
            trail[..3],
            [
                (HistoryEventKind::Claim, ballot_a),
                (HistoryEventKind::Resign, ballot_a),
                (HistoryEventKind::Claim, ballot_b),
            ],
            "the second call is recorded as a successor, not as a re-entry: {trail:?}"
        );

        Ok(())
    }

    // ========================================================================
    // FENCING COMPOSITION
    // ========================================================================

    /// The scenario fencing exists for: a leader is wedged long enough to lose
    /// its term, wakes up still believing it leads, and writes. The register
    /// rejects that write because the new leader installed a higher fence, and
    /// the new leader's own write goes through.
    ///
    /// Note the ordering: winning ballot 2 fences nothing by itself. The fence
    /// is installed by the successor's `read` at its own rank, which is why
    /// that call comes before any fenced work.
    #[cfg(feature = "recipes-ranked-register")]
    #[tokio::test]
    async fn fencing_end_to_end() -> std::result::Result<(), Box<dyn std::error::Error>> {
        use foundationdb::recipes::ranked_register::{
            RankedRegister, RankedRegisterError, WriteResult,
        };

        const LEASE: Duration = Duration::from_secs(3);

        let (db, subspace) = setup("le_fencing_end_to_end").await?;
        let register = RankedRegister::new(subspace.subspace(&"state"));
        let wedged = Contender::new(&db, &subspace, "leader-a", LEASE)?;
        let successor = Contender::new(&db, &subspace, "leader-b", LEASE)?;

        let old = won(wedged.claim().await?);
        assert_eq!(old.ballot(), 1);

        // Activation: winning a term fences nothing until the holder has read
        // at its own rank. Only then does the fenced work happen.
        let register_ref = &register;
        let (old_fence, old_write) = (old.rank(0), old.rank(1));
        let written = db
            .run(|txn, _| async move {
                register_ref.read(&txn, old_fence).await?;
                let written = register_ref.write(&txn, old_write, b"written-by-a").await?;
                Ok::<_, RankedRegisterError>(written)
            })
            .await?;
        assert_eq!(written, WriteResult::Committed);

        // The wedged leader is now out of contact for longer than its lease.
        assert_eq!(denied(successor.claim().await?), LEASE);
        tokio::time::sleep(LEASE + Duration::from_millis(500)).await;
        let new = won(successor.claim().await?);
        assert_eq!(new.ballot(), 2);
        assert!(
            new.rank(0) > old.rank(u32::MAX),
            "every rank of a term must dominate every rank of the one before"
        );

        let new_fence = new.rank(0);
        let fenced = db
            .run(|txn, _| async move { register_ref.read(&txn, new_fence).await })
            .await?;
        assert_eq!(
            fenced.value(),
            Some(&b"written-by-a"[..]),
            "the successor reads what its predecessor left"
        );

        // The wedged leader wakes up and writes, still believing it leads.
        let stale_write = old.rank(2);
        let stale = db
            .run(|txn, _| async move { register_ref.write(&txn, stale_write, b"written-late").await })
            .await?;
        assert_eq!(
            stale,
            WriteResult::Aborted,
            "a write from a dispossessed term must be rejected"
        );

        let new_write = new.rank(1);
        let fresh = db
            .run(|txn, _| async move { register_ref.write(&txn, new_write, b"written-by-b").await })
            .await?;
        assert_eq!(fresh, WriteResult::Committed);

        let value = db
            .run(|txn, _| async move { register_ref.value(&txn).await })
            .await?;
        assert_eq!(
            value.as_deref(),
            Some(&b"written-by-b"[..]),
            "the stale write must not have landed"
        );

        Ok(())
    }
}
