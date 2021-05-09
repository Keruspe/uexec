use crate::{
    future_holder::FutureHolder, local_future::LocalRes, main_task::MainTaskContext,
    receiver::Receiver, state::State, JoinHandle, LocalFuture, LocalJoinHandle,
};

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use crossbeam_deque::Worker;
use crossbeam_utils::sync::{Parker, Unparker};

/* The actual executor */
pub(crate) struct Executor {
    /* Generate a new id for each task */
    task_id: AtomicU64,
    worker: Arc<Worker<FutureHolder>>,
    local_state: State,
    local_worker: Arc<Worker<FutureHolder>>,
}

enum SetupResult<R> {
    Ok(R),
    Pending {
        receiver: Receiver<LocalRes<R>>,
        main_task_exited: Arc<AtomicBool>,
    },
}

impl Executor {
    pub(crate) fn local() -> Self {
        let worker = Worker::new_fifo();
        let local_worker = Worker::new_fifo();
        Self {
            task_id: AtomicU64::new(1),
            worker: Arc::new(worker),
            local_state: Default::default(),
            local_worker: Arc::new(local_worker),
        }
    }

    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next(&self) -> Option<(FutureHolder, Arc<Worker<FutureHolder>>)> {
        self.local_worker
            .pop()
            .map(|fut| (fut, self.local_worker.clone()))
            .or_else(|| {
                self.worker
                    .pop()
                    .or_else(|| crate::EXECUTOR.steal(&self.worker))
                    .map(|fut| (fut, self.worker.clone()))
            })
    }

    pub(crate) fn block_on<R: 'static, F: Future<Output = R> + 'static>(&self, future: F) -> R {
        crate::PARKER.with(|parker| match self.setup(future, parker) {
            SetupResult::Ok(res) => res,
            SetupResult::Pending {
                receiver,
                main_task_exited,
            } => self.run(receiver, main_task_exited, parker),
        })
    }

    fn setup<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        future: F,
        parker: &Parker,
    ) -> SetupResult<R> {
        crate::EXECUTOR.register_current_thread(self.worker.stealer(), parker.unparker().clone());
        let main_task = MainTaskContext::new(parker.unparker().clone());
        let main_task_exited = main_task.exited();
        let handle = self._spawn(
            LocalFuture(future),
            parker.unparker().clone(),
            Some(main_task),
        );
        let mut receiver = Receiver::new(handle, parker.unparker().clone());
        if let Some(res) = crate::EXECUTOR.poll_receiver(&mut receiver) {
            SetupResult::Ok(res.0)
        } else {
            SetupResult::Pending {
                receiver,
                main_task_exited,
            }
        }
    }

    fn run<R: 'static>(
        &self,
        mut receiver: Receiver<LocalRes<R>>,
        main_task_exited: Arc<AtomicBool>,
        parker: &Parker,
    ) -> R {
        loop {
            while let Some((future, worker)) = self.next() {
                future.run(worker, parker.unparker().clone());
            }
            if main_task_exited.load(Ordering::Acquire) {
                if let Some(res) = crate::EXECUTOR.poll_receiver(&mut receiver) {
                    return res.0;
                }
            }
            if self.local_worker.is_empty() && self.worker.is_empty() {
                if let Some(future) = crate::EXECUTOR.steal(&self.worker) {
                    future.run(self.worker.clone(), parker.unparker().clone());
                } else {
                    parker.park();
                }
            }
        }
    }

    pub(crate) fn spawn<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        unparker: Unparker,
        future: F,
    ) -> LocalJoinHandle<R> {
        self._spawn(future, unparker, None)
    }

    fn _spawn<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        future: F,
        unparker: Unparker,
        main_task: Option<MainTaskContext>,
    ) -> LocalJoinHandle<R> {
        let future = LocalFuture(future);
        let id = self.next_task_id();
        let (sender, receiver) = flume::bounded(1);
        FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send_async(res).await);
            },
            self.local_state.clone(),
            main_task,
        )
        .run(self.local_worker.clone(), unparker);
        LocalJoinHandle(JoinHandle::new(id, receiver, self.local_state.clone()))
    }
}
