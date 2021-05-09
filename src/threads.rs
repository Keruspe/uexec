use crate::future_holder::FutureHolder;

use std::{
    collections::HashMap,
    sync::Arc,
    thread::{self, ThreadId},
};

use crossbeam_deque::{Steal, Stealer, Worker};
use crossbeam_utils::sync::Unparker;
use parking_lot::RwLock;

/* List of active threads */
#[derive(Clone, Default)]
pub(crate) struct Threads(Arc<RwLock<HashMap<ThreadId, (u8, Stealer<FutureHolder>, Unparker)>>>);

impl Threads {
    pub(crate) fn register_current(&self, stealer: Stealer<FutureHolder>, unparker: Unparker) {
        self.0
            .write()
            .entry(thread::current().id())
            .and_modify(|(count, _, _)| *count += 1)
            .or_insert((1, stealer, unparker));
    }

    fn deregister(&self, thread: ThreadId) {
        let mut threads = self.0.write();
        if let Some((count, stealer, unparker)) = threads.remove(&thread) {
            if count > 1 {
                threads.insert(thread, (count - 1, stealer, unparker));
            }
        }
    }

    pub(crate) fn deregister_current(&self) {
        self.deregister(thread::current().id());
        self.unpark_random();
    }

    pub(crate) fn unpark_random(&self) {
        let threads = self.0.read();
        if !threads.is_empty() {
            let i = fastrand::usize(..threads.len());
            threads.values().cycle().nth(i).unwrap().2.unpark();
        }
    }

    pub(crate) fn steal(
        &self,
        thread: ThreadId,
        worker: &Worker<FutureHolder>,
    ) -> Option<FutureHolder> {
        let threads = self.0.read();
        // Only consider the other threads, not the current one
        let threads_nb = threads.len() - 1;
        if threads_nb > 0 {
            let i = fastrand::usize(..threads_nb);
            for stealer in threads
                .iter()
                .filter(|(id, _)| **id != thread)
                .map(|(_, (_, stealer, _))| stealer)
                .cycle()
                .skip(i)
                .take(threads_nb)
            {
                loop {
                    match stealer.steal_batch_and_pop(worker) {
                        Steal::Success(future) => return Some(future),
                        Steal::Empty => break,
                        Steal::Retry => {}
                    }
                }
            }
        }
        None
    }
}
