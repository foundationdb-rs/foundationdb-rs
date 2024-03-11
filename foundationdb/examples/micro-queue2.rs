use core::fmt;
use std::{error, marker::PhantomData};

use foundationdb::{
    api::NetworkAutoStop, future::FdbValue, tuple::Subspace, Database, FdbBindingError,
};
use futures::Future;
use rand::{rngs::SmallRng, RngCore, SeedableRng};

// Clears subspaces of a database.
fn clear_subspace<'a>(
    db: &'a Database,
    subspace: &'a Subspace,
) -> impl Future<Output = Result<(), FdbBindingError>> + 'a {
    db.run(move |trx, _| async move {
        trx.clear_subspace_range(subspace);
        Ok(())
    })
}

#[derive(Debug)]
pub struct CapacityOverflow;

impl fmt::Display for CapacityOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("queue is too long")
    }
}

impl error::Error for CapacityOverflow {}

pub struct MicroQueue {
    _fdb: NetworkAutoStop,
    db: Database,
    queue: Subspace,
    rng: SmallRng,
    rands: [u8; 20],
}

impl MicroQueue {
    /// Creates a new, empty, queue in `prefix` [`Subspace`].
    ///
    /// # Errors
    ///
    /// * If client initialization failed.
    /// * If client failed to create [`Database`]
    /// * If client failed to clear `prefix` [`Subspace`]
    pub async fn new(prefix: impl Into<Vec<u8>>) -> Result<Self, FdbBindingError> {
        let _fdb = unsafe {
            // SAFETY: `NetworkAutoStop` will be dropped when `MicroQueue` goes out of scope
            foundationdb::boot()
        };
        let db = Database::default()?;
        let queue = Subspace::from_bytes(prefix);
        clear_subspace(&db, &queue).await?;
        Ok(Self {
            _fdb,
            db,
            queue,
            rng: SmallRng::from_entropy(),
            rands: [0; 20],
        })
    }

    /// Get the last index in the queue.
    ///
    /// # Errors
    ///
    /// * If client failed to get [`FdbValues`](foundationdb::future::FdbValues)
    ///   from `prefix`[`Subspace`].
    /// * If client failed to commit transaction
    async fn last_index(&self) -> Result<usize, FdbBindingError> {
        self.db
            .run(move |trx, _maybe_committed| async move {
                trx.get_range(&self.queue.range().into(), 1, true)
                    .await
                    .map_err(Into::into)
                    .map(|x| x.len())
            })
            .await
    }

    /// Add an element to the queue.
    ///
    /// # Errors
    ///
    /// * If client failed to get [`FdbValues`](foundationdb::future::FdbValues)
    ///   from `prefix`[`Subspace`].
    /// * If client failed to commit transaction
    /// * If the capacity of the queue is `usize::MAX`.
    pub async fn enqueue<'a, 'b>(&'a mut self, value: &'b str) -> Result<(), FdbBindingError> {
        let index = self
            .last_index()
            .await?
            .checked_add(1)
            .ok_or_else(|| FdbBindingError::new_custom_error(Box::new(CapacityOverflow)))?;

        // Create random seed to avoid conflicts.
        self.rng.fill_bytes(&mut self.rands);

        let key = &self.queue.subspace(&(index, self.rands.as_slice()));

        self.db
            .run(|trx, _maybe_committed| async move {
                trx.set(key.bytes(), value.as_bytes());
                Ok(())
            })
            .await
    }

    /// Get the top element of the queue.
    async fn first_item(&self) -> Result<Option<FdbValue>, FdbBindingError> {
        self.db
            .run(|trx, _maybe_committed| async move {
                trx.get_range(&self.queue.range().into(), 1, true)
                    .await
                    .map_err(Into::into)
                    .map(|x| x.into_iter().next_back())
            })
            .await
    }

    /// Remove the top element from the queue.
    pub async fn dequeue(&mut self) -> Result<Option<String>, FdbBindingError> {
        match self.first_item().await? {
            None => Ok(None),
            Some(fdb_value) => {
                let key = fdb_value.key();
                let value = std::str::from_utf8(fdb_value.value()).expect("valid UTF-8");
                self.db
                    .run(|trx, _maybe_committed| async move {
                        trx.clear(key);
                        Ok(Some(value.to_owned()))
                    })
                    .await
            }
        }
    }
}

const LINE: [&'static str; 13] = [
    "Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "George", "Harry", "Ian", "Jack", "Liz",
    "Mary", "Nathan",
];

#[tokio::main]
async fn main() {
    let mut q = MicroQueue::new("Q")
        .await
        .expect("database creation failed");

    for value in LINE {
        q.enqueue(value).await.expect("transaction failed");
    }

    while let Some(value) = q.dequeue().await.expect("transaction failed") {
        println!("{value}");
    }
}
