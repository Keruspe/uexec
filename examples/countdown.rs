use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    thread,
};

static THREADS: u8 = 8;

struct CountDown(u8);

impl Future for CountDown {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let thread = thread::current().id();
        if self.0 == 0 {
            println!("Countdown finished in thread {:?}", thread);
            Poll::Ready(())
        } else {
            println!(
                "Countdown: {} remaining, ran in thread {:?}",
                self.0, thread
            );
            self.0 -= 1;

            if self.0 > 6 {
                println!("Adding another countdown from thread {:?}", thread);
                uexec::spawn(CountDown(2));
            } else if self.0 > 4 {
                println!("Adding another local countdown from thread {:?}", thread);
                uexec::spawn_local(CountDown(2));
            }

            // Return Pending but immediately wake the executor
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn main() {
    // Run a thread pool of executors
    uexec::spawn_workers(THREADS);

    assert_eq!(uexec::block_on(async { 3 + 1 }), 4);
    uexec::block_on(async {
        let tasks: Vec<Pin<Box<dyn Future<Output = ()> + Send>>> = vec![
            Box::pin(CountDown(5)),
            Box::pin(uexec::spawn(CountDown(10))),
        ];
        for task in tasks {
            task.await;
        }
    });

    uexec::terminate_workers();
}
