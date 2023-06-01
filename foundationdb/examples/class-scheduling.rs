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
use std::ops::Deref;
use std::thread;

use futures::prelude::*;
use rand::{rngs::ThreadRng, seq::SliceRandom};

use foundationdb as fdb;
use foundationdb::tuple::{pack, unpack, Subspace};
use foundationdb::{Database, FdbError, RangeOption, TransactError, TransactOption, Transaction};

type Result<T> = std::result::Result<T, Error>;
enum Error {
    Internal(FdbError),
    NoRemainingSeats,
    TooManyClasses,
}

impl From<FdbError> for Error {
    fn from(err: FdbError) -> Self {
        Error::Internal(err)
    }
}

impl TransactError for Error {
    fn try_into_fdb_error(self) -> std::result::Result<FdbError, Self> {
        match self {
            Error::Internal(err) => Ok(err),
            _ => Err(self),
        }
    }
}

// Data model:
// ("attends", student, class) = ""
// ("class", class_name) = seatsLeft

// Generate 1,620 classes like '9:00 chem for dummies'
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

// TODO: make these tuples?
fn all_classes() -> Vec<String> {
    let mut class_names: Vec<String> = Vec::new();
    for level in LEVELS {
        for _type in TYPES {
            for time in TIMES {
                class_names.push(format!("{} {} {}", time, _type, level));
            }
        }
    }

    class_names
}

fn init_classes(trx: &Transaction, all_classes: &[String]) {
    let class_subspace = Subspace::from("class");
    for class in all_classes {
        trx.set(&class_subspace.pack(class), &pack(&100_i64));
    }
}

async fn init(db: &Database, all_classes: &[String]) {
    let trx = db.create_trx().expect("could not create transaction");
    trx.clear_subspace_range(&"attends".into());
    trx.clear_subspace_range(&"class".into());
    init_classes(&trx, all_classes);

    trx.commit().await.expect("failed to initialize data");
}

async fn get_available_classes(db: &Database) -> Vec<String> {
    let trx = db.create_trx().expect("could not create transaction");

    let range = RangeOption::from(&Subspace::from("class"));

    let got_range = trx
        .get_range(&range, 1_024, false)
        .await
        .expect("failed to get classes");
    let mut available_classes = Vec::<String>::new();

    for key_value in got_range.iter() {
        let count: i64 = unpack(key_value.value()).expect("failed to decode count");

        if count > 0 {
            let class: String = unpack(key_value.key()).expect("failed to decode class");
            available_classes.push(class);
        }
    }

    available_classes
}

async fn ditch_trx(trx: &Transaction, student: &str, class: &str) {
    let attends_key = pack(&("attends", student, class));

    // TODO: should get take an &Encode? current impl does encourage &[u8] reuse...
    if trx
        .get(&attends_key, true)
        .await
        .expect("get failed")
        .is_none()
    {
        return;
    }

    let class_key = pack(&("class", class));
    let available_seats = trx
        .get(&class_key, true)
        .await
        .expect("get failed")
        .expect("class seats were not initialized");
    let available_seats: i64 =
        unpack::<i64>(available_seats.deref()).expect("failed to decode i64") + 1;

    //println!("{} ditching class: {}", student, class);
    trx.set(&class_key, &pack(&available_seats));
    trx.clear(&attends_key);
}

async fn ditch(db: &Database, student: String, class: String) -> Result<()> {
    db.transact_boxed_local(
        (student, class),
        move |trx, (student, class)| ditch_trx(trx, student, class).map(|_| Ok(())).boxed_local(),
        fdb::TransactOption::default(),
    )
    .await
}

