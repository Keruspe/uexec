# uexec

[![API Docs](https://docs.rs/uexec/badge.svg)](https://docs.rs/uexec)
[![Build status](https://github.com/Keruspe/uexec/workflows/Build%20and%20test/badge.svg)](https://github.com/Keruspe/uexec/actions)
[![Downloads](https://img.shields.io/crates/d/uexec.svg)](https://crates.io/crates/uexec)

Simple work-stealing global and local executor

# Features

- no optional feature yet

# Examples

```rust
use futures_lite::future;

// spawn several worker threads
uexec::spawn_workers(4);

// spawn a task on the multi-threaded executor
let task1 = uexec::spawn(async {
    1 + 2
});
// spawn a task on the local executor (same thread)
let task2 = uexec::spawn_local(async {
    3 + 4
});
let task = future::zip(task1, task2);

// run the executor
uexec::block_on(async {
    assert_eq!(task.await, (3, 7));
});

// terminate our worker threads
uexec::terminate_workers();
```

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

#### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
