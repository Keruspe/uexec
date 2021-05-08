use crate::{
    future_holder::FutureHolder, local_future::LocalRes, main_task::MainTaskContext,
    receiver::Receiver, state::State, threads::Threads, JoinHandle, LocalFuture, LocalJoinHandle,
};

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use crossbeam_utils::sync::Parker;

/* The actual executor */
#[derive(Default)]
pub(crate) struct Executor {
    /* Generate a new id for each task */
    task_id: AtomicU64,
    threads: Threads,
    state: State,
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
        crate::PARKER.with(|parker| Self {
            task_id: AtomicU64::new(1),
            threads: Threads::default().with_current(parker.unparker().clone()),
            state: Default::default(),
        })
    }

    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next(&self) -> Option<FutureHolder> {
        self.state.next().or_else(|| crate::EXECUTOR.next())
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
        crate::EXECUTOR.register_current_thread(parker.unparker().clone());
        let main_task = MainTaskContext::new(parker.unparker().clone());
        let main_task_exited = main_task.exited();
        let handle = self._spawn(LocalFuture(future), Some(main_task));
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
            while let Some(future) = self.next() {
                future.run();
            }
            if main_task_exited.load(Ordering::Acquire) {
                if let Some(res) = crate::EXECUTOR.poll_receiver(&mut receiver) {
                    return res.0;
                }
            }
            if !self.state.has_pollable_tasks() && !crate::EXECUTOR.has_pollable_tasks() {
                parker.park();
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
        FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send_async(res).await);
            },
            self.threads.clone(),
            self.state.clone(),
            main_task,
        )
        .run();
        LocalJoinHandle(JoinHandle::new(id, receiver, self.state.clone()))
    }
}
