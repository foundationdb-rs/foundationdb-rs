//! The state of a named transaction of the stack machine.

use foundationdb as fdb;

use std::mem::transmute;

use fdb::*;

pub(crate) enum TransactionState {
    Transaction(Transaction),
    TransactionCancelled(TransactionCancelled),
    TransactionCommitted(TransactionCommitted),
    TransactionCommitError(TransactionCommitError),
    Pending(usize),
    Dead,
}

impl std::fmt::Debug for TransactionState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use TransactionState as S;

        match self {
            S::Transaction(..) => "Transaction",
            S::TransactionCancelled(..) => "TransactionCancelled",
            S::TransactionCommitted(..) => "TransactionCommitted",
            S::TransactionCommitError(..) => "TransactionCommitError",
            S::Pending(..) => "Pending",
            S::Dead => "Dead",
        }
        .fmt(f)
    }
}

impl TransactionState {
    pub(crate) fn reset(&mut self) {
        use TransactionState as S;
        *self = match std::mem::replace(self, S::Dead) {
            S::TransactionCommitted(c) => S::Transaction(c.reset()),
            S::TransactionCommitError(c) => S::Transaction(c.reset()),
            c => c,
        }
    }

    pub(crate) fn as_mut(&mut self) -> &mut Transaction {
        use TransactionState as S;

        self.reset();
        match *self {
            S::Transaction(ref mut tr) => tr,
            S::TransactionCancelled(ref mut tr) => unsafe {
                // rust binding prevent accessing cancelled transaction
                &mut *(tr as *mut TransactionCancelled as *mut Transaction)
            },
            _ => panic!("transaction is owned by a future that is still not done"),
        }
    }

    pub(crate) fn take(&mut self, id: usize) -> Transaction {
        use TransactionState as S;

        self.reset();
        match std::mem::replace(self, S::Dead) {
            S::Transaction(tr) => {
                *self = S::Pending(id);
                tr
            }
            S::TransactionCancelled(tr) => {
                *self = S::Pending(id);
                unsafe {
                    // rust binding prevent accessing cancelled transaction
                    transmute::<TransactionCancelled, Transaction>(tr)
                }
            }
            _ => panic!("transaction is owned by a future that is still not done"),
        }
    }
}
