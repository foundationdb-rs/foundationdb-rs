use foundationdb::tuple::Subspace;
use foundationdb::{Database, RangeOption};
use rand::distributions::Uniform;
use rand::Rng;

/// The goal of this example is to store blob data as chunk of 1kB
/// Retrieve these data then verify correctness

async fn clear_subspace(db: &Database, subspace: &Subspace) {
    let transaction = db.create_trx().expect("Unable to create transaction");
    transaction.clear_subspace_range(subspace);
    transaction
        .commit()
        .await
        .expect("Unable to commit transaction");
}

static CHUNK_SIZE: usize = 1024;
async fn write_blob(db: &Database, subspace: &Subspace, data: &[u8]) {
    let transaction = db.create_trx().expect("Unable to create transaction");

    let mut chunk_number = 0;
    data.to_vec().chunks(CHUNK_SIZE).for_each(|chunk| {
        let start = chunk_number * CHUNK_SIZE;
        let end = start + chunk.len();

        let key = subspace.pack(&(start));
        transaction.set(&key, &data[start..end]);
        chunk_number += 1;
    });

    transaction
        .commit()
        .await
        .expect("Unable to commit transaction");
}

async fn read_blob(db: &Database, subspace: &Subspace) -> Option<Vec<u8>> {
    let transaction = db.create_trx().expect("Unable to create transaction");

    let range = RangeOption::from(subspace.range());

    let get_result = transaction.get_range(&range, 1_024, false).await;

    if let Ok(results) = get_result {
        let mut data: Vec<u8> = vec![];

        for result in results {
            let value = result.value();
            data.extend(value.iter());
        }

        return Some(data);
    }
    None
}

async fn test_blob_storing(db: &Database, subspace: &Subspace, iteration: usize) {
    // Generate random 10k bytes
    let data: Vec<u8> = rand::thread_rng()
        .sample_iter(Uniform::from(0..=255))
        .take(10000)
        .collect();

    // Create a subspace for iteration
    let subspace = subspace.subspace(&(iteration));

    // Write data as chunk of 1024
    write_blob(db, &subspace, &data).await;

    // Read data
    let result = read_blob(db, &subspace)
        .await
        .expect("Unable to read data from database");

    // Check correctness
    assert_eq!(&data, &result);
}

#[tokio::main]
async fn main() {
    let _guard = unsafe { foundationdb::boot() };
    let db = Database::new_compat(None)
        .await
        .expect("Unable to create database");

    let subspace = Subspace::all().subspace(&("example-blob"));
    clear_subspace(&db, &subspace).await;

    for i in 1..=100 {
        println!("Iteration #{}", i);
        test_blob_storing(&db, &subspace, i).await;
    }
}
