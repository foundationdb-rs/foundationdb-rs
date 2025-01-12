extern crate foundationdb as fdb;
extern crate futures;
extern crate rand;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate structopt;

use std::sync::atomic::*;
use std::sync::Arc;

use foundationdb_bench::Stopwatch;
use futures::future::*;
use rand::prelude::*;
use rand::rngs::mock::StepRng;
use structopt::StructOpt;

use crate::fdb::*;

#[derive(Clone)]
struct Counter {
    size: usize,
    inner: Arc<AtomicUsize>,
}
impl Counter {
    fn new(size: usize) -> Self {
        Self {
            size,
            inner: Default::default(),
        }
    }

    fn decr(&self, n: usize) -> bool {
        let val = self.inner.fetch_add(n, Ordering::SeqCst);
        val < self.size
    }
}

#[derive(Clone)]
struct Bench {
    db: Arc<Database>,
    opt: Opt,
}

impl Bench {
    fn run(self) {
        let opt = &self.opt;
        let counter = Counter::new(opt.count);

        let mut handles = Vec::new();

        let sw = Stopwatch::start_new();

        let step = (opt.queue_depth + opt.threads - 1) / opt.threads;
        let mut start = 0;
        for _ in 0..opt.threads {
            let end = std::cmp::min(start + step, opt.queue_depth);

            let range = start..end;
            let counter = counter.clone();
            let b = self.clone();
            let handle = std::thread::spawn(move || {
                futures::executor::block_on(b.run_range(range, counter))
            });
            handles.push(handle);

            start = end;
        }

        for handle in handles {
            handle
                .join()
                .expect("failed to join")
                .expect("failed to run bench");
        }

        let elapsed = sw.elapsed_ms() as usize;

        info!(
            "bench took: {:?} ms, {:?} tps",
            elapsed,
            1000 * opt.count / elapsed
        );
    }

    async fn run_range(&self, r: std::ops::Range<usize>, counter: Counter) -> FdbResult<()> {
        try_join_all(r.map(|n| {
            // With deterministic Rng, benchmark with same parameters will overwrite same set
            // of keys again, which makes benchmark result stable.
            let rng = StepRng::new(n as u64, 1);
            self.run_bench(rng, counter.clone())
        }))
        .await?;
        Ok(())
    }

    async fn run_bench(&self, mut rng: StepRng, counter: Counter) -> FdbResult<()> {
        let mut key_buf = vec![0; self.opt.key_len];

        let mut val_buf = vec![0; self.opt.val_len];

        let trx_batch_size = self.opt.trx_batch_size;
        let mut trx = self.db.create_trx()?;

        loop {
            for _ in 0..trx_batch_size {
                rng.fill_bytes(&mut key_buf);
                rng.fill_bytes(&mut val_buf);
                key_buf[0] = 0x01;
                trx.set(&key_buf, &val_buf);
            }

            trx = trx.commit().await?.reset();

            if !counter.decr(trx_batch_size) {
                break Ok(());
            }
        }
    }
}

#[derive(StructOpt, Debug, Clone)]
#[structopt(name = "fdb-bench")]
struct Opt {
    #[structopt(short = "t", long = "threads", default_value = "1")]
    threads: usize,

    #[structopt(short = "q", long = "queue-depth", default_value = "1000")]
    queue_depth: usize,

    #[structopt(short = "c", long = "count", default_value = "300000")]
    count: usize,

    #[structopt(long = "trx-batch-size", default_value = "10")]
    trx_batch_size: usize,

    #[structopt(long = "key-len", default_value = "10")]
    key_len: usize,
    #[structopt(long = "val-len", default_value = "100")]
    val_len: usize,
}

fn main() {
    env_logger::init();
    let opt = Opt::from_args();
    info!("opt: {:?}", opt);

    foundationdb::boot().expect("could not boot fdb client");
    let db = Arc::new(
        futures::executor::block_on(fdb::Database::new_compat(None))
            .expect("failed to get database"),
    );

    let bench = Bench { db, opt };
    bench.run();
}
