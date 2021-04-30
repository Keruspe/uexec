use crate::{state::State, threads::Threads};

use std::{sync::Arc, task::Wake};

/* Wake the executor after registering us in the list of tasks that need polling */
pub(crate) struct MyWaker {
    id: u64,
    threads: Threads,
    state: State,
}

impl MyWaker {
    pub(crate) fn new(id: u64, threads: Threads, state: State) -> Self {
        Self { id, threads, state }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.state.register_pollable(self.id);
        self.threads.unpark_random();
    }
}

/* Dummy waker */
pub(crate) struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: Arc<Self>) {}
}
