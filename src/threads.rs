use std::{
    collections::HashMap,
    sync::Arc,
    thread::{self, ThreadId},
};

use parking::Unparker;
use parking_lot::RwLock;

/* List of active threads */
#[derive(Clone, Default)]
pub(crate) struct Threads(Arc<RwLock<HashMap<ThreadId, (u8, Unparker)>>>);

impl Threads {
    pub(crate) fn register_current(&self, unparker: Unparker) {
        self.0
            .write()
            .entry(thread::current().id())
            .and_modify(|(count, _)| *count += 1)
            .or_insert((1, unparker));
    }

    pub(crate) fn with_current(self, unparker: Unparker) -> Self {
        self.register_current(unparker);
        self
    }

    fn deregister(&self, thread: ThreadId) {
        let mut threads = self.0.write();
        if let Some((count, unparker)) = threads.remove(&thread) {
            if count > 1 {
                threads.insert(thread, (count - 1, unparker));
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
            for (_, thread) in threads.values().skip(i).chain(threads.values().take(i)) {
                if thread.unpark() {
                    return;
                }
            }
        }
    }
}
