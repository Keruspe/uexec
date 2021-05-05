use crate::{local_future::LocalRes, state::State};

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Wait for a spawned task to complete or cancel it.
pub struct JoinHandle<R> {
    id: u64,
    receiver_fut: Pin<Box<dyn Future<Output = Result<R, flume::RecvError>>>>,
    state: State,
}

impl<R: 'static> JoinHandle<R> {
    pub(crate) fn new(id: u64, receiver: flume::Receiver<R>, state: State) -> Self {
        Self {
            id,
            receiver_fut: Box::pin(receiver.into_recv_async()),
            state,
        }
    }

    /// Cancel a spawned task, returning its result if it was finished
    pub fn cancel(mut self) -> Option<R> {
        let mut cx = Context::from_waker(&*crate::DUMMY_WAKER);
        self.state.cancel(self.id);
        match self.receiver_fut.as_mut().poll(&mut cx) {
            Poll::Ready(res) => res.ok(),
            Poll::Pending => None,
        }
    }
}

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.receiver_fut
            .as_mut()
            .poll(cx)
            .map(|res| res.expect("inner channel isn't expected to fail"))
    }
}

unsafe impl<R: Send> Send for JoinHandle<R> {}

/// Wait for a spawned local task to complete or cancel it.
pub struct LocalJoinHandle<R>(pub(crate) JoinHandle<LocalRes<R>>);

impl<R: 'static> LocalJoinHandle<R> {
    /// Cancel a spawned local task, returning its result if it was finished
    pub fn cancel(self) -> Option<R> {
        self.0.cancel().map(|res| res.0)
    }
}

impl<R> Future for LocalJoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|res| res.0)
    }
}
