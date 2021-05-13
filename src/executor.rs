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
use crossbeam_utils::sync::Parker;

/* The actual executor */
pub(crate) struct Executor {
    /* Generate a new id for each task */
    task_id: AtomicU64,
    worker: Worker<FutureHolder>,
    local_state: State,
    local_worker: Worker<FutureHolder>,
    parker: Parker,
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
        Self {
            task_id: AtomicU64::new(1),
            worker: Worker::new_fifo(),
            local_state: Default::default(),
            local_worker: Worker::new_fifo(),
            parker: Default::default(),
        }
    }

    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next(&self) -> Option<(FutureHolder, bool)> {
        self.local_worker.pop().map(|fut| (fut, true)).or_else(|| {
            self.worker
                .pop()
                .or_else(|| crate::EXECUTOR.steal(&self.worker))
                .map(|fut| (fut, false))
        })
    }

    pub(crate) fn push_future(&self, future: FutureHolder, local: bool) {
        if local {
            self.local_worker.push(future);
        } else {
            self.worker.push(future);
        }
        self.parker.unparker().unpark();
    }

    pub(crate) fn block_on<R: 'static, F: Future<Output = R> + 'static>(&self, future: F) -> R {
        match self.setup(future) {
            SetupResult::Ok(res) => res,
            SetupResult::Pending {
                receiver,
                main_task_exited,
            } => self.run(receiver, main_task_exited),
        }
    }

    fn setup<R: 'static, F: Future<Output = R> + 'static>(&self, future: F) -> SetupResult<R> {
        crate::EXECUTOR
            .register_current_thread(self.worker.stealer(), self.parker.unparker().clone());
        let main_task = MainTaskContext::new(self.parker.unparker().clone());
        let main_task_exited = main_task.exited();
        let handle = self._spawn(LocalFuture(future), Some(main_task));
        let mut receiver = Receiver::new(handle, self.parker.unparker().clone());
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
    ) -> R {
        loop {
            while let Some((future, local)) = self.next() {
                future.run(local, self.parker.unparker().clone());
            }
            if main_task_exited.load(Ordering::Acquire) {
                if let Some(res) = crate::EXECUTOR.poll_receiver(&mut receiver) {
                    return res.0;
                }
            }
            if self.local_worker.is_empty() && self.worker.is_empty() {
                if let Some(future) = crate::EXECUTOR.steal(&self.worker) {
                    future.run(false, self.parker.unparker().clone());
                } else {
                    self.parker.park();
                }
            }
        }
    }

    pub(crate) fn spawn<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        future: F,
    ) -> LocalJoinHandle<R> {
        self._spawn(future, None)
    }

    fn _spawn<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        future: F,
        main_task: Option<MainTaskContext>,
    ) -> LocalJoinHandle<R> {
        let future = LocalFuture(future);
        let id = self.next_task_id();
        let (sender, receiver) = flume::bounded(1);
        let future = FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send_async(res).await);
            },
            self.local_state.clone(),
            main_task,
        );
        let canceled = future.canceled();
        future.run(true, self.parker.unparker().clone());
        LocalJoinHandle(JoinHandle::new(
            id,
            receiver,
            self.local_state.clone(),
            canceled,
        ))
    }
}
