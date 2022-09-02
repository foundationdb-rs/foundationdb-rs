use foundationdb::tuple::{unpack, Subspace, TuplePack};
use foundationdb::{Database, FdbResult, RangeOption, Transaction};
use std::fmt::{Display, Formatter};

#[derive(Default)]
struct User {
    id: String,
    name: String,
    zipcode: String,
}

impl Display for User {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "id => '{}', zipcode => '{}', name => '{}'",
            self.id, self.zipcode, self.name
        )
    }
}

/// Cleanup any data from previous run
async fn clear_subspaces(db: &Database, subspaces: &Vec<Subspace>) {
    let transaction = db.create_trx().expect("Failed to create transaction");

    // teardown, clear data from previous run
    for subspace in subspaces {
        transaction.clear_subspace_range(subspace);
    }

    transaction
        .commit()
        .await
        .expect("Unable to commit transaction");
}

async fn create_user(
    transaction: &Transaction,
    id: &str,
    zipcode: &str,
    name: &str,
    user_subspace: &Subspace,
    zipcode_index_subspace: &Subspace,
) {
    let key = user_subspace.pack(&(id, zipcode));
    let index = zipcode_index_subspace.pack(&(zipcode, id));

    transaction.set(&key, &name.pack_to_vec());
    transaction.set(&index, &[]);
}

async fn get_user(
    user_subspace: &Subspace,
    transaction: &Transaction,
    user_id: &str,
    zipcode: &str,
) -> FdbResult<User> {
    let key = user_subspace.pack(&(user_id, zipcode));

    let user = transaction
        .get(&key, false)
        .await?
        .expect("Could not found a row");

    let name: String = unpack(&user).expect("Could not unpack");
    Ok(User {
        id: user_id.to_string(),
        zipcode: zipcode.to_string(),
        name,
    })
}

async fn search_user_by_zipcode(
    db: &Database,
    user_subspace: &Subspace,
    zipcode_index: &Subspace,
    zipcode: &str,
) -> Option<Vec<User>> {
    let transaction = db.create_trx().expect("Unable to create transaction");

    // Create scanning zipcode index range
    let begin = zipcode_index.pack(&(zipcode));

    let mut end = zipcode_index.pack(&(zipcode));
    end.pop();
    end.push(255_u8);

    let range = RangeOption::from((begin, end));

    let result_get_index = &transaction.get_range(&range, 1, false).await;

    let mut users = vec![];

    match result_get_index {
        Ok(results) => {
            for result in results {
                let (zipcode, id): (String, String) = zipcode_index
                    .unpack(result.key())
                    .expect("Unable to unpack value from index");

                let result_user = get_user(user_subspace, &transaction, &id, &zipcode).await;

                if let Ok(user) = result_user {
                    users.push(user);
                }
            }

            Some(users)
        }
        Err(_err) => None,
    }
}

#[tokio::main]
async fn main() {
    let _guard = unsafe { foundationdb::boot() };
    let db = Database::new_compat(None)
        .await
        .expect("failed to get database");

    let user_subspace = Subspace::from_bytes("user");
    let zipcode_index_subspace = Subspace::from_bytes("zipcode_index");

    clear_subspaces(
        &db,
        &vec![user_subspace.clone(), zipcode_index_subspace.clone()],
    )
    .await;

    populate_users(&db, &user_subspace, &zipcode_index_subspace)
        .await
        .expect("Unable to populate database");

    let users = search_user_by_zipcode(&db, &user_subspace, &zipcode_index_subspace, "205").await;

    if let Some(users) = users {
        users.iter().for_each(|user| println!("{}", user));
    }
}

async fn populate_users(
    db: &Database,
    user_subspace: &Subspace,
    zipcode_index_subspace: &Subspace,
) -> FdbResult<()> {
    let transaction = db.create_trx().expect("Failed to create transaction");

    create_user(
        &transaction,
        "001",
        "20500",
        "Barack",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "002",
        "20500",
        "Michelle",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "003",
        "20500",
        "Sasha",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "004",
        "20500",
        "Malia",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "005",
        "20500",
        "Bo",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "101",
        "SW1A 1AA",
        "Elizabeth",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;
    create_user(
        &transaction,
        "102",
        "SW1A 1AA",
        "Philip",
        user_subspace,
        zipcode_index_subspace,
    )
    .await;

    transaction.commit().await?;
    Ok(())
}
