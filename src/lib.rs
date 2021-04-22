use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use parking_lot::Mutex;

/* List of tasks that need polling */
#[derive(Clone, Default)]
struct Tasks(Arc<Mutex<VecDeque<u64>>>);

impl Tasks {
    fn register(&self, id: u64) {
        self.0.lock().push_back(id);
    }

    fn next(&self) -> Option<u64> {
        self.0.lock().pop_front()
    }
}

/* Wake the executor after registering us in the list of tasks that need polling */
struct MyWaker {
    id: u64,
    tasks: Tasks,
    thread: Thread,
}

impl MyWaker {
    fn new(id: u64, tasks: Tasks, thread: Thread) -> Self {
        Self { id, tasks, thread }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.tasks.register(self.id);
        self.thread.unpark();
    }
}

/* Holds the future and its waker */
struct FutureHolder {
    future: Pin<Box<dyn Future<Output = ()>>>,
    waker: Waker,
}

impl FutureHolder {
    fn poll(&mut self) -> Poll<()> {
        let mut ctx = Context::from_waker(&self.waker);
        self.future.as_mut().poll(&mut ctx)
    }
}

/* The actual executor */
struct Executor {
    task_id: AtomicU64,
    tasks: Tasks,
    thread: Thread,
    state: Mutex<HashMap<u64, FutureHolder>>,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            task_id: Default::default(),
            tasks: Tasks::default(),
            thread: thread::current(),
            state: Default::default(),
        }
    }
}

impl Executor {
    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::SeqCst)
    }

    fn wrap<F: Future<Output = ()> + 'static>(&self, id: u64, future: F) -> FutureHolder {
        let waker = Arc::new(MyWaker::new(id, self.tasks.clone(), self.thread.clone()));
        FutureHolder {
            future: Box::pin(future),
            waker: waker.into(),
        }
    }

    fn block_on<F: Future<Output = ()> + 'static>(&self, future: F) {
        let main_id = self.next_task_id();
        let future = self.wrap(main_id, future);
        if self.poll_future(main_id, future) {
            return;
        }
        loop {
            thread::park();
            while let Some(id) = self.tasks.next() {
                let future = self.state.lock().remove(&id);
                if let Some(future) = future {
                    if self.poll_future(id, future) && main_id == id {
                        return;
                    }
                }
            }
        }
    }

    fn spawn<F: Future<Output = ()> + 'static>(&self, future: F) {
        let id = self.next_task_id();
        let future = self.wrap(id, future);
        self.poll_future(id, future);
    }

    fn poll_future(&self, id: u64, mut future: FutureHolder) -> bool {
        match future.poll() {
            Poll::Ready(()) => true,
            Poll::Pending => {
                self.state.lock().insert(id, future);
                false
            }
        }
    }
}

/* Implicit executor for the current thread */
thread_local! {
    static EXECUTOR: Executor = Default::default();
}

pub fn block_on<F: Future<Output = ()> + 'static>(future: F) {
    EXECUTOR.with(|executor| executor.block_on(future))
}

pub fn spawn<F: Future<Output = ()> + 'static>(future: F) {
    EXECUTOR.with(|executor| executor.spawn(future))
}
