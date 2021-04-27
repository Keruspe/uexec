#[test]
fn workers() {
    for _ in 0..32 {
        uexec::spawn_workers(u8::MAX);
        uexec::terminate_workers();
    }
}
