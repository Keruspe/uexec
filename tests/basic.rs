#[test]
fn basic() {
    assert_eq!(uexec::block_on(async { 3 + 1 }), 4);
}
