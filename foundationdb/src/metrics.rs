use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Label key-value pairs for metrics
pub type Labels = Vec<(String, String)>;

/// Unique key for a metric: name + labels
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub struct MetricKey {
    name: String,
    labels: Labels,
}

impl MetricKey {
    /// Create a new MetricKey
    ///
    /// # Arguments
    /// * `name` - The name of the metric
    /// * `labels` - Key-value pairs for labeling the metric
    ///
    /// # Returns
    /// * `MetricKey` - A new MetricKey instance
    pub fn new(name: &str, labels: &[(&str, &str)]) -> Self {
        // Convert labels to owned strings
        let mut sorted_labels: Labels = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        // Sort labels by key to ensure consistent ordering
        sorted_labels.sort_by(|a, b| a.0.cmp(&b.0));

        Self {
            name: name.to_string(),
            labels: sorted_labels,
        }
    }
}

/// Tracks metrics for a transaction.
///
/// This struct maintains transaction metrics protected by a mutex to allow safe concurrent access.
#[derive(Debug, Clone, Default)]
pub struct TransactionMetrics {
    /// All metrics for the transaction, organized by category
    pub metrics: Arc<Mutex<MetricsReport>>,
}

/// Transaction-level information that persists across retries
#[derive(Debug, Default, Clone)]
pub struct TransactionInfo {
    /// Number of retries performed
    pub retries: u64,
    /// Transaction read version
    pub read_version: Option<i64>,
    /// Transaction commit version
    pub commit_version: Option<i64>,
}

/// Data structure containing the actual metrics for a transaction,
/// organized into different categories
#[derive(Debug, Clone, Default)]
pub struct MetricsReport {
    /// Metrics for the current transaction attempt
    pub current: CounterMetrics,
    /// Aggregated metrics across all transaction attempts, including retries
    pub total: CounterMetrics,
    /// Time-related metrics for performance analysis
    pub time: TimingMetrics,
    /// Custom metrics for the current transaction attempt
    pub custom_metrics: HashMap<MetricKey, u64>,
    /// Transaction-level information
    pub transaction: TransactionInfo,
}

impl MetricsReport {
    /// Set the read version for the transaction
    ///
    /// # Arguments
    /// * `version` - The read version
    pub fn set_read_version(&mut self, version: i64) {
        self.transaction.read_version = Some(version);
    }

    /// Set the commit version for the transaction
    ///
    /// # Arguments
    /// * `version` - The commit version
    pub fn set_commit_version(&mut self, version: i64) {
        self.transaction.commit_version = Some(version);
    }

    /// Increment the retry counter
    pub fn increment_retries(&mut self) {
        self.transaction.retries += 1;
    }
}

/// Time-related metrics for performance analysis of transaction execution phases
#[derive(Debug, Default, Clone)]
pub struct TimingMetrics {
    /// Time spent in commit phase execution
    pub commit_execution_ms: u64,
    /// Time spent handling errors and retries
    pub on_error_execution_ms: Vec<u64>,
    /// Total transaction duration from start to finish
    pub total_execution_ms: u64,
}

impl TimingMetrics {
    /// Record commit execution time
    ///
    /// # Arguments
    /// * `duration_ms` - The duration of the commit execution in milliseconds
    pub fn record_commit_time(&mut self, duration_ms: u64) {
        self.commit_execution_ms = duration_ms;
    }

    /// Add an error execution time to the list
    ///
    /// # Arguments
    /// * `duration_ms` - The duration of the error handling in milliseconds
    pub fn add_error_time(&mut self, duration_ms: u64) {
        self.on_error_execution_ms.push(duration_ms);
    }

    /// Set the total execution time
    ///
    /// # Arguments
    /// * `duration_ms` - The total duration of the transaction in milliseconds
    pub fn set_execution_time(&mut self, duration_ms: u64) {
        self.total_execution_ms = duration_ms;
    }

    /// Get the sum of all error handling times
    ///
    /// # Returns
    /// * `u64` - The total time spent handling errors in milliseconds
    pub fn get_total_error_time(&self) -> u64 {
        self.on_error_execution_ms.iter().sum()
    }
}

/// Represents a FoundationDB command.
pub enum FdbCommand {
    /// Atomic operation
    Atomic,
    /// Clear operation
    Clear,
    /// Clear range operation
    ClearRange,
    /// Get operation
    Get(u64, u64),
    GetRange(u64, u64),
    Set(u64),
}

const INCREMENT: u64 = 1;