async fn signup_trx(trx: &Transaction, student: &str, class: &str) -> Result<()> {
    let attends_key = pack(&("attends", student, class));
    if trx
        .get(&attends_key, true)
        .await
        .expect("get failed")
        .is_some()
    {
        //println!("{} already taking class: {}", student, class);
        return Ok(());
    }

    let class_key = pack(&("class", class));
    let available_seats: i64 = unpack(
        &trx.get(&class_key, true)
            .await
            .expect("get failed")
            .expect("class seats were not initialized"),
    )
    .expect("failed to decode i64");

    if available_seats <= 0 {
        return Err(Error::NoRemainingSeats);
    }

    let attends_range = RangeOption::from(&("attends", &student).into());
    if trx
        .get_range(&attends_range, 1_024, false)
        .await
        .expect("get_range failed")
        .len()
        >= 5
    {
        return Err(Error::TooManyClasses);
    }

    //println!("{} taking class: {}", student, class);
    trx.set(&class_key, &pack(&(available_seats - 1)));
    trx.set(&attends_key, &pack(&""));

    Ok(())
}

async fn signup(db: &Database, student: String, class: String) -> Result<()> {
    db.transact_boxed_local(
        (student, class),
        |trx, (student, class)| signup_trx(trx, student, class).boxed_local(),
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
        ditch_trx(trx, student_id, old_class).await;
        signup_trx(trx, student_id, new_class).await?;
        Ok(())
    }

    db.transact_boxed_local(
        (student_id, old_class, new_class),
        move |trx, (student_id, old_class, new_class)| {
            switch_classes_body(trx, student_id, old_class, new_class).boxed_local()
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
    rng: &mut ThreadRng,
    mood: Mood,
    student_id: &str,
    all_classes: &[String],
    my_classes: &mut Vec<String>,
) -> Result<()> {
    match mood {
        Mood::Add => {
            let class = all_classes.choose(rng).unwrap();
            signup(db, student_id.to_string(), class.to_string()).await?;
            my_classes.push(class.to_string());
        }
        Mood::Ditch => {
            let class = all_classes.choose(rng).unwrap();
            ditch(db, student_id.to_string(), class.to_string()).await?;
            my_classes.retain(|s| s != class);
        }
        Mood::Switch => {
            let old_class = my_classes.choose(rng).unwrap().to_string();
            let new_class = all_classes.choose(rng).unwrap();
            switch_classes(
                db,
                student_id.to_string(),
                old_class.to_string(),
                new_class.to_string(),
            )
            .await?;
            my_classes.retain(|s| s != &old_class);
            my_classes.push(new_class.to_string());
        }
    }
    Ok(())
}

async fn simulate_students(student_id: usize, num_ops: usize) {
    let db = Database::new_compat(None)
        .await
        .expect("failed to get database");

    let student_id = format!("s{}", student_id);
    let mut rng = rand::thread_rng();

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

        let mood = moods.choose(&mut rng).copied().unwrap();

        // on errors we recheck for available classes
        if perform_op(
            &db,
            &mut rng,
            mood,
            &student_id,
            &available_classes,
            &mut my_classes,
        )
        .await
        .is_err()
        {
            println!("getting available classes");
            available_classes = Cow::Owned(get_available_classes(&db).await);
        }
    }
}

async fn run_sim(db: &Database, students: usize, ops_per_student: usize) {
    let mut threads: Vec<(usize, thread::JoinHandle<()>)> = Vec::with_capacity(students);
    for i in 0..students {
        // TODO: ClusterInner has a mutable pointer reference, if thread-safe, mark that trait as Sync, then we can clone DB here...
        threads.push((
            i,
            thread::spawn(move || {
                futures::executor::block_on(simulate_students(i, ops_per_student));
            }),
        ));
    }

    // explicitly join...
    for (id, thread) in threads {
        thread.join().expect("failed to join thread");

        let student_id = format!("s{}", id);
        let attends_range = RangeOption::from(&("attends", &student_id).into());

        for key_value in db
            .create_trx()
            .unwrap()
            .get_range(&attends_range, 1_024, false)
            .await
            .expect("get_range failed")
            .iter()
        {
            let (_, s, class) = unpack::<(String, String, String)>(key_value.key()).unwrap();
            assert_eq!(student_id, s);

            println!("{} is taking: {}", student_id, class);
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
    run_sim(&db, 10, 10).await;
}
