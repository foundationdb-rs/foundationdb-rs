// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

#[macro_use]
extern crate lazy_static;

use std::borrow::Cow;

use futures::pin_mut;
use futures::prelude::*;

use rand::seq::{IteratorRandom, SliceRandom};

use foundationdb as fdb;
use foundationdb::future::FdbValue;
use foundationdb::tuple::{pack, unpack, Subspace};
use foundationdb::{Database, FdbError, RangeOption, TransactError, TransactOption, Transaction};

type Result<T> = std::result::Result<T, Error>;
enum Error {
    Internal(FdbError),
    NoRemainingSeats,
    TooManyClasses,
    NotInClass,
    AlreadyInClass,
}

impl From<FdbError> for Error {
    fn from(err: FdbError) -> Self {
        Self::Internal(err)
    }
}

impl TransactError for Error {
    fn try_into_fdb_error(self) -> std::result::Result<FdbError, Self> {
        match self {
            Self::Internal(err) => Ok(err),
            _ => Err(self),
        }
    }
}

// NOTE: This example does not use a directory.
// It will create keys with prefixes "\u{2}attends\0" and "\u{2}class\0" in your database,
//      and remove any existing keys with those prefixes.

// Data model:
// ("attends", student, class) = ""
// ("class", class_name) = seatsLeft

// Constants controlling the size of the simulation
const NUMBER_OF_STUDENTS: usize = 10;
const OPERATIONS_PER_STUDENT: usize = 10;

// Try changing these to lower values to produce more contention in the transactions.
// For example, if you set both values to 2 and then change the "snapshot" parameter
//   of the first get() method call in signup_trx to true, you can see how the lack of
//   serializable isolation causes the classes to end up with more than 2 students.
const CLASS_COUNT: usize = 10;
const INITIAL_SEATS_PER_CLASS: u32 = 5;

// Generate 1,620 possible class names like '9:00 chem for dummies'
const LEVELS: &[&str] = &[
    "intro",
    "for dummies",
    "remedial",
    "101",
    "201",
    "301",
    "mastery",
    "lab",
    "seminar",
];

const TYPES: &[&str] = &[
    "chem", "bio", "cs", "geometry", "calc", "alg", "film", "music", "art", "dance",
];

const TIMES: &[&str] = &[
    "2:00", "3:00", "4:00", "5:00", "6:00", "7:00", "8:00", "9:00", "10:00", "11:00", "12:00",
    "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00",
];

lazy_static! {
    static ref ALL_CLASSES: Vec<String> = all_classes();
}

fn all_classes() -> Vec<String> {
    let mut class_names: Vec<String> = Vec::new();
    for level in LEVELS {
        for class_type in TYPES {
            for time in TIMES {
                class_names.push(format!("{} {} {}", time, class_type, level));
            }
        }
    }

    class_names
        .into_iter()
        .choose_multiple(&mut rand::thread_rng(), CLASS_COUNT)
}

fn init_classes(trx: &Transaction, all_classes: &[String]) {
    let class_subspace = Subspace::from("class");
    for class in all_classes {
        trx.set(&class_subspace.pack(class), &pack(&INITIAL_SEATS_PER_CLASS));
    }
}

async fn init(db: &Database, all_classes: &[String]) {
    let class_subspace = Subspace::from("class");
    let attends_subspace = Subspace::from("attends");

    let trx = db.create_trx().expect("could not create transaction");
    trx.clear_subspace_range(&attends_subspace);
    trx.clear_subspace_range(&class_subspace);
    init_classes(&trx, all_classes);

    trx.commit().await.expect("failed to initialize data");
}

async fn get_available_classes(db: &Database) -> Vec<String> {
    let class_subspace = Subspace::from("class");

    let trx = db.create_trx().expect("could not create transaction");

    let range = RangeOption::from(&class_subspace);

    let got_range = trx
        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        .get_ranges_keyvalues(range, false)
        .try_collect::<Vec<FdbValue>>()
        .await
        .expect("failed to get classes");
    let mut available_classes = Vec::<String>::new();

    for key_value in got_range {
        let count: i64 = unpack(key_value.value()).expect("failed to decode count");

        if count > 0 {
            let class: String = class_subspace
                .unpack(key_value.key())
                .expect("failed to decode class");
            available_classes.push(class);
        }
    }

    available_classes
}

