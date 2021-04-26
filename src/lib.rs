// TODO: support canceling

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
#[derive(Default)]
struct State {
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
    fn next_task_id(&mut self) -> u64 {
        self.task_id += 1;
        self.task_id
    }

    fn register_main_task(&mut self, id: u64) {
        self.main_tasks.insert(id);
    }

    fn drop_main_task(&mut self, id: u64) {
        self.main_tasks.remove(&id);
    }

    fn register_future(&mut self, future: FutureHolder) {
        self.futures.insert(future.id, future);
    }

    fn register_pollable(&mut self, id: u64) {
        self.pollable.push_back(id);
    }

    fn cancel(&mut self, id: u64) {
        self.drop_main_task(id);
        let pollable_len = self.pollable.len();
        self.pollable.retain(|i| *i != id);
        if let Some(future) = self.futures.remove(&id) {
            // Only run the last poll if the future was pollable
            if self.pollable.len() != pollable_len {
                future.last_run();
            }
        }
    }

    fn next(&mut self) -> Option<FutureHolder> {
        let mut future = None;
        let mut attempts = VecDeque::new();
        while let Some(id) = self.pollable.pop_front() {
            future = self.futures.remove(&id);
            if future.is_some() {
                break;
            }
            attempts.push_back(id);
        }
        for attempt in attempts {
            self.pollable.push_back(attempt);
        }
        future
    }

    fn register_current_thread(&mut self) {
        self.threads.push(thread::current());
    }

    fn with_current_thread(mut self) -> Self {
        self.register_current_thread();
        self
    }

    fn deregister_current_thread(&mut self) {
        let current = thread::current().id();
        self.threads.retain(|thread| thread.id() != current);
    }

    fn peek_random_thread(&self) -> Thread {
        let i = fastrand::usize(..self.threads.len());
        self.threads[i].clone()
    }
}

/* Wake the executor after registering us in the list of tasks that need polling */
struct MyWaker {
    id: u64,
    state: Arc<Mutex<State>>,
}

impl MyWaker {
    fn new(id: u64, state: Arc<Mutex<State>>) -> Self {
        Self { id, state }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        let mut state = self.state.lock();
        state.register_pollable(self.id);
        state.peek_random_thread().unpark();
    }
}

/* Dummy waker */
struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: Arc<Self>) {}
}

static DUMMY_WAKER: Lazy<Waker> = Lazy::new(|| Arc::new(DummyWaker).into());

/* Holds the future and its waker */
struct FutureHolder {
    id: u64,
    state: Arc<Mutex<State>>,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    waker: Waker,
}

impl FutureHolder {
    fn new<F: Future<Output = ()> + Send + 'static>(
        id: u64,
        future: F,
        state: Arc<Mutex<State>>,
    ) -> Self {
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
                self.state.clone().lock().register_future(self);
                false
            }
        }
    }

    fn last_run(mut self) {
        let mut cx = Context::from_waker(&*DUMMY_WAKER);
        let _ = self.future.as_mut().poll(&mut cx);
    }
}

/* Unsafe wrapper for faking Send on local futures.
 * This is safe as we're guaranteed not to actually Send them. */
struct LocalFuture<R, F: Future<Output = R>>(F);

impl<R, F: Future<Output = R>> Future for LocalFuture<R, F> {
    type Output = LocalRes<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|this| &mut this.0) }
            .poll(cx)
            .map(LocalRes)
    }
}

unsafe impl<R, F: Future<Output = R>> Send for LocalFuture<R, F> {}

pub struct LocalRes<R>(R);

unsafe impl<R> Send for LocalRes<R> {}

/* Facility to return data from block_on */
struct Receiver<T> {
    handle: JoinHandle<T>,
    waker: Waker,
}

impl<T> Receiver<T> {
    fn new(handle: JoinHandle<T>, thread: Thread) -> Self {
        Self {
            handle,
            waker: Arc::new(ReceiverWaker(thread)).into(),
        }
    }

    fn poll(&mut self) -> Poll<T> {
        let mut ctx = Context::from_waker(&self.waker);
        Pin::new(&mut self.handle).poll(&mut ctx)
    }
}

struct ReceiverWaker(Thread);

impl Wake for ReceiverWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/* The actual executor */
#[derive(Clone, Default)]
struct Executor {
    state: Arc<Mutex<State>>,
}

impl Executor {
    fn local() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default().with_current_thread())),
        }
    }

    fn next(&self, local_executor: &Executor) -> Option<FutureHolder> {
        local_executor
            .state
            .lock()
            .next()
            .or_else(|| self.state.lock().next())
    }

    fn main_task_exited(&self, id: u64) -> bool {
        !self.state.lock().main_tasks.contains(&id)
    }

    fn has_pollable_tasks(&self) -> bool {
        !self.state.lock().pollable.is_empty()
    }

    fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(&self, future: F) -> R {
        self.state.lock().register_current_thread();
        let handle = self.spawn(future);
        let main_id = handle.id;
        let mut receiver = Receiver::new(handle, thread::current());
        if let Poll::Ready(res) = receiver.poll() {
            return res;
        } else {
            self.state.lock().register_main_task(main_id);
        }
        LOCAL_EXECUTOR.with(|local_executor| loop {
            while let Some(future) = self.next(local_executor) {
                let id = future.id;
                if future.run() {
                    self.state.lock().drop_main_task(id);
                }
            }
            if self.main_task_exited(main_id) {
                if let Poll::Ready(res) = receiver.poll() {
                    self.state.lock().deregister_current_thread();
                    return res;
                }
            }
            if !self.has_pollable_tasks() {
                thread::park();
            }
        })
    }

    fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
    ) -> JoinHandle<R> {
        let id = self.state.lock().next_task_id();
        let (sender, receiver) = async_channel::bounded(1);
        FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send(res).await);
            },
            self.state.clone(),
        )
        .run();
        JoinHandle {
            id,
            receiver,
            state: self.state.clone(),
        }
    }
}

/* Handle for waiting for spawned tasks completion */
pub struct JoinHandle<R> {
    id: u64,
    receiver: async_channel::Receiver<R>,
    state: Arc<Mutex<State>>,
}

impl<R> JoinHandle<R> {
    pub fn cancel(self) -> Option<R> {
        self.state.lock().cancel(self.id);
        self.receiver.try_recv().ok()
    }
}

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll_next(cx)
            .map(|res| res.expect("inner channel isn't expected to fail"))
    }
}

pub struct LocalJoinHandle<R>(JoinHandle<LocalRes<R>>);

impl<R> LocalJoinHandle<R> {
    pub fn cancel(self) -> Option<R> {
        self.0.cancel().map(|res| res.0)
    }
}

impl<R> Future for LocalJoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|res| res.0)
    }
}

/* Implicit global Executor */
static EXECUTOR: Lazy<Executor> = Lazy::new(Executor::default);

pub fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(future: F) -> R {
    EXECUTOR.block_on(future)
}

pub fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
    future: F,
) -> JoinHandle<R> {
    EXECUTOR.spawn(future)
}

pub fn worker() {
    block_on(future::pending())
}

/* Implicit State for futures local to this thread */
thread_local! {
    static LOCAL_EXECUTOR: Executor = Executor::local();
}

pub fn spawn_local<R: 'static, F: Future<Output = R> + 'static>(future: F) -> LocalJoinHandle<R> {
    let executor = LOCAL_EXECUTOR.with(|executor| executor.clone());
    LocalJoinHandle(executor.spawn(LocalFuture(future)))
}