impl TransactionMetrics {
    /// Create a new instance of TransactionMetrics
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(Mutex::new(MetricsReport::default())),
        }
    }

    /// Set the read version for the transaction
    ///
    /// # Arguments
    /// * `version` - The read version
    pub fn set_read_version(&self, version: i64) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.set_read_version(version);
    }

    /// Set the commit version for the transaction
    ///
    /// # Arguments
    /// * `version` - The commit version
    pub fn set_commit_version(&self, version: i64) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.set_commit_version(version);
    }

    /// Resets the current metrics and increments the retry counter in total metrics.
    ///
    /// This method is called when a transaction is retried due to a conflict or other retryable error.
    /// It increments the retry count in the transaction info and resets the current metrics to zero.
    /// It also merges current custom metrics into total custom metrics before clearing them.
    pub fn reset_current(&self) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.increment_retries();
        data.current = CounterMetrics::default();
        data.custom_metrics.clear();
    }

    /// Increment the retry counter
    pub fn increment_retries(&self) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.increment_retries();
    }

    /// Reports metrics for a specific FDB command by incrementing the appropriate counters.
    ///
    /// This method updates both the current and total metrics for the given command.
    ///
    /// # Arguments
    /// * `fdb_command` - The FDB command to report metrics for
    pub fn report_metrics(&self, fdb_command: FdbCommand) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.current.increment(&fdb_command);
        data.total.increment(&fdb_command);
    }

    /// Get the number of retries
    ///
    /// # Returns
    /// * `u64` - The number of retries
    pub fn get_retries(&self) -> u64 {
        let data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.transaction.retries
    }

    /// Returns a clone of all metrics data.
    ///
    /// # Returns
    /// * `MetricsData` - A clone of all metrics data
    pub fn get_metrics_data(&self) -> MetricsReport {
        let data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.clone()
    }

    /// Returns a clone of the transaction information.
    ///
    /// # Returns
    /// * `TransactionInfo` - A clone of the transaction information
    pub fn get_transaction_info(&self) -> TransactionInfo {
        let data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.transaction.clone()
    }

    /// Set a custom metric
    ///
    /// # Arguments
    /// * `name` - The name of the metric
    /// * `value` - The value to set
    /// * `labels` - Key-value pairs for labeling the metric
    pub fn set_custom(&self, name: &str, value: u64, labels: &[(&str, &str)]) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = MetricKey::new(name, labels);
        data.custom_metrics.insert(key.clone(), value);
    }

    /// Increment a custom metric
    ///
    /// # Arguments
    /// * `name` - The name of the metric
    /// * `amount` - The amount to increment by
    /// * `labels` - Key-value pairs for labeling the metric
    pub fn increment_custom(&self, name: &str, amount: u64, labels: &[(&str, &str)]) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = MetricKey::new(name, labels);
        // Increment in both current and total custom metrics
        *data.custom_metrics.entry(key.clone()).or_insert(0) += amount;
    }

    /// Record commit execution time
    ///
    /// # Arguments
    /// * `duration_ms` - The duration of the commit execution in milliseconds
    pub fn record_commit_time(&self, duration_ms: u64) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.time.commit_execution_ms = duration_ms;
    }

    /// Add an error execution time to the list
    ///
    /// # Arguments
    /// * `duration_ms` - The duration of the error handling in milliseconds
    pub fn add_error_time(&self, duration_ms: u64) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.time.on_error_execution_ms.push(duration_ms);
    }

    /// Set the total execution time
    ///
    /// # Arguments
    /// * `duration_ms` - The total duration of the transaction in milliseconds
    pub fn set_execution_time(&self, duration_ms: u64) {
        let mut data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.time.set_execution_time(duration_ms);
    }

    /// Get the sum of all error handling times
    ///
    /// # Returns
    /// * `u64` - The total time spent handling errors in milliseconds
    pub fn get_total_error_time(&self) -> u64 {
        let data = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.time.on_error_execution_ms.iter().sum()
    }
}

/// `CounterMetrics` is used in two contexts within the metrics system:
/// - As `current` metrics: tracking operations for the current transaction attempt
/// - As `total` metrics: aggregating operations across all transaction attempts including retries
///
/// Each counter is incremented when the corresponding operation is performed, allowing
/// for detailed analysis of transaction behavior and performance characteristics.
#[derive(Debug, Default, Clone)]
pub struct CounterMetrics {
    // ATOMIC OP
    /// Number of atomic operations performed (add, and, or, etc.)
    pub call_atomic_op: u64,
    // CLEAR
    /// Number of key clear operations performed
    pub call_clear: u64,
    // CLEAR RANGE
    /// Number of range clear operations performed
    pub call_clear_range: u64,
    // GET
    /// Number of get operations performed
    pub call_get: u64,
    /// Total number of key-value pairs fetched across all read operations
    pub keys_values_fetched: u64,
    /// Total number of bytes read from the database
    pub bytes_read: u64,
    // SET
    /// Number of set operations performed
    pub call_set: u64,
    /// Total number of bytes written to the database
    pub bytes_written: u64,
}

