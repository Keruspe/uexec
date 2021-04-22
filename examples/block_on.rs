use std::{future::Future, task::{Context, Poll}, pin::Pin, thread, time::Duration};
use uexec::Executor;

struct CountDown(u8);

impl Future for CountDown {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0 == 0 {
            println!("Countdown finished");
            Poll::Ready(())
        } else {
            println!("Countdown: {} remaining", self.0);
            self.0 -= 1;

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
    let future = CountDown(3);
    let mut executor = Executor::default();
    executor.block_on(future);
}
