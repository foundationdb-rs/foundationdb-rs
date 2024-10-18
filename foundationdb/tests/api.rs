use foundationdb::api::{
    check_api_version_set, get_max_api_version, is_network_setup, is_network_thread_running,
    spawn_network_thread_if_needed, stop_network,
};

#[test]
fn test_boot() {
    foundationdb::boot().expect("could not boot fdb client");
    assert!(check_api_version_set().is_ok());
    assert!(is_network_setup());
    assert!(is_network_thread_running());

    // set_api_version can be called multiple times
    foundationdb::api::set_api_version(get_max_api_version())
        .expect("should be safe to call multiple times");

    spawn_network_thread_if_needed().expect("could not start network thread");
    // can be called multiple times
    spawn_network_thread_if_needed().expect("could not start network thread");

    stop_network().expect("could not stop network");
    stop_network().expect("could not stop network");
}
