use crate::future_holder::FutureHolder;

use std::{
    collections::HashMap,
    sync::Arc,
    thread::{self, ThreadId},
};

use crossbeam_deque::Stealer;
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

    pub(crate) fn with_current(self, stealer: Stealer<FutureHolder>, unparker: Unparker) -> Self {
        self.register_current(stealer, unparker);
        self
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
}
