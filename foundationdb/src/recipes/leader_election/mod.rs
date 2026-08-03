// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Poll-based Dynamo-style leader leases with FoundationDB fencing.
//!
//! This recipe adapts the client-side timing model of the
//! [Amazon DynamoDB lock client](https://aws.amazon.com/blogs/aws/new-amazon-dynamodb-lock-client/):
//! a durable owner record is observed, and a caller's own monotonic elapsed
//! time decides when that unchanged record is suspicious. FoundationDB's
//! serializable transaction on the single state key, not local time, decides
//! which concurrent acquire, renewal, reacquisition, or takeover wins.
//! It is Dynamo-inspired, not wire-compatible with the DynamoDB lock client.
//!
//! Every owner record persists its lease duration alongside a monotonically
//! increasing revision. Followers wait the duration stored in their exact
//! observation, never this handle's local duration. No timestamp or deadline
//! is persisted. A leadership token may renew only while its caller-local
//! elapsed time is below the duration stored in that exact durable revision.
//! The caller supplies two monotonic-clock readings. Pass `attempt_started_at`
//! immediately before each `poll` call in every retry attempt. It governs
//! renewal and takeover eligibility, and stamps new leadership, so time spent
//! reading or committing only shortens local validity. After `Database::run`
//! succeeds, pass a fresh `adopted_at` to [`PollResult::into_next_state`]. That
//! starts timing for a new or reset observation only after its durable read is
//! known to have committed; an unchanged observation retains its original time.
//!
//! A successful acquisition, renewal, reacquisition, or takeover returns a fresh
//! [`Rank`](crate::recipes::ranked_register::Rank). Leadership status alone
//! never authorizes an unfenced external side effect. Correctness-sensitive
//! FoundationDB work must use that rank with a
//! [`RankedRegister`](crate::recipes::ranked_register::RankedRegister) in the
//! same enclosing transaction; an external sink must atomically enforce the
//! rank and reject older ranks.
//!
//! Local elapsed time only permits an attempted conditional takeover. It does
//! not prove that the old process stopped. A failed or unavailable poll cannot
//! renew leadership, so callers must not continue protected work from a stale
//! local token.
//!
//! The caller owns scheduling and local state. This component never calls
//! `Database::run`, retries, sets transaction options, sleeps, draws random
//! values, starts background work, or reads wall-clock time. Adopt a returned
//! [`PollResult`] or [`LocalState`] only after the enclosing `Database::run`
//! succeeds, so retries, cancellation, and unknown commits authorize no work.
//!
//! ## Further reading
//!
//! - [AWS Builders Library: Leader Election in Distributed Systems](https://aws.amazon.com/builders-library/leader-election-in-distributed-systems/)
//! - [Martin Kleppmann: How to do distributed locking](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html)
//! - Mike Burrows, "The Chubby Lock Service for Loosely-Coupled Distributed
//!   Systems" (OSDI 2006).
//! - Gregory Chockler and Dahlia Malkhi, "Active Disk Paxos with Infinitely
//!   Many Processes" (PODC 2002).
//! - Salman Niazi, Mahmoud Ismail, Gautier Berthou, and Jim Dowling, "Leader
//!   Election Using NewSQL Database Systems" (DAIS 2015, LNCS 9038).
//! - [AWS Labs: Amazon DynamoDB Lock Client](https://github.com/awslabs/amazon-dynamodb-lock-client)
//! - [Apache ZooKeeper Recipes, GUID note](https://zookeeper.apache.org/doc/r3.5.5/recipes.html)

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{LeaderElectionError, Result};
pub use types::{
    ElectionState, Leadership, LocalState, Observation, ParticipantId, PollOutcome, PollResult,
    PollTransition, ResignOutcome,
};

use crate::{Transaction, tuple::Subspace};
use std::ops::Deref;
use std::time::Duration;

/// A handle for one independently scoped leader lease.
///
/// `lease_duration` is this caller's non-zero desired duration. It is
/// persisted on every successful acquisition, takeover, and renewal. Existing
/// foreign records remain governed by their own persisted duration.
#[derive(Clone, Debug)]
pub struct LeaderElection {
    subspace: Subspace,
    lease_duration: Duration,
}

impl LeaderElection {
    /// Creates an election handle with the non-zero duration it will persist on
    /// ownership changes.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(subspace))
    )]
    pub fn new(subspace: Subspace, lease_duration: Duration) -> Result<Self> {
        if lease_duration.is_zero() {
            return Err(LeaderElectionError::InvalidLeaseDuration);
        }
        Ok(Self {
            subspace,
            lease_duration,
        })
    }

    /// Returns this handle's desired duration for its future owner records.
    #[cfg_attr(feature = "trace", tracing::instrument(level = "debug", skip(self)))]
    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    /// Polls the durable lease state in the caller's transaction.
    ///
    /// A released state is acquired immediately. An exact, locally unexpired
    /// [`Leadership`] token renews ownership with a fresh revision. A first or
    /// changed [`Observation`] never steals; only an exact unchanged observation
    /// may take over, or same-owner reacquire, after the observed record's
    /// persisted duration. An expired or mismatched leadership token becomes
    /// observation/reacquisition state and cannot renew directly.
    ///
    /// `attempt_started_at` must be read from the caller's monotonic clock
    /// immediately before this call in each retry attempt. It is deliberately
    /// before the durable read, making renewal and takeover decisions
    /// conservative relative to read and commit delay. After the enclosing
    /// `Database::run` succeeds, pass a fresh caller-clock reading to
    /// [`PollResult::into_next_state`] to adopt the returned state.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn, participant, local_state))
    )]
    pub async fn poll<T>(
        &self,
        txn: &T,
        participant: &ParticipantId,
        local_state: &LocalState,
        attempt_started_at: Duration,
    ) -> Result<PollResult>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::poll(
            txn,
            &self.subspace,
            self.lease_duration,
            participant,
            local_state,
            attempt_started_at,
        )
        .await
    }

    /// Reads durable state without making any liveness or leadership-validity claim.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn))
    )]
    pub async fn state<T>(&self, txn: &T) -> Result<ElectionState>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::state(txn, &self.subspace).await
    }

    /// Releases ownership only when `leadership` still exactly matches durable state.
    ///
    /// The revision and persisted duration remain, so a later acquisition has
    /// a strictly newer fencing rank. A stale delayed resignation is rejected.
    #[cfg_attr(
        feature = "trace",
        tracing::instrument(level = "debug", skip(self, txn, leadership))
    )]
    pub async fn resign<T>(&self, txn: &T, leadership: &Leadership) -> Result<ResignOutcome>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::resign(txn, &self.subspace, leadership).await
    }
}
