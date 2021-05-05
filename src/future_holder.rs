use crate::{main_task::MainTaskContext, state::State, threads::Threads, waker::MyWaker};

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

/* Holds the future and its waker */
pub(crate) struct FutureHolder {
    id: u64,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    threads: Threads,
    state: State,
    waker: Waker,
    main_task: Option<MainTaskContext>,
}

impl FutureHolder {
    pub(crate) fn new<F: Future<Output = ()> + Send + 'static>(
        id: u64,
        future: F,
        threads: Threads,
        state: State,
        main_task: Option<MainTaskContext>,
    ) -> Self {
        let waker = Arc::new(MyWaker::new(id, threads.clone(), state.clone())).into();
        Self {
            id,
            future: Box::pin(future),
            threads,
            state,
            waker,
            main_task,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn run(mut self) {
        let mut ctx = Context::from_waker(&self.waker);
        match self.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => self.exit_main_task(),
            Poll::Pending => {
                let threads = self.threads.clone();
                self.state.clone().register_future(self, threads);
            }
        }
    }

    pub(crate) fn last_run(mut self) {
        let mut cx = Context::from_waker(&*crate::DUMMY_WAKER);
        let _ = self.future.as_mut().poll(&mut cx);
        self.exit_main_task();
    }

    fn exit_main_task(self) {
        if let Some(main_task) = self.main_task {
            main_task.exit();
        }
    }
}
