//! Asynchronous runtime module
//!
//! This module defines the `fdb_spawn` method to run to completion asynchronous tasks containing
//! FoundationDB futures

#![doc = include_str!("../docs/fdb_rt.md")]

use std::{
    cell::UnsafeCell,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

static POLL_COUNT: AtomicU64 = AtomicU64::new(0);
static IN_POLL: AtomicBool = AtomicBool::new(false);

struct FDBWaker {
    f: UnsafeCell<Pin<Box<dyn Future<Output = ()>>>>,
}

fn fdbwaker_wake(waker_ref: &FDBWaker, decrease: bool) {
    let poll_id = POLL_COUNT.fetch_add(1, Ordering::SeqCst);

    // Re-entrancy detection
    if IN_POLL.load(Ordering::SeqCst) {
        println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
        println!("!!! RE-ENTRANCY DETECTED: fdbwaker_wake called while already polling !!!");
        println!("!!! poll_id={}, decrease={}", poll_id, decrease);
        println!("!!! This creates two mutable references to UnsafeCell - UNDEFINED BEHAVIOR !!!");
        println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    }

    println!(
        "[fdb_rt] fdbwaker_wake #{}: polling (decrease={})",
        poll_id, decrease
    );

    IN_POLL.store(true, Ordering::SeqCst);

    let waker_raw = fdbwaker_clone(waker_ref);
    println!("[fdb_rt] #{}: after fdbwaker_clone", poll_id);
    let waker = unsafe { Waker::from_raw(waker_raw) };
    println!("[fdb_rt] #{}: after Waker::from_raw", poll_id);
    let mut cx = Context::from_waker(&waker);
    println!("[fdb_rt] #{}: after Context::from_waker", poll_id);
    let f = unsafe { &mut *waker_ref.f.get() };
    println!("[fdb_rt] #{}: after getting &mut future, about to poll", poll_id);
    let result = f.as_mut().poll(&mut cx);
    println!("[fdb_rt] #{}: poll completed", poll_id);

    IN_POLL.store(false, Ordering::SeqCst);

    match &result {
        Poll::Ready(()) => println!("[fdb_rt] fdbwaker_wake #{}: poll returned Ready", poll_id),
        Poll::Pending => println!("[fdb_rt] fdbwaker_wake #{}: poll returned Pending", poll_id),
    }
    if decrease {
        fdbwaker_drop(waker_ref);
    }
}

fn fdbwaker_clone(waker_ref: &FDBWaker) -> RawWaker {
    println!("[fdb_rt] fdbwaker_clone: cloning waker");
    let waker_arc = unsafe { Arc::from_raw(waker_ref) };
    std::mem::forget(waker_arc.clone()); // increase ref count
    RawWaker::new(Arc::into_raw(waker_arc) as *const (), &VTABLE)
}

fn fdbwaker_drop(waker_ref: &FDBWaker) {
    println!("[fdb_rt] fdbwaker_drop: dropping waker");
    unsafe { Arc::from_raw(waker_ref) };
}

const VTABLE: RawWakerVTable = unsafe {
    RawWakerVTable::new(
        |waker_ptr| fdbwaker_clone(&*(waker_ptr as *const FDBWaker)), // clone
        |waker_ptr| fdbwaker_wake(&*(waker_ptr as *const FDBWaker), true), // wake (decrease refcount)
        |waker_ptr| fdbwaker_wake(&*(waker_ptr as *const FDBWaker), false), // wake_by_ref (don't decrease refcount)
        |waker_ptr| fdbwaker_drop(&*(waker_ptr as *const FDBWaker)), // drop (decrease refcount)
    )
};

/// Spawn an async block and resolve all contained FoundationDB futures
pub fn fdb_spawn<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    println!("[fdb_rt] fdb_spawn: starting new future");
    let f = UnsafeCell::new(Box::pin(future));
    let waker_arc = Arc::new(FDBWaker { f });
    let raw_waker = RawWaker::new(Arc::into_raw(waker_arc) as *const (), &VTABLE);
    let waker = unsafe { Waker::from_raw(raw_waker) };
    waker.wake();
}
