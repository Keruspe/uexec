use std::thread::{self, JoinHandle};

use parking_lot::Mutex;

/* Control the pool of workers */
#[derive(Default)]
pub(crate) struct Workers(Mutex<Vec<Worker>>);

impl Workers {
    pub(crate) fn spawn(&self, threads: u8) {
        let mut workers = self.0.lock();
        for _ in 0..threads {
            let (sender, receiver) = async_channel::bounded(1);
            let handle = thread::spawn(|| {
                crate::block_on(async move {
                    let _ = receiver.recv().await;
                })
            });
            workers.push(Worker {
                trigger: sender,
                thread: handle,
            });
        }
    }

    pub(crate) fn terminate(&self) {
        let mut workers = self.0.lock();
        for worker in workers.iter() {
            worker.trigger_termination();
        }
        for worker in workers.drain(..) {
            worker.terminate();
        }
    }
}

struct Worker {
    trigger: async_channel::Sender<()>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn trigger_termination(&self) {
        let _ = self.trigger.try_send(());
    }

    fn terminate(self) {
        if let Err(err) = self.thread.join() {
            std::panic::resume_unwind(err);
        }
    }
}
