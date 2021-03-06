use crate::LocalJoinHandle;

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use crossbeam_utils::sync::Unparker;

/* Facility to return data from block_on */
pub(crate) struct Receiver<T> {
    handle: LocalJoinHandle<T>,
    waker: Waker,
}

impl<T> Receiver<T> {
    pub(crate) fn new(handle: LocalJoinHandle<T>, thread: Unparker) -> Self {
        Self {
            handle,
            waker: Arc::new(ReceiverWaker(thread)).into(),
        }
    }

    pub(crate) fn poll(&mut self) -> Poll<T> {
        let mut ctx = Context::from_waker(&self.waker);
        Pin::new(&mut self.handle).poll(&mut ctx)
    }
}

struct ReceiverWaker(Unparker);

impl Wake for ReceiverWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}
