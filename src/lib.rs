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

/* The common state containing all the futures */
#[derive(Default)]
struct State(Mutex<HashMap<u64, FutureHolder>>);

impl State {
    fn register(&self, id: u64, future: FutureHolder) {
        self.0.lock().insert(id, future);
    }

    fn take(&self, id: u64) -> Option<FutureHolder> {
        self.0.lock().remove(&id)
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

/* Facility to return data from block_on */
struct Receiver<'a, T: 'a> {
    recv: Pin<Box<async_channel::Recv<'a, T>>>,
    waker: Waker,
}

impl<'a, T: 'a> Receiver<'a, T> {
    fn new(receiver: &'a async_channel::Receiver<T>, thread: Thread) -> Self {
        Self {
            recv: Box::pin(receiver.recv()),
            waker: Arc::new(ReceiverWaker(thread)).into(),
        }
    }

    fn poll(&mut self) -> Poll<T> {
        let mut ctx = Context::from_waker(&self.waker);
        self.recv.as_mut().poll(&mut ctx).map(|res| res.expect("channel not expected to fail"))
    }
}

struct ReceiverWaker(Thread);
impl Wake for ReceiverWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/* The actual executor */
struct Executor {
    task_id: AtomicU64,
    tasks: Tasks,
    thread: Thread,
    state: State,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            task_id: Default::default(),
            tasks: Default::default(),
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

    fn block_on<R: 'static, F: Future<Output = R> + 'static>(&self, future: F) -> R {
        let mut main_exited = false;
        let main_id = self.next_task_id();
        let (sender, receiver) = async_channel::bounded(1);
        let mut receiver = Receiver::new(&receiver, self.thread.clone());
        let future = self.wrap(main_id, async move {
            let res = future.await;
            drop(sender.send(res).await);
        });
        if self.poll_future(main_id, future) {
            main_exited = true;
        }
        loop {
            while let Some(id) = self.tasks.next() {
                let future = self.state.take(id);
                if let Some(future) = future {
                    if self.poll_future(id, future) && main_id == id {
                        main_exited = true;
                    }
                }
            }
            if main_exited {
                if let Poll::Ready(res) = receiver.poll() {
                    return res;
                }
            }
            thread::park();
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
                self.state.register(id, future);
                false
            }
        }
    }
}

/* Implicit executor for the current thread */
thread_local! {
    static EXECUTOR: Executor = Default::default();
}

pub fn block_on<R: 'static, F: Future<Output = R> + 'static>(future: F) -> R {
    EXECUTOR.with(|executor| executor.block_on(future))
}

pub fn spawn<F: Future<Output = ()> + 'static>(future: F) {
    EXECUTOR.with(|executor| executor.spawn(future))
}
