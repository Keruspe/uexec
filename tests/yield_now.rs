// Taken from async-executor's benchmark

use futures_lite::future;

const TASKS: usize = 300;
const STEPS: usize = 300;

fn run(f: impl FnOnce()) {
    let (s, r) = flume::bounded::<()>(1);
    easy_parallel::Parallel::new()
        .each(0..num_cpus::get(), |_| {
            let r = r.clone();
            uexec::block_on(async move { r.recv_async().await })
        })
        .finish(move || {
            let _s = s;
            f()
        });
}

#[test]
fn yield_now() {
    run(|| {
        future::block_on(async {
            let mut tasks = Vec::new();
            for _ in 0..TASKS {
                tasks.push(uexec::spawn(async move {
                    for _ in 0..STEPS {
                        future::yield_now().await;
                    }
                }));
            }
            for task in tasks {
                task.await;
            }
        });
    });
}