impl CounterMetrics {
    /// Increments the metrics for a given FDB command.
    ///
    /// This method updates the metrics counters based on the type of FDB command.
    ///
    /// # Arguments
    /// * `fdb_command` - The FDB command to increment metrics for
    pub fn increment(&mut self, fdb_command: &FdbCommand) {
        match fdb_command {
            FdbCommand::Atomic => self.call_atomic_op += INCREMENT,
            FdbCommand::Clear => self.call_clear += INCREMENT,
            FdbCommand::ClearRange => self.call_clear_range += INCREMENT,
            FdbCommand::Get(bytes_count, kv_fetched) => {
                self.keys_values_fetched += *kv_fetched;
                self.bytes_read += *bytes_count;
                self.call_get += INCREMENT
            }
            FdbCommand::GetRange(bytes_count, keys_values_fetched) => {
                self.keys_values_fetched += *keys_values_fetched;
                self.bytes_read += *bytes_count;
            }
            FdbCommand::Set(bytes_count) => {
                self.bytes_written += *bytes_count;
                self.call_set += INCREMENT
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_metric_key_equality() {
        // Test that different combinations of the same labels are considered equal
        let key1 = MetricKey::new("counter", &[("region", "us-west"), ("service", "api")]);
        let key2 = MetricKey::new("counter", &[("service", "api"), ("region", "us-west")]);

        // Same labels in different order should be considered equal
        assert_eq!(key1, key2);

        // Different label values should produce different keys
        let key3 = MetricKey::new("counter", &[("region", "us-east"), ("service", "api")]);
        assert_ne!(key1, key3);

        // Different label keys should produce different keys
        let key4 = MetricKey::new("counter", &[("zone", "us-west"), ("service", "api")]);
        assert_ne!(key1, key4);

        // Different metric names should produce different keys
        let key5 = MetricKey::new("timer", &[("region", "us-west"), ("service", "api")]);
        assert_ne!(key1, key5);
    }

    #[test]
    fn test_metric_key_in_hashmap() {
        let mut metrics = HashMap::new();

        // Insert metrics with different label combinations
        metrics.insert(
            MetricKey::new("counter", &[("region", "us-west"), ("service", "api")]),
            100,
        );
        metrics.insert(
            MetricKey::new("counter", &[("region", "us-east"), ("service", "api")]),
            200,
        );
        metrics.insert(
            MetricKey::new("timer", &[("region", "us-west"), ("service", "api")]),
            300,
        );

        // Verify we can retrieve metrics with the same label combinations
        assert_eq!(
            metrics.get(&MetricKey::new(
                "counter",
                &[("region", "us-west"), ("service", "api")]
            )),
            Some(&100)
        );

        // Verify we can retrieve metrics with labels in different order
        assert_eq!(
            metrics.get(&MetricKey::new(
                "counter",
                &[("service", "api"), ("region", "us-west")]
            )),
            Some(&100)
        );

        // Verify different label values produce different keys
        assert_eq!(
            metrics.get(&MetricKey::new(
                "counter",
                &[("region", "us-east"), ("service", "api")]
            )),
            Some(&200)
        );

        // Verify different metric names produce different keys
        assert_eq!(
            metrics.get(&MetricKey::new(
                "timer",
                &[("region", "us-west"), ("service", "api")]
            )),
            Some(&300)
        );
    }

    #[test]
    fn test_metric_key_label_order_independence() {
        // Create a HashSet to verify uniqueness
        let mut unique_keys = HashSet::new();

        // Add keys with the same labels in different orders
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("a", "1"), ("b", "2"), ("c", "3")],
        ));
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("a", "1"), ("c", "3"), ("b", "2")],
        ));
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("b", "2"), ("a", "1"), ("c", "3")],
        ));
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("b", "2"), ("c", "3"), ("a", "1")],
        ));
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("c", "3"), ("a", "1"), ("b", "2")],
        ));
        unique_keys.insert(MetricKey::new(
            "counter",
            &[("c", "3"), ("b", "2"), ("a", "1")],
        ));

        // All permutations should be considered the same key
        assert_eq!(unique_keys.len(), 1);
    }

    #[test]
    fn test_custom_metrics_operations() {
        // Create a metrics instance
        let metrics = TransactionMetrics::new();

        // Set initial value for a custom metric
        metrics.set_custom(
            "api_calls",
            100,
            &[("endpoint", "users"), ("method", "GET")],
        );

        // Get the metrics data and verify the value was set
        let data = metrics.get_metrics_data();
        let key = MetricKey::new("api_calls", &[("endpoint", "users"), ("method", "GET")]);
        assert_eq!(data.custom_metrics.get(&key).copied(), Some(100));

        // Increment the same metric
        metrics.increment_custom("api_calls", 50, &[("endpoint", "users"), ("method", "GET")]);

        // Verify the value was incremented
        let data = metrics.get_metrics_data();
        assert_eq!(data.custom_metrics.get(&key).copied(), Some(150));

        // Set the same metric to a new value (overwrite)
        metrics.set_custom(
            "api_calls",
            200,
            &[("endpoint", "users"), ("method", "GET")],
        );

        // Verify the value was overwritten
        let data = metrics.get_metrics_data();
        assert_eq!(data.custom_metrics.get(&key).copied(), Some(200));

        // Test with different label order
        metrics.increment_custom("api_calls", 75, &[("method", "GET"), ("endpoint", "users")]);

        // Verify the value was incremented regardless of label order
        let data = metrics.get_metrics_data();
        assert_eq!(data.custom_metrics.get(&key).copied(), Some(275));
    }
}
