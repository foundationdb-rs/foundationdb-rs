#[macro_use]
extern crate log;

use foundationdb as fdb;

mod directory;
mod instr;
mod stack;
mod trx_state;
mod unit_tests;

use directory::DirectoryStackItem;
use instr::{Instr, InstrCode, mutation_from_str, streaming_from_value};
use stack::{StackFuture, StackItem, StackResult};
use trx_state::TransactionState;

use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::Write;
use std::sync::Arc;
use std::thread;

use fdb::options::{ConflictRangeType, TransactionOption};
use fdb::tuple::{Bytes, Element, Subspace, TuplePack, pack, pack_into, unpack};
use fdb::*;
use futures::future;
use futures::prelude::*;
use num_bigint::BigInt;

pub(crate) static WAITED_FOR_EMPTY: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"WAITED_FOR_EMPTY")));
pub(crate) static RESULT_NOT_PRESENT: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"RESULT_NOT_PRESENT")));
pub(crate) static GOT_READ_VERSION: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"GOT_READ_VERSION")));
pub(crate) static GOT_COMMITTED_VERSION: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"GOT_COMMITTED_VERSION")));

pub(crate) static ERROR_NONE: Element = Element::Bytes(Bytes(Cow::Borrowed(b"ERROR: NONE")));
pub(crate) static ERROR_MULTIPLE: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"ERROR: MULTIPLE")));
pub(crate) static OK: Element = Element::Bytes(Bytes(Cow::Borrowed(b"OK")));
pub(crate) static ERROR_DIRECTORY: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"DIRECTORY_ERROR")));
pub(crate) static ESTIMATE_RANGE_RESPONSE: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"GOT_ESTIMATED_RANGE_SIZE")));
pub(crate) static GOT_APPROXIMATE_SIZE_RESPONSE: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"GOT_APPROXIMATE_SIZE")));
pub(crate) static GET_RANGE_SPLIT_POINTS_RESPONSE: Element =
    Element::Bytes(Bytes(Cow::Borrowed(b"GOT_RANGE_SPLIT_POINTS")));

use foundationdb::directory::DirectoryLayer;

use tuple::VersionstampOffset;

pub(crate) fn range(prefix: Bytes) -> (Bytes<'static>, Bytes<'static>) {
    let begin = prefix.clone().into_owned();
    let end = strinc(prefix).into_owned();

    (begin.into(), end.into())
}

pub(crate) struct StackMachine {
    prefix: Bytes<'static>,

    // The transaction map, from byte string names to transactions.
    //
    // Deviation from the spec: the spec says this map is global and shared by all tester
    // threads. Here each StackMachine, so each tester thread (START_THREAD), has its own map. A
    // thread cannot see a transaction another thread created. The tests only reuse names
    // across threads in directory_hca (tr0, tr1, tr2): there, each of our threads commits its
    // own transactions, where the Python tester shares them between threads.
    transactions: HashMap<Bytes<'static>, TransactionState>,

    // A stack of data items of mixed types and their associated metadata. At a minimum, each item
    // should be stored with the 0-based instruction number which resulted in it being put onto the
    // stack. Your stack must support push and pop operations. It may be helpful if it supports
    // random access, clear and a peek operation. The stack is initialized to be empty.
    stack: Vec<StackItem>,

    // A current FDB transaction name (stored as a byte string). The transaction name should be
    // initialized to the prefix that instructions are being read from.
    cur_transaction: Bytes<'static>,

    // A last seen FDB version, which is a 64-bit integer.
    last_version: i64,

    threads: Vec<thread::JoinHandle<()>>,

    trx_counter: usize,

    directory_stack: Vec<DirectoryStackItem>,
    directory_index: usize,
    error_index: usize,
}

/// Every flag of an instruction must have been consumed by the code that ran it.
pub(crate) fn check_flags(instr: &Instr) {
    if instr.has_flags() {
        panic!("flag not handled for instr: {instr:?}");
    }
}

pub(crate) fn strinc(key: Bytes) -> Bytes {
    let mut key = key.into_owned();
    for i in (0..key.len()).rev() {
        if key[i] != 0xff {
            key[i] += 1;
            return Bytes::from(key);
        } else {
            // stripping key from trailing 0xFF bytes
            key.remove(i);
        }
    }
    panic!("failed to strinc");
}

impl StackMachine {
    fn new(db: &Database, prefix: Bytes<'static>) -> Self {
        let cur_transaction = prefix.clone();
        let mut transactions = HashMap::new();
        transactions.insert(
            cur_transaction.clone(),
            TransactionState::Transaction(db.create_trx().unwrap()),
        );

        Self {
            prefix,
            transactions,
            stack: Vec::new(),
            cur_transaction,
            last_version: 0,
            threads: Vec::new(),
            trx_counter: 0,

            directory_stack: vec![DirectoryStackItem::Directory(DirectoryLayer::default())],
            directory_index: 0,
            error_index: 0,
        }
    }

    fn next_trx_id(&mut self) -> usize {
        self.trx_counter += 1;
        self.trx_counter
    }

    async fn fetch_instr(&self, trx: &Transaction) -> FdbResult<Vec<Instr>> {
        let opt = RangeOption::from(&Subspace::from(&self.prefix));
        debug!("opt = {opt:?}");
        let instrs = Vec::new();
        trx.get_ranges(opt, false)
            .try_fold(instrs, |mut instrs, res| {
                for kv in res.iter() {
                    let instr = Instr::from(kv.value());
                    instrs.push(instr);
                }
                future::ok(instrs)
            })
            .await
    }

    async fn pop(&mut self) -> StackItem {
        let mut item = self.stack.pop().expect("stack empty");
        if let Some((name, state)) = item.await_fut().await {
            self.transactions.insert(name, state);
        }
        item
    }

    async fn maybe_pop(&mut self) -> Option<StackItem> {
        let mut item = self.stack.pop()?;
        if let Some((name, state)) = item.await_fut().await {
            self.transactions.insert(name, state);
        }
        Some(item)
    }

    async fn pop_i64(&mut self) -> i64 {
        let element = self.pop_element().await;
        match element {
            Element::Int(v) => v,
            _ => panic!("i64 was expected, found {element:?}"),
        }
    }

    async fn pop_i32(&mut self) -> i32 {
        i32::try_from(self.pop_i64().await).expect("i64 to fit in i32")
    }

    async fn pop_usize(&mut self) -> usize {
        usize::try_from(self.pop_i64().await).expect("i64 to fit in usize")
    }

    async fn pop_str(&mut self) -> String {
        let element = self.pop_element().await;
        match element {
            Element::String(v) => v.into_owned(),
            _ => panic!("string was expected, found {element:?}"),
        }
    }

