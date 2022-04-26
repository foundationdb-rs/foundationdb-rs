use foundationdb::tuple::Subspace;
use foundationdb::{Database, RangeOption};
use ring::digest::{Context, SHA256};
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};

///
/// The goal of this example is to create a record entry for a binary data.
///
/// Because this ones are to big to fit into a single key/value.
///
/// We have to split it as a bunch of keys of for example 1kb each.
///
/// Workflow:
///
/// 0 - Create keyspace and cleanup previous one
/// 1 - Load bytes from image
/// 2 - Compute checksum of input data
/// 3 - Write bytes to database
/// 4 - Read bytes from database
/// 5 - Compute checksum of output data

async fn clear_subspace(db: &Database, subspaces: Vec<&Subspace>) {
    let transaction = db.create_trx().expect("Unable to create transaction");

    for subspace in subspaces {
        transaction.clear_subspace_range(subspace)
    }

    transaction
        .commit()
        .await
        .expect("Unable to commit transaction");
}

fn sha256_hex_digest<R: Read>(mut reader: R) -> String {
    let mut context = Context::new(&SHA256);
    let mut buffer = [0_u8; 1024];

    loop {
        let count = reader.read(&mut buffer).expect("Unable to read buffer");

        if count == 0_usize {
            break;
        }
        context.update(&buffer[..count]);
    }

    let digest = context.finish();

    data_encoding::HEXLOWER.encode(digest.as_ref())
}

fn load_bytes_from_file(path: &str) -> (Vec<u8>, String) {
    let mut file = File::open(path).expect("Unable to open path");
    let mut buffer = [0_u8; 1024];
    let mut data = vec![];

    loop {
        let count = file.read(&mut buffer).expect("Unable to read from file");
        if count == 0_usize {
            break;
        }
        data.extend(&buffer[..count].to_vec())
    }
    file.seek(SeekFrom::Start(0))
        .expect("Unable to rewind file cursor");

    let digest = sha256_hex_digest(&mut file);
    (data, digest)
}

static CHUNK_SIZE: usize = 1024;

/// Writes to database a slice of data chunked with the specified chunk size if any
/// and with the corresponding key name into the given subspace
async fn write_blob(
    db: &Database,
    subspace: &Subspace,
    key_name: &str,
    data: &Vec<u8>,
    chunk_size: Option<usize>,
) {
    if data.len() == 0 {
        return;
    }

    let transaction = db.create_trx().expect("Unable to create transaction");

    let chunk_size = chunk_size.unwrap_or(CHUNK_SIZE);

    let mut i = 0;
    data.chunks(chunk_size).for_each(|chunk| {
        let start = i * chunk_size;
        let end = start + chunk.len();

        let key = subspace.pack(&(key_name, format!("{:06}", start)));
        transaction.set(&key, &data[start..end]);
        i += 1;
    });

    transaction
        .commit()
        .await
        .expect("Unable to commit transaction");
}

/// Gets data from database from the given subspace with specified key name.
async fn read_blob(db: &Database, subspace: &Subspace, key_name: &str) -> Option<Vec<u8>> {
    let transaction = db.create_trx().expect("Unable to create transaction");

    let begin = subspace.pack(&(key_name));
    let mut end = subspace.pack(&(key_name));
    end.pop();
    end.push(255_u8);

    let range = RangeOption::from((begin, end));

    // TODO : verify why iteration < 8 fails the test
    let get_result = transaction.get_range(&range, 8, false).await;

    if let Ok(results) = get_result {
        let mut data: Vec<u8> = vec![];

        for result in results {
            let value = result.value();

            data.extend(value.into_iter());

            // dbg!(String::from_utf8_lossy(&result.key()));
        }

        return Some(data);
    }

    None
}

fn main() {
    let _guard = unsafe { foundationdb::boot() };
    let db =
        futures::executor::block_on(Database::new_compat(None)).expect("Unable to create database");

    // Create the subspace handling image data
    let images_subspace = Subspace::from_bytes("images");

    futures::executor::block_on(clear_subspace(&db, vec![&images_subspace]));
    let file_name = "./assets/logo-400x400.png";

    let (data, digest_input) = load_bytes_from_file(file_name);

    futures::executor::block_on(write_blob(
        &db,
        &images_subspace,
        file_name,
        &data,
        Some(512),
    ));
    let data_from_database =
        futures::executor::block_on(read_blob(&db, &images_subspace, file_name));

    assert_ne!(data_from_database, None);

    let data_from_database = Cursor::new(data_from_database.unwrap());
    let digest_data_from_database = sha256_hex_digest(data_from_database);

    assert_eq!(digest_data_from_database, digest_input);
}
