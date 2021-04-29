#![warn(missing_docs, rust_2018_idioms)]
//! # uexec - minimal executor

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, ThreadId},
};

use futures_core::Stream;
use once_cell::sync::Lazy;
use parking::{Parker, Unparker};
use parking_lot::Mutex;

/* The list of "main" block_on tasks */
#[derive(Clone, Default)]
struct MainTasks(Arc<Mutex<HashMap<u64, Unparker>>>);

impl MainTasks {
    fn register(&self, id: u64, unparker: Unparker) {
        self.0.lock().insert(id, unparker);
    }

    fn contains(&self, id: u64) -> bool {
        self.0.lock().contains_key(&id)
    }

    fn remove(&self, id: u64) -> Option<Unparker> {
        self.0.lock().remove(&id)
    }

    fn drop(&self, id: u64) {
        if let Some(thread) = self.remove(id) {
            thread.unpark();
        }
    }
}

/* The common state containing all the futures */
#[derive(Default)]
struct State {
    /* Pending futures */
    futures: HashMap<u64, FutureHolder>,
    /* List of tasks that need polling */
    pollable: VecDeque<u64>,
    /* List of active threads */
    threads: HashMap<ThreadId, Unparker>,
}

impl State {
    fn register_future(&mut self, future: FutureHolder) {
        self.futures.insert(future.id, future);
    }

    fn drop_future(&mut self, id: u64) -> Option<FutureHolder> {
        self.pollable.retain(|i| *i != id);
        self.futures.remove(&id)
    }

    fn register_pollable(&mut self, id: u64) {
        if !self.pollable.contains(&id) {
            self.pollable.push_back(id);
        }
    }