    async fn pop_bytes(&mut self) -> Bytes<'static> {
        let element = self.pop_element().await;
        match element {
            Element::Bytes(v) => v,
            // checking nested bytes in a Tuple as well
            Element::Tuple(elements) => match elements.first() {
                Some(Element::Bytes(b)) => b.clone(),
                _ => panic!("bytes were expected, found a Tuple with no Bytes in it"),
            },
            _ => panic!("bytes were expected, found {element:?}"),
        }
    }

    pub(crate) async fn pop_optional_bytes(&mut self) -> Option<Vec<u8>> {
        let element = self.pop_element().await;
        match element {
            Element::Bytes(v) => Some(v.to_vec()),
            _ => None,
        }
    }

    async fn pop_string_tuple(&mut self, count: usize) -> Vec<Vec<String>> {
        let mut result = vec![];

        if count == 0 {
            result.push(vec![]);
        } else {
            for _i in 0..count {
                let mut sub_result = vec![];
                let vec_size = self.pop_i64().await;
                for _j in 0..vec_size {
                    sub_result.push(self.pop_str().await);
                }
                result.push(sub_result);
            }
        }
        result
    }

    async fn pop_element(&mut self) -> Element<'static> {
        let item = self.pop().await;
        if let Some(data) = item.data {
            return data;
        }
        panic!("no data");
    }

    async fn pop_selector(&mut self) -> KeySelector<'static> {
        let key: Bytes = self.pop_bytes().await;
        let or_equal: i32 = i32::try_from(self.pop_i64().await).unwrap();
        let offset: i32 = i32::try_from(self.pop_i64().await).unwrap();

        KeySelector::new(key.0, or_equal != 0, offset)
    }

    fn push(&mut self, number: usize, data: Element<'static>) {
        self.stack.push(StackItem {
            number,
            data: Some(data),
            fut: None,
        });
    }

    fn push_fut(&mut self, number: usize, id: usize, fut: StackFuture) {
        let item = StackItem {
            number,
            data: None,
            fut: Some((id, fut)),
        };
        self.stack.push(item);
    }

    fn push_res(&mut self, number: usize, res: FdbResult<()>, ok_str: &[u8]) {
        match res {
            Ok(..) => self.push(number, Element::Bytes(ok_str.to_vec().into())),
            Err(err) => self.push_err(number, err),
        }
    }

    fn push_directory_err(&mut self, code: &InstrCode, number: usize, err: impl std::fmt::Debug) {
        debug!("[{number}] DIRECTORY_ERROR during {code:?}: {err:?}");
        self.push(number, Element::Tuple(vec![ERROR_DIRECTORY.clone()]));

        if matches!(
            code,
            InstrCode::DirectoryCreateSubspace
                | InstrCode::DirectoryCreateOrOpen
                | InstrCode::DirectoryCreateLayer
                | InstrCode::DirectoryCreate
                | InstrCode::DirectoryOpen
                | InstrCode::DirectoryMove
                | InstrCode::DirectoryMoveTo
                | InstrCode::DirectoryOpenSubspace
        ) {
            debug!(
                "pushed NULL in the directory_stack at index {} because of the error",
                self.directory_stack.len()
            );
            self.directory_stack.push(DirectoryStackItem::Null);
        }
    }

    fn push_err(&mut self, number: usize, err: FdbError) {
        trace!("ERROR {err:?}");
        let packed = pack(&Element::Tuple(vec![
            Element::Bytes(Bytes::from(b"ERROR".as_ref())),
            Element::Bytes(Bytes::from(format!("{}", err.code()).into_bytes())),
        ]));
        self.push(number, Element::Bytes(packed.into()));
    }

    fn check<T>(&mut self, number: usize, r: FdbResult<T>) -> Result<T, ()> {
        match r {
            Ok(v) => Ok(v),
            Err(err) => {
                self.push_err(number, err);
                Err(())
            }
        }
    }

    #[allow(clippy::cognitive_complexity)]
    async fn run_step(
        &mut self,
        db: Arc<Database>,
        number: usize,
        mut instr: Instr,
    ) -> Result<(), ()> {
        let is_db = instr.pop_database();

        let (mut trx, trx_name) = if is_db {
            (
                TransactionState::Transaction(self.check(number, db.create_trx())?),
                None,
            )
        } else {
            let mut trx = self
                .transactions
                .remove(&self.cur_transaction) // some instr requires transaction ownership
                .expect("failed to find trx");
            if let TransactionState::Pending(id) = trx {
                let stack_item = self.stack.iter_mut().find(|s| match s.fut {
                    Some((trx_id, ..)) => trx_id == id,
                    _ => false,
                });
                if let Some(stack_item) = stack_item
                    && let Some((name, state)) = stack_item.await_fut().await
                {
                    assert_eq!(name, self.cur_transaction);
                    trx = state;
                }
            }
            (trx, Some(self.cur_transaction.clone()))
        };

        let result = self
            .run_instr(db, number, instr, is_db, &mut trx, trx_name)
            .await;

        // Every instruction borrows the current transaction from the map. Put it back
        // whatever the outcome, otherwise an instruction that reports its error on the
        // stack would leave the next one without a transaction.
        if !is_db && !self.transactions.contains_key(&self.cur_transaction) {
            self.transactions.insert(self.cur_transaction.clone(), trx);
        }

        result
    }

    /// Runs one instruction on `trx`, the transaction [`Self::run_step`] borrowed.
    #[allow(clippy::cognitive_complexity, clippy::too_many_arguments)]
    async fn run_instr(
        &mut self,
        db: Arc<Database>,
        number: usize,
        mut instr: Instr,
        is_db: bool,
        trx: &mut TransactionState,
        trx_name: Option<Bytes<'static>>,
    ) -> Result<(), ()> {
        use crate::InstrCode::*;

        if instr.code.is_directory() {
            self.run_directory_instr(db, number, &mut instr, is_db, trx)
                .await;
            check_flags(&instr);
            return Ok(());
        }

        let mut mutation = false;
        let mut pending = false;

        match instr.code {
            // Pushes the provided item onto the stack.
            Push(ref element) => {
                debug!("push {element:?}");
                self.push(number, element.clone())
            }
            // Duplicates the top item on the stack. The instruction number for
            // the duplicate item should be the same as the original.
            Dup => {
                let top = self.pop().await;
                debug!("dup {top:?}");
                self.stack.push(top.clone());
                self.stack.push(top);
            }
            // Discards all items in the stack.
            EmptyStack => {
                debug!("empty_stack");
                self.stack.clear()
            }
            // Pops the top item off of the stack as INDEX.
            // Swaps the items in the stack at depth 0 and depth INDEX.
            // Does not modify the instruction numbers of the swapped items.
            Swap => {
                let depth: usize = self.pop_usize().await;
                assert!(
                    depth < self.stack.len(),
                    "swap index invalid {} >= {}",
                    depth,
                    self.stack.len()
                );
                let depth_0 = self.stack.len() - 1;
                let depth = depth_0 - depth;
                debug!("swap {depth_0} {depth}");
                self.stack.swap(depth_0, depth);
            }
            // Pops and discards the top item on the stack.
            Pop => {
                debug!("pop");
                self.pop().await;
            }
            // Pops the top two items off of the stack as A and B and then
            // pushes the difference (A-B) onto the stack.
            // A and B may be assumed to be integers.
            Sub => {
                let a = self.pop_element().await;
                let b = self.pop_element().await;
                debug!("sub {a:?} - {b:?}");
                let c = match (a, b) {
                    // checking for possible overflow
                    (Element::Int(a), Element::Int(b)) => match a.checked_sub(b) {
                        None => Element::BigInt(BigInt::from(a) - BigInt::from(b)),
                        Some(c) => Element::Int(c),
                    },
                    (Element::Float(a), Element::Float(b)) => Element::Float(a - b),
                    (Element::Double(a), Element::Double(b)) => Element::Double(a - b),
                    (Element::BigInt(a), Element::BigInt(b)) => Element::BigInt(a - b),
                    (Element::Int(a), Element::BigInt(b)) => Element::BigInt(a - b),
                    (Element::BigInt(a), Element::Int(b)) => Element::BigInt(a - b),
                    (a, b) => panic!("sub between invalid elements {a:?} {b:?}"),
                };
                self.push(number, c);
            }
            // Pops the top two items off the stack as A and B and then pushes
            // the concatenation of A and B onto the stack. A and B can be
            // assumed to be of the same type and will be either byte strings or
            // unicode strings.
            Concat => {
                let a = self.pop_element().await;
                let b = self.pop_element().await;
                debug!("concat {a:?} {b:?}");
                let c = match (a, b) {
                    (Element::String(a), Element::String(b)) => {
                        Element::String(format!("{a}{b}").into())
                    }
                    (Element::Bytes(a), Element::Bytes(b)) => {
                        let mut bytes = Vec::new();
                        bytes.extend_from_slice(&a);
                        bytes.extend_from_slice(&b);
                        Element::Bytes(bytes.into())
                    }
                    (a, b) => panic!("concat between invalid elements {a:?} {b:?}"),
                };
                self.push(number, c);
            }
            // Pops the top item off the stack as PREFIX. Using a new
            // transaction with normal retry logic, inserts a key-value pair
            // into the database for each item in the stack of the form:
            //
            // PREFIX + tuple.pack((stackIndex, instructionNumber)) = tuple.pack((item,))
            //
            // where stackIndex is the current index of the item in the stack.
            // The oldest item in the stack should have stackIndex 0.
            //
            // If the byte string created by tuple packing the item exceeds 40000 bytes,
            // then the value should be truncated to the first 40000 bytes of the packed
            // tuple.
            //
            // When finished, the stack should be empty. Note that because the stack may be
            // large, it may be necessary to commit the transaction every so often (e.g.
            // after every 100 sets) to avoid past_version errors.
            LogStack => {
                let prefix: Bytes = self.pop_bytes().await;
                debug!("log_stack {prefix:?}");
                let mut entries = Vec::with_capacity(self.stack.len());
                let mut stack_idx = self.stack.len();
                while let Some(stack_item) = self.maybe_pop().await {
                    stack_idx -= 1;
                    let mut key = prefix.clone().into_owned();
                    stack_idx.pack_into_vec(&mut key);
                    stack_item.number.pack_into_vec(&mut key);

                    let mut value = pack(&stack_item.data.unwrap());
                    value.truncate(40000);
                    entries.push((key, value));
                }
                // Like the Python reference, write in batches of 100 on fresh transactions,
                // and leave the current transaction alone.
                for batch in entries.chunks(100) {
                    db.run(|trx, _| async move {
                        for (key, value) in batch {
                            trx.set(key, value);
                        }
                        Ok::<_, fdb::FdbBindingError>(())
                    })
                    .await
                    .expect("LOG_STACK failed to write the stack");
                }
            }

            // Creates a new transaction and stores it in the global transaction map
            // under the currently used transaction name.
            NewTransaction => {
                let name = self.cur_transaction.clone();
                debug!("create_trx {name:?}");

                let trx = self.check(number, db.create_trx())?;
                trx.set_option(fdb::options::TransactionOption::DebugTransactionIdentifier(
                    "RUST".to_string(),
                ))
                .unwrap();
                trx.set_option(fdb::options::TransactionOption::LogTransaction)
                    .unwrap();
                self.transactions
                    .insert(name, TransactionState::Transaction(trx));
            }

            // Pop the top item off of the stack as TRANSACTION_NAME. Begin using the
            // transaction stored at TRANSACTION_NAME in the transaction map for future
            // operations. If no entry exists in the map for the given name, a new
            // transaction should be inserted.
            UseTransaction => {
                let name: Bytes = self.pop_bytes().await;
                debug!("use_transaction {name:?}");
                if !is_db {
                    // Popping the name may have resolved a pending future and restored the
                    // current transaction already. Keep that newer state if it exists.
                    self.transactions
                        .entry(self.cur_transaction.clone())
                        .or_insert_with(|| std::mem::replace(trx, TransactionState::Dead));
                }
                if !self.transactions.contains_key(&name) {
                    let trx = self.check(number, db.create_trx())?;
                    self.transactions
                        .insert(name.clone(), TransactionState::Transaction(trx));
                }
                self.cur_transaction = name;
            }
            // Pops the top item off of the stack as ERROR_CODE. Passes ERROR_CODE in a
            // language-appropriate way to the on_error method of current transaction
            // object and blocks on the future. If on_error re-raises the error, bubbles
            // the error out as indicated above. May optionally push a future onto the
            // stack.
            OnError => {
                let trx_name = trx_name.clone();
                let error_code: i32 = self.pop_i32().await;
                let error = FdbError::from_code(error_code);
                debug!("on_error {error:?}");
                let trx_id = self.next_trx_id();
                let f = trx
                    .take(trx_id)
                    .on_error_with_transaction(error)
                    .map(|(trx, result)| StackResult {
                        state: trx_name.map(|n| (n, TransactionState::Transaction(trx))),
                        data: result.map(|()| RESULT_NOT_PRESENT.clone().into_owned()),
                    })
                    .boxed_local();
                self.push_fut(number, trx_id, f);
                pending = true;
            }

            // Pops the top item off of the stack as KEY and then looks up KEY in the
            // database using the get() method. May optionally push a future onto the
            // stack.
            Get => {
                let key: Bytes = self.pop_bytes().await;
                debug!("get {key:?}");
                let f = trx
                    .as_mut()
                    .get(&key, instr.pop_snapshot())
                    .map_ok(|v| match v {
                        Some(v) => Element::Bytes(v.to_vec().into()),
                        None => RESULT_NOT_PRESENT.clone().into_owned(),
                    })
                    .map(StackResult::from)
                    .boxed_local();
                self.push_fut(number, 0, f);
                pending = true;
            }

            // Pops the top four items off of the stack as KEY, OR_EQUAL, OFFSET, PREFIX
            // and then constructs a key selector. This key selector is then resolved
            // using the get_key() method to yield RESULT. If RESULT starts with PREFIX,
            // then RESULT is pushed onto the stack. Otherwise, if RESULT < PREFIX, PREFIX
            // is pushed onto the stack. If RESULT > PREFIX, then strinc(PREFIX) is pushed
            // onto the stack. May optionally push a future onto the stack.
            GetKey => {
                let selector = self.pop_selector().await;
                let prefix: Bytes = self.pop_bytes().await;
                debug!("get_key {selector:?}, prefix = {prefix:?}");

                let f = trx
                    .as_mut()
                    .get_key(&selector, instr.pop_snapshot())
                    .map_ok(|key| {
                        let key = Bytes::from(key.as_ref());
                        let bytes = if key.starts_with(&prefix) {
                            key
                        } else if key < prefix {
                            prefix
                        } else {
                            assert!(key > prefix);
                            strinc(prefix)
                        };
                        Element::Bytes(bytes.to_vec().into())
                    })
                    .map(StackResult::from)
                    .boxed_local();
                self.push_fut(number, 0, f);
                pending = true;
            }

            // Pops the top five items off of the stack as BEGIN_KEY, END_KEY, LIMIT,
            // REVERSE and STREAMING_MODE. Performs a range read in a language-appropriate
            // way using these parameters. The resulting range of n key-value pairs are
            // packed into a tuple as [k1,v1,k2,v2,...,kn,vn], and this single packed value
            // is pushed onto the stack.
            //
            // _STARTS_WITH:
            //
            // Pops the top four items off of the stack as PREFIX, LIMIT, REVERSE and
            // STREAMING_MODE. Performs a prefix range read in a language-appropriate way
            // using these parameters. Output is pushed onto the stack as with GET_RANGE.
            //
            // _SELECTOR:
            //
            // Pops the top ten items off of the stack as BEGIN_KEY, BEGIN_OR_EQUAL,
            // BEGIN_OFFSET, END_KEY, END_OR_EQUAL, END_OFFSET, LIMIT, REVERSE,
            // STREAMING_MODE, and PREFIX. Constructs key selectors BEGIN and END from
            // the first six parameters, and then performs a range read in a language-
            // appropriate way using BEGIN, END, LIMIT, REVERSE and STREAMING_MODE. Output
            // is pushed onto the stack as with GET_RANGE, excluding any keys that do not
            // begin with PREFIX.
            GetRange => {
                let selector = instr.pop_selector();
                let starts_with = instr.pop_starts_with();
                let snapshot = instr.pop_snapshot();

                let (begin, end) = if starts_with {
                    let begin: Bytes = self.pop_bytes().await;
                    let end = strinc(begin.clone());
                    (
                        KeySelector::first_greater_or_equal(begin.0),
                        KeySelector::first_greater_or_equal(end.0),
                    )
                } else if selector {
                    let begin = self.pop_selector().await;
                    let end = self.pop_selector().await;
                    (begin, end)
                } else {
                    let begin: Bytes = self.pop_bytes().await;
                    let end: Bytes = self.pop_bytes().await;
                    (
                        KeySelector::first_greater_or_equal(begin.0),
                        KeySelector::first_greater_or_equal(end.0),
                    )
                };

                let limit: i64 = self.pop_i64().await;
                let reverse: i64 = self.pop_i64().await;
                let streaming_mode: i32 = self.pop_i32().await;
                let mode = streaming_from_value(streaming_mode);
                debug!(
                    "get_range begin={:?}\n, begin={:?}\n, end={:?}\n,end={:?}\n, limit={:?}, rev={:?}, mode={:?}",
                    begin,
                    unpack::<Element>(begin.key()),
                    end,
                    unpack::<Element>(end.key()),
                    limit,
                    reverse,
                    mode
                );

                let prefix: Option<Bytes> = if selector {
                    Some(self.pop_bytes().await)
                } else {
                    None
                };
                let opt = RangeOption {
                    begin,
                    end,
                    mode,
                    limit: if limit > 0 {
                        Some(limit as usize)
                    } else {
                        None
                    },
                    reverse: reverse != 0,
                    ..RangeOption::default()
                };

                let res = trx
                    .as_mut()
                    .get_ranges(opt, snapshot)
                    .try_fold(Vec::new(), move |mut out, kvs| {
                        for kv in kvs.iter() {
                            let key = kv.key();
                            let value = kv.value();
                            debug!(" - {:?} {:?}", Bytes::from(key), Bytes::from(value));
                            if let Some(ref prefix) = prefix
                                && !key.starts_with(prefix)
                            {
                                continue;
                            }
                            pack_into(&Bytes::from(key), &mut out);
                            pack_into(&Bytes::from(value), &mut out);
                        }
                        future::ok(out)
                    })
                    .map_ok(|data| Element::Bytes(data.into()))
                    .await;
                match res {
                    Err(err) => self.push_err(number, err),
                    Ok(element) => self.push(number, element),
                }
            }

            // Gets the current read version and stores it in the internal stack machine
            // state as the last seen version. Pushed the string "GOT_READ_VERSION" onto
            // the stack.
            GetReadVersion => {
                let _snapshot = instr.pop_snapshot();
                let version = trx.as_mut().get_read_version().await;
                match version {
                    Ok(version) => {
                        self.last_version = version;
                        self.push(number, GOT_READ_VERSION.clone().into_owned());
                    }
                    Err(err) => self.push_err(number, err),
                }
            }

            // Calls get_versionstamp and pushes the resulting future onto the stack.
            GetVersionstamp => {
                let f = trx
                    .as_mut()
                    .get_versionstamp()
                    .map_ok(|v| Element::Bytes(v.to_vec().into()))
                    .map(StackResult::from)
                    .boxed_local();
                self.push_fut(number, 0, f);
                pending = true;
            }

            // Pops the top two items off of the stack as KEY and VALUE. Sets KEY to have
            // the value VALUE. A SET_DATABASE call may optionally push a future onto the
            // stack.
            Set => {
                let key: Bytes = self.pop_bytes().await;
                let value: Bytes = self.pop_bytes().await;
                debug!("set {key:?} {value:?}");
                trx.as_mut().set(&key, &value);
                mutation = true;
            }

            // Sets the current transaction read version to the internal state machine last
            // seen version.
            SetReadVersion => {
                debug!("set_read_version {:?}", self.last_version);
                trx.as_mut().set_read_version(self.last_version);
            }

            // Pops the top item off of the stack as KEY and then clears KEY from the
            // database. A CLEAR_DATABASE call may optionally push a future onto the stack.
            Clear => {
                let key: Bytes = self.pop_bytes().await;
                debug!("clear {key:?}");
                trx.as_mut().clear(&key);
                mutation = true;
            }

            // CLEAR_RANGE
            // Pops the top two items off of the stack as BEGIN_KEY and END_KEY. Clears the
            // range of keys from BEGIN_KEY to END_KEY in the database. A
            // CLEAR_RANGE_DATABASE call may optionally push a future onto the stack.
            //
            // CLEAR_RANGE_STARTS_WITH
            // Pops the top item off of the stack as PREFIX and then clears all keys from
            // the database that begin with PREFIX. A CLEAR_RANGE_STARTS_WITH_DATABASE call
            // may optionally push a future onto the stack.
            ClearRange => {
                let starts_with = instr.pop_starts_with();
                let (begin, end) = if starts_with {
                    let prefix = self.pop_bytes().await;
                    range(prefix)
                } else {
                    let begin: Bytes = self.pop_bytes().await;
                    let end: Bytes = self.pop_bytes().await;
                    (begin, end)
                };
                debug!("clear_range {begin:?} {end:?}");
                trx.as_mut().clear_range(&begin, &end);
                mutation = true;
            }

            // Pops the top three items off of the stack as OPTYPE, KEY, and VALUE.
            // Performs the atomic operation described by OPTYPE upon KEY with VALUE. An
            // ATOMIC_OP_DATABASE call may optionally push a future onto the stack.
            AtomicOp => {
                let optype: String = self.pop_str().await;
                let key: Bytes = self.pop_bytes().await;
                let value: Bytes = self.pop_bytes().await;
                debug!("atomic_op {key:?} {value:?} {optype:?}");

                let op = mutation_from_str(&optype);
                trx.as_mut().atomic_op(&key, &value, op);
                mutation = true;
            }

            // Pops the top two items off of the stack as BEGIN_KEY and END_KEY. Adds a
            // read conflict range or write conflict range from BEGIN_KEY to END_KEY.
            // Pushes the byte string "SET_CONFLICT_RANGE" onto the stack.
            ReadConflictRange => {
                let begin: Bytes = self.pop_bytes().await;
                let end: Bytes = self.pop_bytes().await;
                debug!("read_conflict_range {begin:?} {end:?}");
                self.push_res(
                    number,
                    trx.as_mut()
                        .add_conflict_range(&begin, &end, ConflictRangeType::Read),
                    b"SET_CONFLICT_RANGE",
                );
            }
            WriteConflictRange => {
                let begin: Bytes = self.pop_bytes().await;
                let end: Bytes = self.pop_bytes().await;
                debug!("write_conflict_range {begin:?} {end:?}");
                self.push_res(
                    number,
                    trx.as_mut()
                        .add_conflict_range(&begin, &end, ConflictRangeType::Write),
                    b"SET_CONFLICT_RANGE",
                );
            }
            // Pops the top item off of the stack as KEY. Adds KEY as a read conflict key
            // or write conflict key. Pushes the byte string "SET_CONFLICT_KEY" onto the
            // stack.
            ReadConflictKey => {
                let begin: Bytes = self.pop_bytes().await;
                let mut end = begin.clone().into_owned();
                end.push(0);
                debug!("read_conflict_key {begin:?} {end:?}");
                self.push_res(
                    number,
                    trx.as_mut()
                        .add_conflict_range(&begin, &end, ConflictRangeType::Read),
                    b"SET_CONFLICT_KEY",
                );
            }
            WriteConflictKey => {
                let begin: Bytes = self.pop_bytes().await;
                let mut end = begin.clone().into_owned();
                end.push(0);
                debug!("write_conflict_key {begin:?} {end:?}");
                self.push_res(
                    number,
                    trx.as_mut()
                        .add_conflict_range(&begin, &end, ConflictRangeType::Write),
                    b"SET_CONFLICT_KEY",
                );
            }
            // Sets the NEXT_WRITE_NO_WRITE_CONFLICT_RANGE transaction option on the
            // current transaction. Does not modify the stack.
            DisableWriteConflict => {
                debug!("disable_write_conflict");
                self.check(
                    number,
                    trx.as_mut()
                        .set_option(TransactionOption::NextWriteNoWriteConflictRange),
                )?
            }
            // Commits the current transaction (with no retry behavior). May optionally
            // push a future onto the stack.
            Commit => {
                debug!("commit");
                let trx_name = trx_name.clone();
                let trx_id = self.next_trx_id();
                let f = trx
                    .take(trx_id)
                    .commit()
                    .map(|r| match r {
                        Ok(c) => StackResult {
                            state: trx_name.map(|n| (n, TransactionState::TransactionCommitted(c))),
                            data: Ok(RESULT_NOT_PRESENT.clone().into_owned()),
                        },
                        Err(c) => {
                            let err = FdbError::from_code(c.code());
                            StackResult {
                                state: trx_name
                                    .map(|n| (n, TransactionState::TransactionCommitError(c))),
                                data: Err(err),
                            }
                        }
                    })
                    .boxed_local();
                self.push_fut(number, trx_id, f);
                pending = true;
            }
            // Resets the current transaction.
            Reset => {
                debug!("reset");
                trx.as_mut().reset();
            }
            // Cancels the current transaction.
            Cancel => {
                debug!("cancel");
                let cancelled = trx.take(0).cancel();
                *trx = TransactionState::TransactionCancelled(cancelled);
            }

            // Gets the committed version from the current transaction and stores it in the
            // internal stack machine state as the last seen version. Pushes the byte
            // string "GOT_COMMITTED_VERSION" onto the stack.
            GetCommittedVersion => {
                debug!("committed_version");
                if let TransactionState::TransactionCommitted(t) = &*trx {
                    let last_version = t
                        .committed_version()
                        .expect("failed to get committed version");
                    self.last_version = last_version;
                    self.push(number, GOT_COMMITTED_VERSION.clone().into_owned());
                } else {
                    warn!("committed_version() called on a non commited transaction");
                    self.last_version = -1;
                    self.push(number, GOT_COMMITTED_VERSION.clone().into_owned());
                }
            }

            // Calls get_approximate_size and pushes the byte string "GOT_APPROXIMATE_SIZE"
            // onto the stack. Note bindings may issue GET_RANGE calls with different
            // limits, so these bindings can obtain different sizes back.
            GetApproximateSize => {
                debug!("get_approximate_size");
                match trx.as_mut().get_approximate_size().await {
                    Ok(_) => self.push(number, GOT_APPROXIMATE_SIZE_RESPONSE.clone().into_owned()),
                    Err(error) => self.push_err(number, error),
                }
            }

            GetRangeSplitPoints => {
                debug!("get_range_split_points");
                let begin: Bytes = self.pop_bytes().await;
                let end: Bytes = self.pop_bytes().await;
                let chunk_size = self.pop_i64().await;

                match trx
                    .as_mut()
                    .get_range_split_points(&begin, &end, chunk_size)
                    .await
                {
                    Ok(_) => {
                        self.push(number, GET_RANGE_SPLIT_POINTS_RESPONSE.clone().into_owned())
                    }
                    Err(error) => self.push_err(number, error),
                }
            }

            // Pops the top item off the stack and pushes it back on. If the top item on
            // the stack is a future, this will have the side effect of waiting on the
            // result of the future and pushing the result on the stack. Does not change
            // the instruction number of the item.
            WaitFuture => {
                debug!("wait_future");
                let item = self.pop().await;
                self.stack.push(item);
            }

            // Pops the top two items off of the stack as BEGIN_KEY and END_KEY to
            // construct a key range. Then call the `getEstimatedRangeSize` API of
            // the language binding. Make sure the API returns without error. Finally
            // push the string "GOT_ESTIMATED_RANGE_SIZE" onto the stack.
            GetEstimatedRangeSize => {
                debug!("get estimated range size");
                let begin = self.pop_bytes().await;
                let end = self.pop_bytes().await;

                match trx
                    .as_mut()
                    .get_estimated_range_size_bytes(&begin, &end)
                    .await
                {
                    Ok(estimate) => {
                        debug!("got an estimate of {estimate} bytes");
                        self.push(number, ESTIMATE_RANGE_RESPONSE.clone().into_owned());
                    }
                    Err(error) => {
                        self.push_err(number, error);
                    }
                };
            }

            // Pops the top item off of the stack as N. Pops the next N items off of the
            // stack and packs them as the tuple [item0,item1,...,itemN], and then pushes
            // this single packed value onto the stack.
            TuplePack => {
                let n: usize = self.pop_usize().await;
                debug!("tuple_pack {n}");
                let mut buf = Vec::new();
                for _ in 0..n {
                    let element: Element = self.pop_element().await;
                    debug!(" - {element:?}");
                    buf.push(element);
                }
                let tuple = Element::Tuple(buf);
                self.push(number, Element::Bytes(pack(&tuple).into()));
            }

            // Pops the top item off of the stack as a byte string prefix. Pops the next item
            // off of the stack as N. Pops the next N items off of the stack and packs them
            // as the tuple [item0,item1,...,itemN], with the provided prefix and tries to
            // append the position of the first incomplete versionstamp as if the byte
            // string were to be used as a key in a SET_VERSIONSTAMP_KEY atomic op. If there
            // are no incomplete versionstamp instances, then this pushes the literal byte
            // string 'ERROR: NONE' to the stack. If there is more than one, then this pushes
            // the literal byte string 'ERROR: MULTIPLE'. If there is exactly one, then it pushes
            // the literal byte string 'OK' and then pushes the packed tuple. (Languages that
            // do not contain a 'Versionstamp' tuple-type do not have to implement this
            // operation.)
            TuplePackWithVersionstamp => {
                let prefix = self.pop_bytes().await;
                let n: usize = self.pop_usize().await;
                debug!("tuple_pack_with_versionstamp {prefix:?} {n}");
                let mut buf = Vec::new();
                for _ in 0..n {
                    let element: Element = self.pop_element().await;
                    debug!(" - {element:?}");
                    buf.push(element);
                }

                let tuple = Element::Tuple(buf.clone());
                let i = tuple.count_incomplete_versionstamp();
                let mut vec = prefix.into_owned();
                let offset = buf.pack_into_vec_with_versionstamp(&mut vec);
                match offset {
                    VersionstampOffset::None { size: _ } => {
                        assert_eq!(i, 0);
                        self.push(number, ERROR_NONE.clone().into_owned());
                    }
                    VersionstampOffset::OneIncomplete { offset: _ } => {
                        assert_eq!(i, 1);
                        let data = Element::Bytes(vec.into());
                        self.push(number, OK.clone().into_owned());
                        self.push(number, data);
                    }
                    VersionstampOffset::MultipleIncomplete => {
                        assert!(i > 1);
                        self.push(number, ERROR_MULTIPLE.clone().into_owned());
                    }
                }
            }

            // Pops the top item off of the stack as PACKED, and then unpacks PACKED into a
            // tuple. For each element of the tuple, packs it as a new tuple and pushes it
            // onto the stack.
            TupleUnpack => {
                let data = self.pop_bytes().await;
                debug!("tuple_unpack {data:?}");
                let data: Vec<Element> = unpack(&data).unwrap();
                for element in data {
                    debug!(" - {element:?}");
                    self.push(number, Element::Bytes(pack(&(element,)).into()));
                }
            }
            // Pops the top item off of the stack as N. Pops the next N items off of the
            // stack, and passes these items as a tuple (or array, or language-appropriate
            // structure) to the tuple range method. Pushes the begin and end elements of
            // the returned range onto the stack.
            TupleRange => {
                let n: usize = self.pop_usize().await;
                debug!("tuple_range {n:?}");
                let mut tup = Vec::new();
                for _ in 0..n {
                    let element = self.pop_element().await;
                    tup.push(element);
                }

                let subspace = Subspace::from(tup);
                let (start, end) = subspace.range();
                self.push(number, Element::Bytes(start.into()));
                self.push(number, Element::Bytes(end.into()));
            }

            // Pops the top item off of the stack as N. Pops the next N items off of the
            // stack as packed tuples (i.e., byte strings), unpacks them, sorts the tuples,
            // repacks them into byte strings, and then pushes these packed tuples onto
            // the stack so that the final top of the stack now has the greatest
            // element. If the binding has some kind of tuple comparison function, it should
            // use that to sort. Otherwise, it should sort them lexicographically by
            // their byte representation. The choice of function should not affect final sort order.
            TupleSort => {
                let n: usize = self.pop_usize().await;
                debug!("tuple_sort {n:?}");
                let mut tup = Vec::<Element>::new();
                for _ in 0..n {
                    let packed = self.pop_bytes().await;
                    let element: Element = unpack(&packed).unwrap();
                    debug!("- {element:?}");
                    tup.push(element.into_owned());
                }
                tup.sort();
                debug!("tuple_sorted");
                for element in tup {
                    debug!("- {element:?}");
                    self.push(number, Element::Bytes(pack(&element).into()));
                }
            }

            // Pops the top item off of the stack. This will be a byte-string of length 4
            // containing the IEEE 754 encoding of a float in big-endian order.
            // This is then converted into a float and pushed onto the stack.
            EncodeFloat => {
                let bytes = self.pop_bytes().await;
                debug!("encode_float {bytes:?}");
                let mut arr = [0; 4];
                arr.copy_from_slice(&bytes);
                let f = f32::from_bits(u32::from_be_bytes(arr));
                self.push(number, Element::Float(f));
            }
            // Pops the top item off of the stack. This will be a byte-string of length 8
            // containing the IEEE 754 encoding of a double in big-endian order.
            // This is then converted into a double and pushed onto the stack.
            EncodeDouble => {
                let bytes = self.pop_bytes().await;
                debug!("encode_double {bytes:?}");
                let mut arr = [0; 8];
                arr.copy_from_slice(&bytes);
                let f = f64::from_bits(u64::from_be_bytes(arr));
                self.push(number, Element::Double(f));
            }
            // Pops the top item off of the stack. This will be a single-precision float.
            // This is converted into a (4 byte) byte-string of its IEEE 754 representation
            // in big-endian order, and pushed onto the stack.
            DecodeFloat => {
                let f: f32 = match self.pop_element().await {
                    Element::Float(v) => v,
                    element => panic!("float was expected, found {element:?}"),
                };
                debug!("decode_float {f}");
                self.push(
                    number,
                    Element::Bytes(f.to_bits().to_be_bytes().to_vec().into()),
                );
            }
            // Pops the top item off of the stack. This will be a double-precision float.
            // This is converted into a (8 byte) byte-string its IEEE 754 representation
            // in big-endian order, and pushed onto the stack.
            DecodeDouble => {
                let f: f64 = match self.pop_element().await {
                    Element::Double(v) => v,
                    element => panic!("double was expected, found {element:?}"),
                };
                debug!("decode_double {f}");
                self.push(
                    number,
                    Element::Bytes(f.to_bits().to_be_bytes().to_vec().into()),
                );
            }

            // Pops the top item off of the stack as PREFIX. Creates a new stack machine
            // instance operating on the same database as the current stack machine, but
            // operating on PREFIX. The new stack machine should have independent internal
            // state. The new stack machine should begin executing instructions concurrent
            // with the current stack machine through a language-appropriate mechanism.
            StartThread => {
                let prefix = self.pop_bytes().await;
                debug!("start_thread {prefix:?}");
                let db = db.clone();
                self.threads.push(
                    thread::Builder::new()
                        .name(format!("{prefix:?}"))
                        .spawn(move || {
                            let mut sm = StackMachine::new(&db, prefix.clone());
                            futures::executor::block_on(sm.run(db)).unwrap();
                            sm.join();
                            debug!("thread {prefix:?} exit");
                        })
                        .unwrap(),
                );
            }

            // Pops the top item off of the stack as PREFIX. Blocks execution until the
            // range with prefix PREFIX is not present in the database. This should be
            // implemented as a polling loop inside of a language- and binding-appropriate
            // retryable construct which synthesizes FoundationDB error 1020 when the range
            // is not empty. Pushes the string "WAITED_FOR_EMPTY" onto the stack when
            // complete.
            WaitEmpty => {
                let prefix = self.pop_bytes().await;
                debug!("wait_empty {prefix:?}");
                let (begin, end) = range(prefix);

                let r = db
                    .run(|trx, _| {
                        let (begin, end) = (&begin, &end);
                        async move {
                            let range = trx
                                .get_range(&RangeOption::from((&begin[..], &end[..])), 1, false)
                                .await?;
                            debug!("wait_empty {:?} range {}", begin, range.len());
                            if !range.is_empty() {
                                // not_committed: retry until the range is empty.
                                return Err(FdbBindingError::from(FdbError::from_code(1020)));
                            }
                            Ok(())
                        }
                    })
                    .await
                    .map_err(|e| {
                        e.get_fdb_error()
                            .unwrap_or_else(|| panic!("WAIT_EMPTY failed: {e}"))
                    });
                self.check(number, r)?;
                self.push(number, WAITED_FOR_EMPTY.clone().into_owned());
            }

            // Runs the unit tests of the binding, ported from the Python tester.
            UnitTests => {
                debug!("unit_tests");
                let r = unit_tests::run_unit_tests(&db).await;
                self.check(number, r)?;
            }

            code => unreachable!("{code:?} is run by run_directory_instr"),
        }

        if is_db && pending {
            let item = self.pop().await;
            self.stack.push(item);
        }

        if is_db && mutation {
            let r = trx.take(0).commit().await.map_err(|e| e.into());
            self.check(number, r)?;
            self.push(number, RESULT_NOT_PRESENT.clone().into_owned());
        }

        check_flags(&instr);
        Ok(())
    }

    async fn run(&mut self, db: Arc<Database>) -> FdbResult<()> {
        info!("Fetching instructions...");
        let instrs = self.fetch_instr(&db.create_trx()?).await?;
        info!("{} instructions found", instrs.len());

        for (i, instr) in instrs.into_iter().enumerate() {
            trace!("{}/{}, {:?}", i, self.stack.len(), instr);
            let _ = self.run_step(db.clone(), i, instr).await;
        }

        Ok(())
    }

    fn join(&mut self) {
        for handle in self.threads.drain(0..) {
            handle.join().expect("joined thread to not panic");
        }
    }
}

