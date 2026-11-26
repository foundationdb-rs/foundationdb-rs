//! The stack of the stack machine and its items.

use foundationdb as fdb;

use std::pin::Pin;

use fdb::tuple::{Bytes, Element, pack};
use fdb::*;
use futures::prelude::*;

use crate::TransactionState;

pub(crate) struct StackResult {
    pub(crate) state: Option<(Bytes<'static>, TransactionState)>,
    pub(crate) data: FdbResult<Element<'static>>,
}

impl From<FdbError> for StackResult {
    fn from(err: FdbError) -> Self {
        StackResult {
            state: None,
            data: Err(err),
        }
    }
}

impl From<Element<'static>> for StackResult {
    fn from(data: Element<'static>) -> Self {
        StackResult {
            state: None,
            data: Ok(data),
        }
    }
}

impl From<FdbResult<Element<'static>>> for StackResult {
    fn from(data: FdbResult<Element<'static>>) -> Self {
        StackResult { state: None, data }
    }
}

pub(crate) type StackFuture = Pin<Box<dyn Future<Output = StackResult>>>;

pub(crate) struct StackItem {
    pub(crate) number: usize,
    pub(crate) data: Option<Element<'static>>,
    pub(crate) fut: Option<(usize, StackFuture)>,
}

impl StackItem {
    pub(crate) async fn await_fut(&mut self) -> Option<(Bytes<'static>, TransactionState)> {
        let mut ret = None;
        if let Some((_id, fut)) = self.fut.take() {
            let StackResult { state, mut data } = fut.await;

            if let Some((name, state)) = state {
                trace!("{name:?} = {state:?}");
                match state {
                    TransactionState::TransactionCommitError(e) => {
                        let err = FdbError::from_code(e.code());
                        ret = Some((name, TransactionState::TransactionCommitError(e)));
                        data = Err(err);
                    }
                    state => {
                        ret = Some((name, state));
                    }
                }
            }

            let data = data.unwrap_or_else(|err| {
                trace!("ERROR {err:?}");
                let packed = pack(&(
                    Bytes::from(b"ERROR".as_ref()),
                    Bytes::from(format!("{}", err.code()).into_bytes()),
                ));
                Element::Bytes(packed.into())
            });

            self.data = Some(data);
        }
        ret
    }
}

impl Clone for StackItem {
    fn clone(&self) -> Self {
        if self.fut.is_some() {
            panic!("cannot clone future stack item");
        }
        Self {
            number: self.number,
            data: self.data.clone(),
            fut: None,
        }
    }
}

impl std::fmt::Debug for StackItem {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "[item num={}, data={:?}]", self.number, self.data)
    }
}