    fn cancel(&mut self, id: u64) {
        let pollable_len = self.pollable.len();
        if let Some(future) = self.drop_future(id) {
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
            future = self.drop_future(id);
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

    fn register_current_thread(&mut self, unparker: Unparker) {
        self.threads.insert(thread::current().id(), unparker);
    }

    fn with_current_thread(mut self, unparker: Unparker) -> Self {
        self.register_current_thread(unparker);
        self
    }

    fn deregister_current_thread(&mut self) {
        self.threads.remove(&thread::current().id());
        self.unpark_random_thread();
    }

    fn unpark_random_thread(&self) {
        if !self.threads.is_empty() {
            let i = fastrand::usize(..self.threads.len());
            for thread in self
                .threads
                .values()
                .skip(i)
                .chain(self.threads.values().take(i))
            {
                if thread.unpark() {
                    return;
                }
            }
        }
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
        state.unpark_random_thread();
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

struct LocalRes<R>(R);

unsafe impl<R, F: Future<Output = R>> Send for LocalFuture<R, F> {}
unsafe impl<R> Send for LocalRes<R> {}

/* Facility to return data from block_on */
struct Receiver<T> {
    handle: JoinHandle<T>,
    waker: Waker,
}

impl<T> Receiver<T> {
    fn new(handle: JoinHandle<T>, thread: Unparker) -> Self {
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

struct ReceiverWaker(Unparker);

impl Wake for ReceiverWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/* The actual executor */
#[derive(Clone, Default)]
struct Executor {
    main_tasks: MainTasks,
    state: Arc<Mutex<State>>,
}

impl Executor {
    fn local() -> Self {
        PARKER.with(|parker| Self {
            main_tasks: Default::default(),
            state: Arc::new(Mutex::new(
                State::default().with_current_thread(parker.unparker()),
            )),
        })
    }

    fn next(&self, local_executor: &Executor) -> Option<FutureHolder> {
        local_executor
            .state
            .lock()
            .next()
            .or_else(|| self.state.lock().next())
    }

    fn main_task_exited(&self, id: u64) -> bool {
        !self.main_tasks.contains(id)
    }

    fn has_pollable_tasks(&self) -> bool {
        !self.state.lock().pollable.is_empty()
    }

    fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
        parker: &Parker,
        local_executor: &Executor,
    ) -> R {
        self.state.lock().register_current_thread(parker.unparker());
        let handle = self.spawn(future);
        let main_id = handle.id;
        let mut receiver = Receiver::new(handle, parker.unparker());
        self.main_tasks.register(main_id, parker.unparker());
        if let Some(res) = self.poll_receiver(&mut receiver) {
            self.main_tasks.drop(main_id);
            return res;
        }
        loop {
            while let Some(future) = self.next(local_executor) {
                let id = future.id;
                if future.run() {
                    self.main_tasks.drop(id);
                }
            }
            if self.main_task_exited(main_id) {
                if let Some(res) = self.poll_receiver(&mut receiver) {
                    return res;
                }
            }
            if !self.has_pollable_tasks() {
                parker.park();
            }
        }
    }

    fn poll_receiver<R>(&self, receiver: &mut Receiver<R>) -> Option<R> {
        if let Poll::Ready(res) = receiver.poll() {
            self.state.lock().deregister_current_thread();
            Some(res)
        } else {
            None
        }
    }

    fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
    ) -> JoinHandle<R> {
        let id = TASK_ID.fetch_add(1, Ordering::SeqCst);
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
            main_tasks: self.main_tasks.clone(),
            state: self.state.clone(),
        }
    }
}

/// Wait for a spawned task to complete or cancel it.
pub struct JoinHandle<R> {
    id: u64,
    receiver: async_channel::Receiver<R>,
    main_tasks: MainTasks,
    state: Arc<Mutex<State>>,
}

impl<R> JoinHandle<R> {
    /// Cancel a spawned task, returning its result if it was finished
    pub fn cancel(self) -> Option<R> {
        self.main_tasks.drop(self.id);
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

/// Wait for a spawned local task to complete or cancel it.
pub struct LocalJoinHandle<R>(JoinHandle<LocalRes<R>>);

impl<R> LocalJoinHandle<R> {
    /// Cancel a spawned local task, returning its result if it was finished
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

/* Generate a new id for each task */
static TASK_ID: AtomicU64 = AtomicU64::new(1);

/* Implicit global Executor */
static EXECUTOR: Lazy<Executor> = Lazy::new(Executor::default);

/// Run a worker that will end once the given future is Ready on the current thread
pub fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(future: F) -> R {
    PARKER
        .with(|parker| LOCAL_EXECUTOR.with(|executor| EXECUTOR.block_on(future, parker, executor)))
}

/// Spawn a Future on the global executor ran by the pool of workers (block_on)
pub fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
    future: F,
) -> JoinHandle<R> {
    EXECUTOR.spawn(future)
}

/* Control the pool of workers */
struct Worker {
    trigger: async_channel::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Worker {
    fn trigger_termination(&self) {
        let _ = self.trigger.try_send(());
    }

    fn terminate(self) {
        if let Err(err) = self.thread.join() {
            std::panic::resume_unwind(err);
        }
    }
}

static WORKERS: Lazy<Mutex<Vec<Worker>>> = Lazy::new(Default::default);

/// Run new worker threads
pub fn spawn_workers(threads: u8) {
    let mut workers = WORKERS.lock();
    for _ in 0..threads {
        let (sender, receiver) = async_channel::bounded(1);
        let handle = std::thread::spawn(|| {
            block_on(async move {
                let _ = receiver.recv().await;
            })
        });
        workers.push(Worker {
            trigger: sender,
            thread: handle,
        });
    }
}

/// Terminate all worker threads
pub fn terminate_workers() {
    let mut workers = WORKERS.lock();
    for worker in workers.iter() {
        worker.trigger_termination();
    }
    for worker in workers.drain(..) {
        worker.terminate();
    }
}

/* Implicit State for futures local to this thread */
thread_local! {
    static PARKER: Parker = Parker::new();
    static LOCAL_EXECUTOR: Executor = Executor::local();
}

/// Spawn a Future on the current thread (thus not requiring it to be Send)
pub fn spawn_local<R: 'static, F: Future<Output = R> + 'static>(future: F) -> LocalJoinHandle<R> {
    LOCAL_EXECUTOR.with(|executor| LocalJoinHandle(executor.spawn(LocalFuture(future))))
}
