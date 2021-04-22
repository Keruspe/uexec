use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake},
    thread::{self, Thread},
};

use parking_lot::Mutex;

/* Automaticaly generate a new id for each new task */
thread_local! {
    static TASK_ID: AtomicU64 = AtomicU64::default();
}

fn next_task_id() -> u64 {
    TASK_ID.with(|generator| generator.fetch_add(1, Ordering::SeqCst))
}

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
    waker: Arc<MyWaker>,
}

/* The actual executor */
pub struct Executor {
    tasks: Tasks,
    thread: Thread,
    state: HashMap<u64, FutureHolder>,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            tasks: Tasks::default(),
            thread: thread::current(),
            state: HashMap::default(),
        }
    }
}

impl Executor {
    pub fn block_on<F: Future<Output = ()> + 'static>(&mut self, future: F) {
        let main_id = next_task_id();
        let waker = Arc::new(MyWaker::new(
            main_id,
            self.tasks.clone(),
            self.thread.clone(),
        ));
        let holder = FutureHolder {
            future: Box::pin(future),
            waker,
        };
        if self.poll_future(main_id, holder) {
            return;
        }
        loop {
            thread::park();
            while let Some(id) = self.tasks.next() {
                if let Some(future) = self.state.remove(&id) {
                    if self.poll_future(id, future) && main_id == id {
                        return;
                    }
                }
            }
        }
    }

    fn poll_future(&mut self, id: u64, mut future: FutureHolder) -> bool {
        let waker = future.waker.clone().into();
        let mut ctx = Context::from_waker(&waker);
        match future.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => true,
            Poll::Pending => {
                self.state.insert(id, future);
                false
            }
        }
    }
}
