use foundationdb::api::FdbApiBuilder;
use std::thread;

#[test]
#[should_panic(expected = "the fdb select api version can only be run once per process")]
fn test_run() {
    let (runner, cond) = FdbApiBuilder::default()
        .build()
        .expect("could not initialize api")
        .build()
        .expect("could not initialize network");

    let net_thread = thread::spawn(move || {
        unsafe { runner.run() }.expect("failed to run");
    });
    let stopper = cond.wait();

    // network thread is running

    stopper.stop().expect("failed to stop");
    net_thread.join().expect("failed to join net thread");
    println!("stopped!");

    // this should fail:
    let _ = FdbApiBuilder::default().build();
    panic!("previous line should have panicked!");
}
