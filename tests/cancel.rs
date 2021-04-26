#[test]
fn cancel_finished() {
    let res = uexec::block_on(async {
        let handle = uexec::spawn(async { 1 + 2 });
        handle.cancel()
    });
    assert_eq!(res, Some(3));
}

#[test]
fn cancel_pending() {
    let res = uexec::block_on(async {
        let handle = uexec::spawn(std::future::pending::<()>());
        handle.cancel()
    });
    assert_eq!(res, None);
}
