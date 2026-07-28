use std::time::Duration;

use foundationdb::{
    BudgetExceeded, BudgetKind, ClientBudget, FdbBindingError, FdbResult, Transaction,
    env::Environment, tuple::Subspace,
};
use foundationdb_simulation::{
    Metric, Metrics, RustWorkload, Severity, SimDatabase, SingleRustWorkload, WorkloadContext,
    details, register_workload,
};

// How many rows setup() writes, and the modulus of the scan.
const ROW_COUNT: usize = 500;
// What one attempt may spend scanning, measured with the simulator clock. A
// simulated read costs enough that this trips after a dozen of them, well before
// the five seconds FoundationDB gives a transaction.
const TIME_LIMIT: Duration = Duration::from_secs(1);
// Ends the scan even if the budget never trips, so the workload terminates.
const MAX_ITERATIONS: usize = 100_000;

/// Reads rows one at a time, taking its time from the environment it was given.
/// It sets neither transaction options nor budgets: those belong to the caller.
struct BudgetedScanner {
    env: Environment,
    subspace: Subspace,
}

impl BudgetedScanner {
    fn new(env: Environment, subspace: Subspace) -> Self {
        Self { env, subspace }
    }

    /// The environment this scanner reads time from, which the caller also hands
    /// to the budget so that both agree on what time it is.
    fn env(&self) -> &Environment {
        &self.env
    }

    fn key(&self, index: usize) -> Vec<u8> {
        self.subspace.pack(&(index as i64))
    }

    /// Reads one row, returning how many bytes came back.
    ///
    /// The await is where simulated time passes: the simulator decides the
    /// latency of the storage round trip, and that is what moves the clock the
    /// budget is measured against.
    async fn read(&self, trx: &Transaction, index: usize) -> FdbResult<usize> {
        let value = trx.get(&self.key(index), false).await?;
        Ok(value.map_or(0, |bytes| bytes.len()))
    }
}

pub struct BudgetWorkload {
    context: WorkloadContext,
    client_id: i32,
    scanner: BudgetedScanner,
    // how many rows the scan read before it stopped
    iterations: usize,
    // the limit the scan tripped on, None if it never did
    outcome: Option<BudgetExceeded>,
    // the read error that ended the scan early, if any
    read_error: Option<String>,
    // how much simulated time the scan took
    elapsed: Duration,
}

impl SingleRustWorkload for BudgetWorkload {
    fn new(_name: String, context: WorkloadContext) -> Self {
        // The same struct runs in production with `Environment::default()`, in tests
        // with `Environment::with_seed(..)` and here with the simulator's
        // `context.environment()`: only the environment swaps.
        let scanner = BudgetedScanner::new(context.environment(), Subspace::all());
        Self {
            client_id: context.client_id(),
            context,
            scanner,
            iterations: 0,
            outcome: None,
            read_error: None,
            elapsed: Duration::ZERO,
        }
    }
}

impl RustWorkload for BudgetWorkload {
    async fn setup(&mut self, db: SimDatabase) {
        println!("rust_setup({})", self.client_id);
        // Only use a single client
        if self.client_id == 0 {
            let scanner = &self.scanner;
            db.run(|trx, _maybe_committed| async move {
                for index in 0..ROW_COUNT {
                    trx.set(&scanner.key(index), &(index as i64).to_le_bytes());
                }
                Ok::<_, FdbBindingError>(())
            })
            .await
            .expect("could not write the rows to scan");
        }
    }
    async fn start(&mut self, db: SimDatabase) {
        println!("rust_start({})", self.client_id);
        if self.client_id != 0 {
            return;
        }

        let started_at = self.scanner.env().clock().monotonic();
        let trx = db.create_trx().expect("Could not create transaction");

        // Measuring the attempt with the simulator clock is what makes the moment
        // the budget trips part of the deterministic state of the run. Setting the
        // budget is a caller decision, so it stays here and not in the scanner.
        trx.set_client_budget(ClientBudget {
            time_limit: Some(TIME_LIMIT),
            clock: Some(self.scanner.env().clock().clone()),
            ..ClientBudget::default()
        });

        // One attempt, one long loop: the budget is what ends it.
        for index in 0..MAX_ITERATIONS {
            match self.scanner.read(&trx, index % ROW_COUNT).await {
                Ok(_) => self.iterations += 1,
                Err(err) => {
                    // This workload measures a single attempt, so it does not
                    // retry: a failed read ends the scan and check() reports it.
                    self.read_error = Some(err.to_string());
                    break;
                }
            }

            if let Err(exceeded) = trx.check_client_budget() {
                self.outcome = Some(exceeded);
                break;
            }
        }

        self.elapsed = self.scanner.env().clock().monotonic() - started_at;

        self.context.trace(
            Severity::Info,
            "Budget scan finished",
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Iterations" => self.iterations,
                "Kind" => self.outcome.map_or("none".to_string(), |e| e.kind.to_string()),
                "Used" => self.outcome.map_or(0, |e| e.used),
                "Limit" => self.outcome.map_or(0, |e| e.limit),
                "ElapsedMs" => self.elapsed.as_millis(),
                "ReadError" => self.read_error.clone().unwrap_or_default(),
            ],
        );
    }
    async fn check(&mut self, _db: SimDatabase) {
        println!("rust_check({})", self.client_id);
        if self.client_id != 0 {
            return;
        }

        // A Severity::Error trace fails the simulation, so every way the budget
        // could have failed to do its job is reported that way.
        let (severity, message) = match self.outcome {
            Some(BudgetExceeded { kind, used, limit })
                if kind == BudgetKind::Time && used >= limit =>
            {
                (Severity::Info, "Client budget tripped on time as expected")
            }
            Some(_) => (Severity::Error, "Client budget tripped on the wrong limit"),
            None => (Severity::Error, "Client budget never tripped"),
        };

        self.context.trace(
            severity,
            message,
            details![
                "Layer" => "Rust",
                "Client" => self.client_id,
                "Iterations" => self.iterations,
                "Kind" => self.outcome.map_or("none".to_string(), |e| e.kind.to_string()),
                "Used" => self.outcome.map_or(0, |e| e.used),
                "Limit" => self.outcome.map_or(0, |e| e.limit),
                "ElapsedMs" => self.elapsed.as_millis(),
                "ReadError" => self.read_error.clone().unwrap_or_default(),
            ],
        );
    }
    fn get_metrics(&self, mut out: Metrics) {
        println!("rust_get_metrics({})", self.client_id);
        out.extend([
            Metric::val("iterations", self.iterations as f64),
            Metric::val("used_ms", self.outcome.map_or(0, |e| e.used) as f64),
        ]);
    }
    fn get_check_timeout(&self) -> f64 {
        println!("rust_get_check_timeout({})", self.client_id);
        5000.0
    }
}

register_workload!(BudgetWorkload);
