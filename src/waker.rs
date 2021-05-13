use crate::state::State;

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::Wake,
};

use crossbeam_utils::sync::Unparker;

/* Wake the executor after registering us in the list of tasks that need polling */
pub(crate) struct MyWaker {
    id: u64,
    state: State,
    pollable: Arc<AtomicBool>,
    unparker: Unparker,
}

impl MyWaker {
    pub(crate) fn new(
        id: u64,
        state: State,
        pollable: Arc<AtomicBool>,
        unparker: Unparker,
    ) -> Self {
        Self {
            id,
            state,
            pollable,
            unparker,
        }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        if let Some(future) = self.state.deregister_future(self.id) {
            if crate::RUNS_WORKER.with(|runs_worker| runs_worker.load(Ordering::Acquire)) {
                crate::LOCAL_EXECUTOR.with(|executor| executor.push_future(future, false));
            } else {
                crate::EXECUTOR.push_future(future);
            }
        } else {
            self.pollable.store(true, Ordering::Release);
        }
        self.unparker.unpark();
    }
}

/* Dummy waker */
pub(crate) struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: Arc<Self>) {}
}
