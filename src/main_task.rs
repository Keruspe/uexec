use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use parking::Unparker;

/* Context to properly exit block_on once the main task has exited */
pub(crate) struct MainTaskContext {
    exited: Arc<AtomicBool>,
    unparker: Unparker,
}

impl MainTaskContext {
    pub(crate) fn new(unparker: Unparker) -> Self {
        Self {
            exited: Arc::new(AtomicBool::new(false)),
            unparker,
        }
    }

    pub(crate) fn exited(&self) -> Arc<AtomicBool> {
        self.exited.clone()
    }

    pub(crate) fn exit(&self) {
        self.exited.store(true, Ordering::Release);
        self.unparker.unpark();
    }
}
