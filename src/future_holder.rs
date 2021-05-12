use crate::{main_task::MainTaskContext, state::State, waker::MyWaker};

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use crossbeam_deque::Worker;
use crossbeam_utils::sync::Unparker;

/* Holds the future and its waker */
pub(crate) struct FutureHolder {
    id: u64,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    state: State,
    worker: Option<Arc<Worker<Self>>>,
    pollable: Arc<AtomicBool>,
    canceled: Arc<AtomicBool>,
    main_task: Option<MainTaskContext>,
}

impl FutureHolder {
    pub(crate) fn new<F: Future<Output = ()> + Send + 'static>(
        id: u64,
        future: F,
        state: State,
        main_task: Option<MainTaskContext>,
    ) -> Self {
        Self {
            id,
            future: Box::pin(future),
            state,
            worker: None,
            pollable: Arc::new(AtomicBool::new(true)),
            canceled: Arc::new(AtomicBool::new(false)),
            main_task,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn canceled(&self) -> Arc<AtomicBool> {
        self.canceled.clone()
    }

    pub(crate) fn run(mut self, worker: Arc<Worker<Self>>, unparker: Unparker) {
        if self.canceled.load(Ordering::Acquire) {
            self.last_run();
            return;
        }

        let waker = Arc::new(MyWaker::new(
            self.id,
            Some(worker.clone()),
            Some(unparker),
            self.state.clone(),
            self.pollable.clone(),
        ))
        .into();
        self.worker = Some(worker);
        let mut ctx = Context::from_waker(&waker);
        match self.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => self.exit_main_task(),
            Poll::Pending => {
                if self.pollable.swap(false, Ordering::AcqRel) {
                    if let Some(worker) = self.worker.clone() {
                        worker.push(self);
                    }
                } else {
                    self.state.clone().register_future(self);
                }
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

unsafe impl Send for FutureHolder {}
unsafe impl Sync for FutureHolder {}