fn main() {
    let now = std::time::Instant::now();
    env_logger::Builder::from_default_env()
        .format(move |buf, record| {
            let current_thread = thread::current();
            let thread_name = current_thread.name().unwrap_or("?");
            writeln!(
                buf,
                "{} {} {} - {}",
                (std::time::Instant::now() - now).as_millis(),
                thread_name,
                record.level(),
                record.args()
            )
        })
        .format_timestamp_millis()
        .target(env_logger::Target::Stdout)
        .init();

    let args = std::env::args().collect::<Vec<_>>();
    if !(3..=4).contains(&args.len()) {
        eprintln!("usage: {} PREFIX API_VERSION [CLUSTER_FILE]", args[0]);
        std::process::exit(2);
    }
    let prefix = &args[1];

    let cluster_path = if args.len() > 3 {
        Some(args[3].as_str())
    } else {
        None
    };

    let api_version = args[2].parse::<i32>().expect("failed to parse api version");
    unit_tests::API_VERSION.store(api_version, std::sync::atomic::Ordering::Relaxed);

    info!("Starting rust bindingtester with api_version {api_version}");
    api::FdbApiBuilder::default()
        .set_runtime_version(api_version)
        .build()
        .expect("failed to initialize FoundationDB API")
        .boot()
        .expect("failed to start the FoundationDB network");

    let db = Arc::new(
        futures::executor::block_on(fdb::Database::new_compat(cluster_path))
            .expect("failed to get database"),
    );

    let mut sm = StackMachine::new(&db, Bytes::from(prefix.to_owned().into_bytes()));
    futures::executor::block_on(sm.run(db)).unwrap();
    sm.join();
    drop(sm);

    // Stop the network explicitly rather than through the atexit hook: with client
    // trace logging enabled (FDB_NETWORK_OPTION_TRACE_ENABLE), stopping it from the
    // hook crashes the network thread at exit.
    api::stop_network().expect("failed to stop the FoundationDB network");

    info!("Closing...");

    info!("Done.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a running FoundationDB cluster"]
    fn on_error_restores_transaction_before_next_instruction() {
        futures::executor::block_on(async {
            let db = Arc::new(Database::new_compat(None).await.expect("cannot open fdb"));
            let mut sm = StackMachine::new(&db, Bytes::from(b"on-error-reuse".as_slice()));
            let key = b"on-error-reuse".as_slice();
            let instructions = [
                pack(&("NEW_TRANSACTION",)),
                pack(&("PUSH", 1020)),
                pack(&("ON_ERROR",)),
                pack(&("GET_READ_VERSION",)),
                pack(&("PUSH", 2000)),
                pack(&("ON_ERROR",)),
                pack(&("WAIT_FUTURE",)),
                pack(&("PUSH", key)),
                pack(&("GET",)),
                pack(&("WAIT_FUTURE",)),
                pack(&("RESET",)),
                pack(&("GET_READ_VERSION",)),
            ];

            for (number, instruction) in instructions.iter().enumerate() {
                sm.run_step(db.clone(), number, Instr::from(instruction))
                    .await
                    .expect("instruction failed");
            }

            let error = Element::Bytes(pack(&(b"ERROR".as_slice(), b"2000".as_slice())).into());
            let cancelled = Element::Bytes(pack(&(b"ERROR".as_slice(), b"1025".as_slice())).into());
            assert_eq!(sm.stack.len(), 5);
            assert_eq!(sm.stack[0].data.as_ref(), Some(&RESULT_NOT_PRESENT));
            assert_eq!(sm.stack[1].data.as_ref(), Some(&GOT_READ_VERSION));
            assert_eq!(sm.stack[2].data.as_ref(), Some(&error));
            // A failed on_error cancels the native transaction at API >= 610.
            // A replacement transaction or implicit reset would hide this state.
            assert_eq!(sm.stack[3].data.as_ref(), Some(&cancelled));
            assert_eq!(sm.stack[4].data.as_ref(), Some(&GOT_READ_VERSION));
        });
    }

    #[test]
    #[ignore = "requires a running FoundationDB cluster"]
    fn failed_instruction_keeps_current_transaction() {
        futures::executor::block_on(async {
            // A dedicated handle, so the retry limit below does not leak into other tests.
            let db = Arc::new(Database::new_compat(None).await.expect("cannot open fdb"));
            db.set_option(fdb::options::DatabaseOption::TransactionRetryLimit(0))
                .expect("cannot set retry limit");
            let prefix = b"failed-instr-keeps-trx".as_slice();
            db.run(|trx, _| async move {
                trx.set(&[prefix, b"/key"].concat(), b"value");
                Ok::<_, fdb::FdbBindingError>(())
            })
            .await
            .expect("cannot write key");

            // WAIT_EMPTY on a non empty prefix fails once the retry limit is reached, so it
            // pushes an error and leaves run_step early. The next instruction must still
            // find the current transaction instead of panicking.
            let mut sm = StackMachine::new(&db, Bytes::from(prefix));
            let instructions = [
                pack(&("NEW_TRANSACTION",)),
                pack(&("PUSH", prefix)),
                pack(&("WAIT_EMPTY",)),
                pack(&("GET_READ_VERSION",)),
            ];
            let mut results = Vec::new();
            for (number, instruction) in instructions.iter().enumerate() {
                results.push(
                    sm.run_step(db.clone(), number, Instr::from(instruction))
                        .await,
                );
            }

            db.run(|trx, _| async move {
                let (begin, end) = range(Bytes::from(prefix));
                trx.clear_range(&begin, &end);
                Ok::<_, fdb::FdbBindingError>(())
            })
            .await
            .expect("cannot clean up");

            assert_eq!(results, [Ok(()), Ok(()), Err(()), Ok(())]);
            let not_committed =
                Element::Bytes(pack(&(b"ERROR".as_slice(), b"1020".as_slice())).into());
            assert_eq!(sm.stack.len(), 2);
            assert_eq!(sm.stack[0].data.as_ref(), Some(&not_committed));
            assert_eq!(sm.stack[1].data.as_ref(), Some(&GOT_READ_VERSION));
            assert!(sm.transactions.contains_key(&sm.cur_transaction));
        });
    }

    #[test]
    #[ignore = "requires a running FoundationDB cluster"]
    fn log_stack_leaves_current_transaction_alone() {
        futures::executor::block_on(async {
            let db = Arc::new(Database::new_compat(None).await.expect("cannot open fdb"));
            let prefix = b"log-stack-new-trx".as_slice();
            let key = [prefix, b"/key"].concat();
            let log_prefix = [prefix, b"/log/"].concat();
            let mut sm = StackMachine::new(&db, Bytes::from(prefix));
            let instructions = [
                pack(&("NEW_TRANSACTION",)),
                pack(&("PUSH", b"value".as_slice())),
                pack(&("PUSH", key.as_slice())),
                pack(&("SET",)),
                pack(&("PUSH", b"logged".as_slice())),
                pack(&("PUSH", log_prefix.as_slice())),
                pack(&("LOG_STACK",)),
            ];
            for (number, instruction) in instructions.iter().enumerate() {
                sm.run_step(db.clone(), number, Instr::from(instruction))
                    .await
                    .expect("instruction failed");
            }

            let (begin, end) = range(Bytes::from(prefix));
            let (pending_key, logged) = db
                .run(|trx, _| {
                    let (key, log_prefix, begin, end) = (&key, &log_prefix, &begin, &end);
                    async move {
                        let pending_key = trx.get(key, false).await?.map(|v| v.to_vec());
                        let mut log_key = log_prefix.clone();
                        0usize.pack_into_vec(&mut log_key);
                        4usize.pack_into_vec(&mut log_key);
                        let logged = trx.get(&log_key, false).await?.map(|v| v.to_vec());
                        trx.clear_range(begin, end);
                        Ok::<_, fdb::FdbBindingError>((pending_key, logged))
                    }
                })
                .await
                .expect("cannot read back");

            // The SET is still pending on the current transaction, not committed.
            assert_eq!(pending_key, None);
            assert_eq!(logged, Some(pack(&(b"logged".as_slice(),))));
            assert!(sm.stack.is_empty());
            let trx = sm
                .transactions
                .get_mut(&sm.cur_transaction)
                .expect("current transaction is gone");
            let value = trx.as_mut().get(&key, false).await.expect("cannot read");
            assert_eq!(value.as_deref(), Some(b"value".as_slice()));
        });
    }
}
