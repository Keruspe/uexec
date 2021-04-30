use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/* Unsafe wrapper for faking Send on local futures.
 * This is safe as we're guaranteed not to actually Send them. */
pub(crate) struct LocalFuture<R, F: Future<Output = R>>(pub(crate) F);
pub(crate) struct LocalRes<R>(pub(crate) R);

impl<R, F: Future<Output = R>> Future for LocalFuture<R, F> {
    type Output = LocalRes<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|this| &mut this.0) }
            .poll(cx)
            .map(LocalRes)
    }
}

unsafe impl<R, F: Future<Output = R>> Send for LocalFuture<R, F> {}
unsafe impl<R> Send for LocalRes<R> {}
