use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    thread,
    time::Duration,
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

            // Setup out waker to wake the executor after one second
            let waker = cx.waker().clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(1));
                waker.wake();
            });
            Poll::Pending
        }
    }
}

fn main() {
    // Run a thread pool of executors
    for _ in 0..THREADS {
        std::thread::spawn(|| uexec::worker());
    }

    assert_eq!(uexec::block_on(async { 3 + 1 }), 4);
    let future = CountDown(10);
    uexec::block_on(future);
}
