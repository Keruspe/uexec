#[test]
fn workers() {
    uexec::spawn_workers(u8::MAX);
    uexec::terminate_workers();
}
