use crate::{
    future_holder::FutureHolder, receiver::Receiver, state::State, threads::Threads, JoinHandle,
};

use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_utils::sync::Unparker;

/* The global executor state */
#[derive(Default)]
pub(crate) struct GlobalExecutor {
    /* Generate a new id for each task */
    task_id: AtomicU64,
    injector: Injector<FutureHolder>,
    threads: Threads,
    state: State,
}

impl GlobalExecutor {
    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn steal(&self, worker: &Worker<FutureHolder>) -> Option<FutureHolder> {
        loop {
            match self.injector.steal_batch_and_pop(worker) {
                Steal::Success(future) => return Some(future),
                Steal::Empty => return None,
                Steal::Retry => {}
            }
        }
    }

    pub(crate) fn next(&self) -> Option<FutureHolder> {
        self.state.next()
    }

    pub(crate) fn poll_receiver<R>(&self, receiver: &mut Receiver<R>) -> Option<R> {
        if let Poll::Ready(res) = receiver.poll() {
            self.threads.deregister_current();
            Some(res)
        } else {
            None
        }
    }

    pub(crate) fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
    ) -> JoinHandle<R> {
        let id = self.next_task_id();
        let (sender, receiver) = flume::bounded(1);
        FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send_async(res).await);
            },
            self.threads.clone(),
            self.state.clone(),
            None,
        )
        .run();
        JoinHandle::new(id, receiver, self.state.clone())
    }

    pub(crate) fn has_pollable_tasks(&self) -> bool {
        self.state.has_pollable_tasks()
    }

    pub(crate) fn register_current_thread(
        &self,
        stealer: Stealer<FutureHolder>,
        unparker: Unparker,
    ) {
        self.threads.register_current(stealer, unparker)
    }
}