async fn ditch_trx(trx: &Transaction, student: &str, class: &str) -> Result<bool> {
    let class_subspace = Subspace::from("class");
    let attends_subspace = Subspace::from("attends");

    let attends_key = attends_subspace.pack(&(student, class));

    if trx
        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        .get(&attends_key, false)
        .await
        .expect("get failed")
        .is_none()
    {
        return Ok(false); // student is not in this class
    }

    let class_key = class_subspace.pack(&class);

    let available_seats = trx
        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        .get(&class_key, false)
        .await
        .expect("get failed")
        .expect("class seats were not initialized");
    let available_seats: i64 = unpack::<i64>(&available_seats).expect("failed to decode i64") + 1;

    //println!("{} ditching class: {}", student, class);
    trx.set(&class_key, &pack(&available_seats));
    trx.clear(&attends_key);

    Ok(true)
}

async fn ditch(db: &Database, student: String, class: String) -> Result<bool> {
    db.transact_boxed(
        (student, class),
        move |trx, (student, class)| ditch_trx(trx, student, class).boxed(),
        fdb::TransactOption::default(),
    )
    .await
}

async fn signup_trx(trx: &Transaction, student: &str, class: &str) -> Result<bool> {
    let class_subspace = Subspace::from("class");
    let attends_subspace = Subspace::from("attends");

    let attends_key = attends_subspace.pack(&(student, class));
    if trx
        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        .get(&attends_key, false)
        .await
        .expect("get failed")
        .is_some()
    {
        //println!("{} already taking class: {}", student, class);
        return Ok(false);
    }

    let class_key = class_subspace.pack(&class);
    let available_seats: i64 = unpack(
        &trx
            // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
            .get(&class_key, false)
            .await
            .expect("get failed")
            .expect("class seats were not initialized"),
    )
    .expect("failed to decode i64");

    if available_seats <= 0 {
        return Err(Error::NoRemainingSeats);
    }

    // Use the subspace method to get a new Subspace struct representing the key prefix.
    //    (equivalent to subspace.range(tuple) in the Python API)
    // Although this method is intended for getting nested subspaces, it can be used to generate
    //     a representation of an arbitrary key prefix, whether or not the prefix is semantically
    //     used as a subspace.
    let key_prefix = attends_subspace.subspace(&student);

    let attends_range = RangeOption::from(&key_prefix);
    if trx
        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        .get_ranges_keyvalues(attends_range, false)
        .try_collect::<Vec<FdbValue>>()
        .await
        .expect("get_ranges_keyvalues failed")
        .len()
        >= 5
    {
        return Err(Error::TooManyClasses);
    }

    //println!("{} taking class: {}", student, class);
    trx.set(&class_key, &pack(&(available_seats - 1)));
    trx.set(&attends_key, &pack(&""));

    Ok(true)
}

async fn signup(db: &Database, student: String, class: String) -> Result<bool> {
    db.transact_boxed(
        (student, class),
        |trx, (student, class)| signup_trx(trx, student, class).boxed(),
        TransactOption::default(),
    )
    .await
}

