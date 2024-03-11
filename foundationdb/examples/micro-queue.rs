use core::fmt;
use std::error;

use foundationdb::{tuple::Subspace, Database, FdbBindingError};
use futures::{StreamExt, TryStreamExt};
use rand::{rngs::SmallRng, RngCore, SeedableRng};

/// Clears subspaces of a database.
///
/// # Errors
///
/// If client failed to commit transaction.
async fn clear_subspace<'a>(
    db: &'a Database,
    subspace: &'a Subspace,
) -> Result<(), FdbBindingError> {
    db.run(move |trx, _| async move {
        trx.clear_subspace_range(subspace);
        Ok(())
    })
    .await
}

/// Error returned on attempt to insert an item in a [`MicroQueue`]
/// which length is [`usize::MAX`].
#[derive(Debug)]
pub struct Overflow;

impl fmt::Display for Overflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("queue is too long")
    }
}

impl error::Error for Overflow {}

pub struct MicroQueue {
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
    /// * If client failed to clear `prefix` [`Subspace`]
    pub async fn new(
        db: Database,
        prefix: impl Into<Vec<u8>> + Send + Sync + Unpin,
    ) -> Result<Self, FdbBindingError> {
        let queue = Subspace::from_bytes(prefix);
        clear_subspace(&db, &queue).await?;
        Ok(Self {
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
    ///   stored under `prefix` [`Subspace`].
    /// * If client failed to commit transaction.
    async fn last_index(&self) -> Result<usize, FdbBindingError> {
        self.db
            .run(move |trx, _maybe_committed| async move {
                Ok(trx
                    .get_ranges_keyvalues(self.queue.range().into(), true)
                    .count()
                    .await)
            })
            .await
    }

    /// Add an element to the queue.
    ///
    /// # Errors
    ///
    /// * If client failed to get [`FdbValues`](foundationdb::future::FdbValues)
    ///   stored under `prefix Subspace`].
    /// * If client failed to commit transaction.
    /// * If the capacity of the queue is [`usize::MAX`].
    pub async fn enqueue<'a, 'b>(&'a mut self, value: &'b str) -> Result<(), FdbBindingError> {
        let index = self
            .last_index()
            .await?
            .checked_add(1)
            .ok_or_else(|| FdbBindingError::new_custom_error(Box::new(Overflow)))?;

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
    ///
    /// # Errors
    ///
    /// * Upon failure to convert stream into a collection.
    /// * If client failed to commit transaction.
    async fn first_item(&self) -> Result<Option<(Vec<u8>, Vec<u8>)>, FdbBindingError> {
        self.db
            .run(|trx, _maybe_committed| async move {
                Ok(trx
                    .get_ranges_keyvalues(self.queue.range().into(), true)
                    .map_ok(|fdb_value| (fdb_value.key().to_vec(), fdb_value.value().to_vec()))
                    .try_collect::<Vec<(Vec<u8>, Vec<u8>)>>()
                    .await?
                    .drain(..)
                    .next())
            })
            .await
    }

    /// Remove the top element from the queue.
    ///
    /// # Errors
    ///
    /// * Upon failure to convert stream into a collection.
    /// * If client failed to commit transaction.
    ///
    /// # Panics
    ///
    /// If value is corrupted (invalid UTF-8)
    pub async fn dequeue(&mut self) -> Result<Option<String>, FdbBindingError> {
        match self.first_item().await? {
            None => Ok(None),
            Some((key, value)) => {
                let key = &key;
                let value = std::str::from_utf8(&value).expect("valid UTF-8");
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

const LINE: [&str; 13] = [
    "Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "George", "Harry", "Ian", "Jack", "Liz",
    "Mary", "Nathan",
];

#[tokio::main]
async fn main() -> Result<(), FdbBindingError> {
    // initialize FoundationDB Client API
    let fdb = unsafe {
        // SAFETY: only called once
        foundationdb::boot()
    };

    // attempt connection to FoundationDB
    let db = Database::default()?;

    // create a micro queue in `Q` subspace
    let mut q = MicroQueue::new(db, "Q").await?;

    // push values at the back of the queue
    for value in LINE {
        q.enqueue(value).await?;
    }

    // pop values from the back of the queue
    while let Some(value) = q.dequeue().await? {
        println!("{value}");
    }

    // shutdown the client
    drop(fdb);

    Ok(())
}
