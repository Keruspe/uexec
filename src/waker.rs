use crate::{future_holder::FutureHolder, state::State};

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::Wake,
};

use crossbeam_deque::Worker;
use crossbeam_utils::sync::Unparker;

/* Wake the executor after registering us in the list of tasks that need polling */
pub(crate) struct MyWaker {
    id: u64,
    worker: Option<Arc<Worker<FutureHolder>>>,
    unparker: Option<Unparker>,
    state: State,
    pollable: Arc<AtomicBool>,
}

impl MyWaker {
    pub(crate) fn new(
        id: u64,
        worker: Option<Arc<Worker<FutureHolder>>>,
        unparker: Option<Unparker>,
        state: State,
        pollable: Arc<AtomicBool>,
    ) -> Self {
        Self {
            id,
            worker,
            unparker,
            state,
            pollable,
        }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        if let Some(future) = self.state.deregister_future(self.id) {
            if let Some(worker) = self.worker.as_ref() {
                worker.push(future);
            }
            if let Some(unparker) = self.unparker.as_ref() {
                unparker.unpark();
            }
        } else {
            self.pollable.store(true, Ordering::Release);
        }
    }
}

unsafe impl Send for MyWaker {}
unsafe impl Sync for MyWaker {}

/* Dummy waker */
pub(crate) struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: Arc<Self>) {}
}
