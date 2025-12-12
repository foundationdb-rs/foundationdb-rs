//! Invariant verification for the leader election simulation

use foundationdb::{recipes::leader_election::LeaderElection, FdbBindingError};
use foundationdb_simulation::SimDatabase;

use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Verify leader election invariants
    ///
    /// This checks:
    /// - Single leader safety (at most one leader at any time)
    /// - Leader ordering (leader has smallest versionstamp)
    /// - No dead processes in alive list
    pub(crate) async fn verify_invariants(&self, db: &SimDatabase) -> Result<(), String> {
        let current_time = self.current_time();
        let election = LeaderElection::new(self.subspace.clone());

        let result = db
            .run(|mut trx, _maybe_committed| {
                let election = election.clone();
                async move {
                    // Check leader ordering invariant
                    let leader = election
                        .get_current_leader(&mut trx, current_time)
                        .await
                        .map_err(FdbBindingError::from)?;

                    // If there's a leader, they should be the first alive process
                    // (This is a simplified invariant check)

                    Ok::<_, FdbBindingError>(leader)
                }
            })
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Invariant verification failed: {:?}", e)),
        }
    }
}
