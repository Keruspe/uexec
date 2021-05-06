use crate::{
    future_holder::FutureHolder, main_task::MainTaskContext, receiver::Receiver, state::State,
    threads::Threads, JoinHandle, LocalFuture,
};

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    task::Poll,
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
        receiver: Receiver<R>,
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

    fn next(&self, local_executor: &Executor) -> Option<FutureHolder> {
        local_executor.state.next().or_else(|| self.state.next())
    }

    pub(crate) fn block_on<R: 'static, F: Future<Output = R> + 'static>(&self, future: F) -> R {
        crate::LOCAL_EXECUTOR.with(|local_executor| {
            crate::PARKER.with(|parker| match self.setup(future, local_executor, parker) {
                SetupResult::Ok(res) => res,
                SetupResult::Pending {
                    receiver,
                    main_task_exited,
                } => self.run(receiver, main_task_exited, local_executor, parker),
            })
        })
    }

    fn setup<R: 'static, F: Future<Output = R> + 'static>(
        &self,
        future: F,
        local_executor: &Executor,
        parker: &Parker,
    ) -> SetupResult<R> {
        self.threads.register_current(parker.unparker().clone());
        let main_task = MainTaskContext::new(parker.unparker().clone());
        let main_task_exited = main_task.exited();
        let handle = local_executor._spawn(LocalFuture(future), Some(main_task));
        let mut receiver = Receiver::new(handle, parker.unparker().clone());
        if let Some(res) = self.poll_receiver(&mut receiver) {
            SetupResult::Ok(res)
        } else {
            SetupResult::Pending {
                receiver,
                main_task_exited,
            }
        }
    }

    fn run<R: 'static>(
        &self,
        mut receiver: Receiver<R>,
        main_task_exited: Arc<AtomicBool>,
        local_executor: &Executor,
        parker: &Parker,
    ) -> R {
        loop {
            while let Some(future) = self.next(local_executor) {
                future.run();
            }
            if main_task_exited.load(Ordering::Acquire) {
                if let Some(res) = self.poll_receiver(&mut receiver) {
                    return res;
                }
            }
            if !self.state.has_pollable_tasks() {
                parker.park();
            }
        }
    }

    fn poll_receiver<R>(&self, receiver: &mut Receiver<R>) -> Option<R> {
        if let Poll::Ready(res) = receiver.poll() {
            self.threads.deregister_current();
            Some(res)
        } else {
            None
        }
    }

    pub(crate) fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
    ) -> JoinHandle<R> {
        self._spawn(future, None)
    }

    fn _spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
        main_task: Option<MainTaskContext>,
    ) -> JoinHandle<R> {
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
        JoinHandle::new(id, receiver, self.state.clone())
    }
}
