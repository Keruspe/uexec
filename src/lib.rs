// TODO: support Output in spawn
// TODO: support canceling
// TODO: use spawn in block_on
// TODO: merge State and Executor

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::{self, Future},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
};

use futures_core::Stream;
use once_cell::sync::Lazy;
use parking_lot::Mutex;

/* The common state containing all the futures */
#[derive(Clone, Default)]
struct State(Arc<Mutex<StateInner>>);

#[derive(Default)]
struct StateInner {
    /* Generate a new id for each task */
    task_id: u64,
    /* The list of "main" block_on tasks */
    main_tasks: HashSet<u64>,
    /* Pending futures */
    futures: HashMap<u64, FutureHolder>,
    /* List of tasks that need polling */
    pollable: VecDeque<u64>,
    /* List of active threads */
    threads: Vec<Thread>,
}

impl State {
    fn next_task_id(&self) -> u64 {
        let mut inner = self.0.lock();
        let id = inner.task_id;
        inner.task_id += 1;
        id
    }

    fn register_main_task(&self, id: u64) {
        self.0.lock().main_tasks.insert(id);
    }

    fn drop_main_task(&self, id: u64) {
        self.0.lock().main_tasks.remove(&id);
    }

    fn main_task_exited(&self, id: u64) -> bool {
        !self.0.lock().main_tasks.contains(&id)
    }

    fn setup<F: Future<Output = ()> + Send + 'static>(&self, future: F) -> JoinHandle {
        let id = self.next_task_id();
        let (sender, receiver) = async_channel::bounded(1);
        FutureHolder::new(
            id,
            async move {
                future.await;
                drop(sender.send(()).await);
            },
            self.clone(),
        )
        .run();
        JoinHandle(receiver)
    }

    fn register(&self, future: FutureHolder) {
        self.0.lock().futures.insert(future.id, future);
    }

    fn pollable(&self, id: u64) {
        self.0.lock().pollable.push_back(id);
    }

    fn has_pollable_tasks(&self) -> bool {
        !self.0.lock().pollable.is_empty()
    }

    fn next(&self) -> Option<FutureHolder> {
        let mut inner = self.0.lock();
        let mut future = None;
        let mut attempts = VecDeque::new();
        while let Some(id) = inner.pollable.pop_front() {
            future = inner.futures.remove(&id);
            if future.is_some() {
                break;
            }
            attempts.push_back(id);
        }
        for attempt in attempts {
            inner.pollable.push_back(attempt);
        }
        future
    }

    fn with_current_thread(self) -> Self {
        self.register_thread();
        self
    }

    fn register_thread(&self) {
        self.0.lock().threads.push(thread::current());
    }

    fn deregister_thread(&self) {
        let current = thread::current().id();
        self.0
            .lock()
            .threads
            .retain(|thread| thread.id() != current);
    }

    fn random_thread(&self) -> Thread {
        let inner = self.0.lock();
        let i = fastrand::usize(..inner.threads.len());
        inner.threads[i].clone()
    }
}

/* Wake the executor after registering us in the list of tasks that need polling */
struct MyWaker {
    id: u64,
    state: State,
}

impl MyWaker {
    fn new(id: u64, state: State) -> Self {
        Self { id, state }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.state.pollable(self.id);
        self.state.random_thread().unpark();
    }
}

/* Holds the future and its waker */
struct FutureHolder {
    id: u64,
    state: State,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    waker: Waker,
}

impl FutureHolder {
    fn new<F: Future<Output = ()> + Send + 'static>(id: u64, future: F, state: State) -> Self {
        let waker = Arc::new(MyWaker::new(id, state.clone()));
        Self {
            id,
            state,
            future: Box::pin(future),
            waker: waker.into(),
        }
    }

    fn run(mut self) -> bool {
        let mut ctx = Context::from_waker(&self.waker);
        match self.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => true,
            Poll::Pending => {
                self.state.clone().register(self);
                false
            }
        }
    }
}

/* Unsafe wrapper for faking Send on local futures.
 * This is safe as we're guaranteed not to actually Send them. */
struct LocalFuture<F: Future<Output = ()>>(F);

impl<F: Future<Output = ()>> Future for LocalFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|this| &mut this.0) }.poll(cx)
    }
}

unsafe impl<F: Future<Output = ()>> Send for LocalFuture<F> {}

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
    state: State,
}

impl Executor {
    fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(&self, future: F) -> R {
        self.state.register_thread();
        let (sender, receiver) = async_channel::bounded(1);
        let mut receiver = Receiver::new(&receiver, thread::current());
        // Just register the waker
        drop(receiver.poll());
        let main_id = self.state.next_task_id();
        let future = FutureHolder::new(
            main_id,
            async move {
                let res = future.await;
                drop(sender.send(res).await);
            },
            self.state.clone(),
        );
        if !future.run() {
            self.state.register_main_task(main_id);
        }
        STATE.with(|local_state| loop {
            while let Some(future) = local_state.next().or_else(|| self.state.next()) {
                let id = future.id;
                if future.run() {
                    self.state.drop_main_task(id);
                }
            }
            if self.state.main_task_exited(main_id) {
                if let Poll::Ready(res) = receiver.poll() {
                    self.state.deregister_thread();
                    return res;
                }
            }
            if !self.state.has_pollable_tasks() {
                thread::park();
            }
        })
    }

    fn spawn<F: Future<Output = ()> + Send + 'static>(&self, future: F) -> JoinHandle {
        self.state.setup(future)
    }
}

/* Handle for waiting for spawned tasks completion */
pub struct JoinHandle(async_channel::Receiver<()>);

impl Future for JoinHandle {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll_next(cx).map(drop)
    }
}

/* Implicit global Executor */
static EXECUTOR: Lazy<Executor> = Lazy::new(Executor::default);

pub fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(future: F) -> R {
    EXECUTOR.block_on(future)
}

pub fn spawn<F: Future<Output = ()> + Send + 'static>(future: F) -> JoinHandle {
    EXECUTOR.spawn(future)
}

pub fn worker() {
    block_on(future::pending())
}

/* Implicit State for futures local to this thread */
thread_local! {
    static STATE: State = State::default().with_current_thread();
}

pub fn spawn_local<F: Future<Output = ()> + 'static>(future: F) -> JoinHandle {
    let state = STATE.with(|state| state.clone());
    state.setup(LocalFuture(future))
}
