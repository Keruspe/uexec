use crate::{future_holder::FutureHolder, threads::Threads};

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use parking_lot::Mutex;

/* The common state containing all the futures */
#[derive(Clone, Default)]
pub(crate) struct State(Arc<Mutex<Futures>>);

#[derive(Default)]
struct Futures {
    /* Pending futures */
    pending: HashMap<u64, FutureHolder>,
    /* List of tasks that need polling */
    pollable: VecDeque<FutureHolder>,
    /* List of tasks that are already pollable but not yet registered */
    pollable_next: HashSet<u64>,
}

impl State {
    pub(crate) fn register_future(&self, future: FutureHolder, threads: Threads) {
        let mut futures = self.0.lock();
        if futures.pollable_next.remove(&future.id()) {
            futures.pollable.push_back(future);
            threads.unpark_random();
        } else {
            futures.pending.insert(future.id(), future);
        }
    }

    pub(crate) fn register_pollable(&self, id: u64) {
        let mut futures = self.0.lock();
        if let Some(future) = futures.pending.remove(&id) {
            // Future was pending, mark it as pollable
            futures.pollable.push_back(future);
        } else {
            // Future hasn't been registered yet
            futures.pollable_next.insert(id);
        }
    }

    pub(crate) fn has_pollable_tasks(&self) -> bool {
        !self.0.lock().pollable.is_empty()
    }

    pub(crate) fn cancel(&self, id: u64) {
        let mut futures = self.0.lock();
        // If the future was pending, do nothing, otherwise...
        if futures.pending.remove(&id).is_none() {
            // ...check if it was pollable
            if let Some((index, _)) = futures
                .pollable
                .iter()
                .enumerate()
                .find(|(_, future)| future.id() == id)
            {
                if let Some(future) = futures.pollable.remove(index) {
                    // If it was pollable, give it one last chance
                    future.last_run();
                }
            }
        }
    }

    pub(crate) fn next(&self) -> Option<FutureHolder> {
        self.0.lock().pollable.pop_front()
    }
}
