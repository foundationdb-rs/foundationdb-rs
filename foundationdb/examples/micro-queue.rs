use foundationdb::{future::FdbValue, tuple::Subspace, Database};
use rand::{rngs::SmallRng, RngCore, SeedableRng};

// Clears subspaces of a database.
pub async fn clear_subspace(db: &Database, subspace: &Subspace) {
    db.run(|trx, _maybe_committed| async move {
        trx.clear_subspace_range(subspace);
        Ok(())
    })
    .await
    .expect("Unable to commit transaction")
}

// Get the last index in the queue.
async fn last_index(db: &Database, queue: &Subspace) -> usize {
    db.run(|trx, _maybe_committed| async move {
        Ok(trx
            .get_range(&queue.range().into(), 1, true)
            .await
            .expect("Failed to get range")
            .len())
    })
    .await
    .expect("Unable to commit transaction")
}

// Add an element to the queue.
pub async fn enqueue(
    db: &Database,
    rng: &mut impl RngCore,
    queue: &Subspace,
    value: impl AsRef<str>,
) {
    let next_index = last_index(db, queue)
        .await
        .checked_add(1)
        .expect("Queue is too long");

    // Create random seed to avoid conflicts.
    let rands = &mut [0; 20];
    rng.fill_bytes(rands);

    let key = &queue.subspace(&(next_index, rands.as_slice()));
    let value = value.as_ref();

    db.run(|trx, _maybe_committed| async move {
        trx.set(key.bytes(), value.as_bytes());
        Ok(())
    })
    .await
    .expect("Unable to commit transaction")
}

// Get the top element of the queue.
async fn first_item(db: &Database, queue: &Subspace) -> Option<FdbValue> {
    db.run(|trx, _maybe_committed| async move {
        Ok(trx
            .get_range(&queue.range().into(), 1, true)
            .await
            .expect("Failed to get range")
            .into_iter()
            .next_back())
    })
    .await
    .expect("Unable to commit transaction")
}

// Remove the top element from the queue.
pub async fn dequeue(db: &Database, queue: &Subspace) -> Option<String> {
    let fdb_value = first_item(db, queue).await?;
    let key = fdb_value.key();
    let value = std::str::from_utf8(fdb_value.value()).expect("valid UTF-8");
    db.run(|trx, _maybe_committed| async move {
        trx.clear(key);
        Ok(Some(value.to_owned()))
    })
    .await
    .expect("Unable to commit transaction")
}

const LINE: [&'static str; 13] = [
    "Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "George", "Harry", "Ian", "Jack", "Liz",
    "Mary", "Nathan",
];

#[tokio::main]
async fn main() {
    let fdb = unsafe { foundationdb::boot() };
    let db = &Database::default().expect("Failed to create database");
    let queue = &Subspace::from_bytes("Q");
    let rng = &mut SmallRng::from_entropy();

    clear_subspace(db, queue).await;

    for value in LINE {
        enqueue(db, rng, queue, value).await;
    }

    while let Some(value) = dequeue(db, queue).await {
        println!("{value}");
    }

    drop(fdb);
}