async fn switch_classes(
    db: &Database,
    student_id: String,
    old_class: String,
    new_class: String,
) -> Result<()> {
    async fn switch_classes_body(
        trx: &Transaction,
        student_id: &str,
        old_class: &str,
        new_class: &str,
    ) -> Result<()> {
        if !ditch_trx(trx, student_id, old_class).await? {
            return Err(Error::NotInClass); // cancel the transaction by returning an error
        }

        if signup_trx(trx, student_id, new_class).await? {
            Ok(())
        } else {
            Err(Error::AlreadyInClass) // cancel the transaction by returning an error
        }
    }

    db.transact_boxed(
        (student_id, old_class, new_class),
        move |trx, (student_id, old_class, new_class)| {
            switch_classes_body(trx, student_id, old_class, new_class).boxed()
        },
        TransactOption::default(),
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mood {
    Add,
    Ditch,
    Switch,
}

async fn perform_op(
    db: &Database,
    mood: Mood,
    student_id: &str,
    all_classes: &[String],
    my_classes: &mut Vec<String>,
) -> Result<()> {
    match mood {
        Mood::Add => {
            if !all_classes.is_empty() {
                let class = all_classes.choose(&mut rand::thread_rng()).unwrap();

                if signup(db, student_id.to_string(), class.to_string()).await? {
                    println!("{} signed up for {}", student_id, class);
                    my_classes.push(class.to_string());
                }
            }
        }
        Mood::Ditch => {
            if !my_classes.is_empty() {
                let class = my_classes.choose(&mut rand::thread_rng()).unwrap().clone();

                if ditch(db, student_id.to_string(), class.to_string()).await? {
                    println!("{} dropped {}", student_id, class);
                    my_classes.retain(|s| s != &class);
                }
            }
        }
        Mood::Switch => {
            if !my_classes.is_empty() && !all_classes.is_empty() {
                let old_class = my_classes
                    .choose(&mut rand::thread_rng())
                    .unwrap()
                    .to_string();
                let new_class = all_classes.choose(&mut rand::thread_rng()).unwrap();

                switch_classes(
                    db,
                    student_id.to_string(),
                    old_class.to_string(),
                    new_class.to_string(),
                )
                .await?;

                println!(
                    "{} switched from {} to {}",
                    student_id, old_class, new_class
                );

                my_classes.retain(|s| s != &old_class);
                my_classes.push(new_class.to_string());
            }
        }
    }
    Ok(())
}

async fn simulate_students(student_id: usize, num_ops: usize) {
    let db = Database::new_compat(None)
        .await
        .expect("failed to get database");

    let student_id = format!("s{}", student_id);

    let mut available_classes = Cow::Borrowed(&*ALL_CLASSES);
    let mut my_classes = Vec::<String>::new();

    for _ in 0..num_ops {
        let mut moods = Vec::<Mood>::new();

        if !my_classes.is_empty() {
            moods.push(Mood::Ditch);
            moods.push(Mood::Switch);
        }

        if my_classes.len() < 5 {
            moods.push(Mood::Add);
        }

        let mood = moods.choose(&mut rand::thread_rng()).copied().unwrap();

        // on errors we recheck for available classes
        if perform_op(&db, mood, &student_id, &available_classes, &mut my_classes)
            .await
            .is_err()
        {
            // Transaction failed, likely because available_classes list is incorrect.
            // Update available_classes by querying the database.
            println!("getting available classes");
            available_classes = Cow::Owned(get_available_classes(&db).await);
        }
    }
}

async fn run_sim(db: &Database, students: usize, ops_per_student: usize) {
    let mut threads: Vec<(usize, tokio::task::JoinHandle<()>)> = Vec::with_capacity(students);
    for i in 0..students {
        threads.push((i, tokio::task::spawn(simulate_students(i, ops_per_student))));
    }

    let attends_subspace = Subspace::from("attends");

    // explicitly join the threads by awaiting their JoinHandles...
    for (id, thread) in threads {
        thread.await.expect("failed to join thread");

        let student_id = format!("s{}", id);

        let key_prefix = attends_subspace.subspace(&student_id);

        let attends_range = RangeOption::from(&key_prefix);

        /*
        // Example of using the Stream from get_ranges_keyvalues() directly instead of calling try_collect()...

        let trx = db.create_trx().unwrap();

        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        let stream = trx.get_ranges_keyvalues(attends_range, false);

        pin_mut!(stream);

        while let Some(key_value) = stream.next().await {
            let key_value = key_value.expect("get_ranges_keyvalues failed");

            let (s, class) = attends_subspace.unpack::<(String, String)>(key_value.key()).unwrap();
            assert_eq!(student_id, s);

            println!("{} is taking: {}", student_id, class);
        }
        */

        // Example of using get_ranges(), which returns a Stream of slices of key-value pairs...

        let trx = db.create_trx().unwrap();

        // pass false for the snapshot parameter (otherwise it will not use serializable isolation!)
        let stream = trx.get_ranges(attends_range, false);

        pin_mut!(stream);

        while let Some(next_keyvalues) = stream.next().await {
            for key_value in next_keyvalues.expect("get_ranges failed") {
                let (s, class) = attends_subspace
                    .unpack::<(String, String)>(key_value.key())
                    .unwrap();
                assert_eq!(student_id, s);

                println!("{} is taking: {}", student_id, class);
            }
        }
    }

    println!("Ran {} transactions", students * ops_per_student);
}

#[tokio::main]
async fn main() {
    let _guard = unsafe { fdb::boot() };
    let db = fdb::Database::new_compat(None)
        .await
        .expect("failed to get database");
    init(&db, &ALL_CLASSES).await;
    println!("Initialized");
    run_sim(&db, NUMBER_OF_STUDENTS, OPERATIONS_PER_STUDENT).await;
}
