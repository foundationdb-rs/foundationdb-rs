//! Time anomaly injection for simulation testing

use std::time::Duration;

use super::types::{TimeAnomaly, BASE_OFFSET_SECS};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Convert simulation time to Duration timestamp
    pub(crate) fn sim_time_to_duration(&self, sim_time: f64) -> Duration {
        Duration::from_secs(BASE_OFFSET_SECS)
            + Duration::from_secs_f64(sim_time)
            + self.clock_skew
    }

    /// Get current simulation time as Duration
    pub(crate) fn current_time(&self) -> Duration {
        self.sim_time_to_duration(self.context.now())
    }

    /// Select a time anomaly based on weights
    pub(crate) fn select_time_anomaly(&self) -> TimeAnomaly {
        let roll = self.random_range(1000);
        match roll {
            0..=699 => TimeAnomaly::Normal,        // 70%
            700..=819 => TimeAnomaly::Stale,       // 12%
            820..=899 => TimeAnomaly::Future,      // 8%
            900..=929 => TimeAnomaly::ExactlyAtTimeout,    // 3%
            930..=959 => TimeAnomaly::JustBeforeTimeout,   // 3%
            960..=989 => TimeAnomaly::JustAfterTimeout,    // 3%
            990..=994 => TimeAnomaly::Zero,        // 0.5%
            _ => TimeAnomaly::TimeReversal,        // 0.5%
        }
    }

    /// Apply time anomaly to a timestamp
    pub(crate) fn apply_time_anomaly(
        &self,
        base_time: Duration,
        anomaly: TimeAnomaly,
        last_heartbeat: Option<Duration>,
    ) -> Duration {
        match anomaly {
            TimeAnomaly::Normal => base_time,
            TimeAnomaly::Stale => {
                let offset = Duration::from_secs((self.random_range(60) + 1) as u64);
                base_time.saturating_sub(offset)
            }
            TimeAnomaly::Future => {
                let offset = Duration::from_secs((self.random_range(60) + 1) as u64);
                base_time + offset
            }
            TimeAnomaly::ExactlyAtTimeout => {
                if let Some(last) = last_heartbeat {
                    last + self.heartbeat_timeout
                } else {
                    base_time
                }
            }
            TimeAnomaly::JustBeforeTimeout => {
                if let Some(last) = last_heartbeat {
                    last + self.heartbeat_timeout - Duration::from_nanos(1)
                } else {
                    base_time
                }
            }
            TimeAnomaly::JustAfterTimeout => {
                if let Some(last) = last_heartbeat {
                    last + self.heartbeat_timeout + Duration::from_nanos(1)
                } else {
                    base_time
                }
            }
            TimeAnomaly::Zero => Duration::ZERO,
            TimeAnomaly::TimeReversal => {
                if let Some(last) = last_heartbeat {
                    last.saturating_sub(Duration::from_secs(1))
                } else {
                    Duration::from_secs(1)
                }
            }
        }
    }
}
