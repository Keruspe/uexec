use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use parking_lot::Mutex;

/* List of active threads */
#[derive(Clone, Default)]
struct Threads(Arc<Mutex<Vec<Thread>>>);

impl Threads {
    fn register(&self) {
        self.0.lock().push(thread::current());
    }

    fn deregister(&self) {
        let current = thread::current().id();
        self.0.lock().retain(|thread| thread.id() != current);
    }

    fn peek(&self) -> Thread {
        let threads = self.0.lock();
        let i = fastrand::usize(..threads.len());
        threads[i].clone()
    }
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
    threads: Threads,
}

impl MyWaker {
    fn new(id: u64, tasks: Tasks, threads: Threads) -> Self {
        Self { id, tasks, threads }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.tasks.register(self.id);
        self.threads.peek().unpark();
    }
}

/* Holds the future and its waker */
struct FutureHolder {
    id: u64,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    waker: Waker,
}

impl FutureHolder {
    fn run(mut self, state: &State) -> bool {
        let mut ctx = Context::from_waker(&self.waker);
        match self.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => true,
            Poll::Pending => {
                state.register(self.id, self);
                false
            }
        }
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
        self.recv
            .as_mut()
            .poll(&mut ctx)
            .map(|res| res.expect("channel not expected to fail"))
    }
}

struct ReceiverWaker(Thread);

impl Wake for ReceiverWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/* The actual executor */
#[derive(Default)]
struct Executor {
    task_id: AtomicU64,
    main_exited: AtomicBool,
    tasks: Tasks,
    threads: Threads,
    state: State,
}

impl Executor {
    fn wrap<F: Future<Output = ()> + Send + 'static>(&self, future: F) -> FutureHolder {
        let id = self.task_id.fetch_add(1, Ordering::SeqCst);
        let waker = Arc::new(MyWaker::new(id, self.tasks.clone(), self.threads.clone()));
        FutureHolder {
            id,
            future: Box::pin(future),
            waker: waker.into(),
        }
    }

    fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(&self, future: F) -> R {
        self.threads.register();
        let (sender, receiver) = async_channel::bounded(1);
        let mut receiver = Receiver::new(&receiver, thread::current());
        let future = self.wrap(async move {
            let res = future.await;
            drop(sender.send(res).await);
        });
        let main_id = future.id;
        if future.run(&self.state) {
            self.main_exited.store(true, Ordering::SeqCst);
        }
        loop {
            while let Some(id) = self.tasks.next() {
                let future = self.state.take(id);
                if let Some(future) = future {
                    if future.run(&self.state) && main_id == id {
                        self.main_exited.store(true, Ordering::SeqCst);
                    }
                }
            }
            if self.main_exited.load(Ordering::SeqCst) {
                if let Poll::Ready(res) = receiver.poll() {
                    self.threads.deregister();
                    return res;
                }
            }
            thread::park();
        }
    }

    fn spawn<F: Future<Output = ()> + Send + 'static>(&self, future: F) {
        let future = self.wrap(future);
        future.run(&self.state);
    }
}

/* Implicit executor for the current thread */
thread_local! {
    static EXECUTOR: Executor = Default::default();
}

pub fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(future: F) -> R {
    EXECUTOR.with(|executor| executor.block_on(future))
}

pub fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) {
    EXECUTOR.with(|executor| executor.spawn(future))
}
