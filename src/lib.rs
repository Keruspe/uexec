#![warn(missing_docs, rust_2018_idioms)]
//! # uexec - minimal executor

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, ThreadId},
};

use futures_core::Stream;
use once_cell::sync::Lazy;
use parking::{Parker, Unparker};
use parking_lot::{Mutex, RwLock};

/* Implicit global Executor */
static EXECUTOR: Lazy<Executor> = Lazy::new(Default::default);

/* List of wokers we spawned */
static WORKERS: Lazy<Mutex<Vec<Worker>>> = Lazy::new(Default::default);

/* Implicit State for futures local to this thread */
thread_local! {
    static PARKER: Parker = Parker::new();
    static LOCAL_EXECUTOR: Executor = Executor::local();
}

/* List of active threads */
#[derive(Clone, Default)]
struct Threads(Arc<RwLock<HashMap<ThreadId, (u8, Unparker)>>>);

impl Threads {
    fn register_current(&self, unparker: Unparker) {
        self.0
            .write()
            .entry(thread::current().id())
            .and_modify(|(count, _)| *count += 1)
            .or_insert((1, unparker));
    }

    fn with_current(self, unparker: Unparker) -> Self {
        self.register_current(unparker);
        self
    }

    fn deregister(&self, thread: ThreadId) {
        let mut threads = self.0.write();
        if let Some((count, unparker)) = threads.remove(&thread) {
            if count > 1 {
                threads.insert(thread, (count - 1, unparker));
            }
        }
    }

    fn deregister_current(&self) {
        self.deregister(thread::current().id());
        self.unpark_random();
    }

    fn unpark_random(&self) {
        let threads = self.0.read();
        if !threads.is_empty() {
            let i = fastrand::usize(..threads.len());
            for (_, thread) in threads.values().skip(i).chain(threads.values().take(i)) {
                if thread.unpark() {
                    return;
                }
            }
        }
    }
}

/* The common state containing all the futures */
#[derive(Clone, Default)]
struct State(Arc<Mutex<Futures>>);

#[derive(Default)]
struct Futures {
    /* Pending futures */
    pending: HashMap<u64, FutureHolder>,
    /* List of tasks that need polling */
    pollable: VecDeque<FutureHolder>,
    /* List of tasks that are already pollable but not yet registered */
    pollable_next: HashSet<u64>,
}

impl State {
    fn register_future(&self, future: FutureHolder, threads: Threads) {
        let mut futures = self.0.lock();
        if futures.pollable_next.remove(&future.id) {
            futures.pollable.push_back(future);
            threads.unpark_random();
        } else {
            futures.pending.insert(future.id, future);
        }
    }

    fn register_pollable(&self, id: u64) {
        let mut futures = self.0.lock();
        if let Some(future) = futures.pending.remove(&id) {
            // Future was pending, mark it as pollable
            futures.pollable.push_back(future);
        } else {
            // Future hasn't been registered yet
            futures.pollable_next.insert(id);
        }
    }

    fn has_pollable_tasks(&self) -> bool {
        !self.0.lock().pollable.is_empty()
    }

    fn cancel(&self, id: u64) {
        let mut futures = self.0.lock();
        // If the future was pending, do nothing, otherwise...
        if futures.pending.remove(&id).is_none() {
            // ...check if it was pollable
            if let Some((index, _)) = futures
                .pollable
                .iter()
                .enumerate()
                .find(|(_, future)| future.id == id)
            {
                if let Some(future) = futures.pollable.remove(index) {
                    // If it was pollable, give it one last chance
                    future.last_run();
                }
            }
        }
    }

    fn next(&self) -> Option<FutureHolder> {
        self.0.lock().pollable.pop_front()
    }
}

/* Context to properly exit block_on once the main task has exited */
struct MainTaskContext {
    exited: Arc<AtomicBool>,
    unparker: Unparker,
}

impl MainTaskContext {
    fn new(unparker: Unparker) -> Self {
        Self {
            exited: Arc::new(AtomicBool::new(false)),
            unparker,
        }
    }

    fn exit(&self) {
        self.exited.store(true, Ordering::Release);
        self.unparker.unpark();
    }
}

/* Wake the executor after registering us in the list of tasks that need polling */
struct MyWaker {
    id: u64,
    threads: Threads,
    state: State,
}

impl MyWaker {
    fn new(id: u64, threads: Threads, state: State) -> Self {
        Self { id, threads, state }
    }
}

impl Wake for MyWaker {
    fn wake(self: Arc<Self>) {
        self.state.register_pollable(self.id);
        self.threads.unpark_random();
    }
}

/* Dummy waker */
struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: Arc<Self>) {}
}

/* Holds the future and its waker */
struct FutureHolder {
    id: u64,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    threads: Threads,
    state: State,
    waker: Waker,
    main_task: Option<MainTaskContext>,
}

impl FutureHolder {
    fn new<F: Future<Output = ()> + Send + 'static>(
        id: u64,
        future: F,
        threads: Threads,
        state: State,
        main_task: Option<MainTaskContext>,
    ) -> Self {
        let waker = Arc::new(MyWaker::new(id, threads.clone(), state.clone()));
        Self {
            id,
            future: Box::pin(future),
            threads,
            state,
            waker: waker.into(),
            main_task,
        }
    }

    fn run(mut self) {
        let mut ctx = Context::from_waker(&self.waker);
        match self.future.as_mut().poll(&mut ctx) {
            Poll::Ready(()) => {
                if let Some(main_task) = self.main_task {
                    main_task.exit();
                }
            }
            Poll::Pending => {
                let threads = self.threads.clone();
                self.state.clone().register_future(self, threads);
            }
        }
    }

    fn last_run(mut self) {
        static DUMMY_WAKER: Lazy<Waker> = Lazy::new(|| Arc::new(DummyWaker).into());
        let mut cx = Context::from_waker(&*DUMMY_WAKER);
        let _ = self.future.as_mut().poll(&mut cx);
        if let Some(main_task) = self.main_task {
            main_task.exit();
        }
    }
}

/* Unsafe wrapper for faking Send on local futures.
 * This is safe as we're guaranteed not to actually Send them. */
struct LocalFuture<R, F: Future<Output = R>>(F);
struct LocalRes<R>(R);

impl<R, F: Future<Output = R>> Future for LocalFuture<R, F> {
    type Output = LocalRes<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|this| &mut this.0) }
            .poll(cx)
            .map(LocalRes)
    }
}

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
    /* Generate a new id for each task */
    task_id: Arc<AtomicU64>,
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
    fn local() -> Self {
        PARKER.with(|parker| Self {
            task_id: Arc::new(AtomicU64::new(1)),
            threads: Threads::default().with_current(parker.unparker()),
            state: Default::default(),
        })
    }

    fn next_task_id(&self) -> u64 {
        self.task_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next(&self, local_executor: &Executor) -> Option<FutureHolder> {
        local_executor.state.next().or_else(|| self.state.next())
    }

    fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(&self, future: F) -> R {
        PARKER.with(|parker| match self.setup(future, parker) {
            SetupResult::Ok(res) => res,
            SetupResult::Pending {
                receiver,
                main_task_exited,
            } => self.run(receiver, main_task_exited, parker),
        })
    }

    fn setup<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
        &self,
        future: F,
        parker: &Parker,
    ) -> SetupResult<R> {
        self.threads.register_current(parker.unparker());
        let main_task = MainTaskContext::new(parker.unparker());
        let main_task_exited = main_task.exited.clone();
        let handle = self._spawn(future, Some(main_task));
        let mut receiver = Receiver::new(handle, parker.unparker());
        if let Some(res) = self.poll_receiver(&mut receiver) {
            SetupResult::Ok(res)
        } else {
            SetupResult::Pending {
                receiver,
                main_task_exited,
            }
        }
    }

    fn run<R: Send + 'static>(
        &self,
        mut receiver: Receiver<R>,
        main_task_exited: Arc<AtomicBool>,
        parker: &Parker,
    ) -> R {
        LOCAL_EXECUTOR.with(|local_executor| loop {
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
        })
    }

    fn poll_receiver<R>(&self, receiver: &mut Receiver<R>) -> Option<R> {
        if let Poll::Ready(res) = receiver.poll() {
            self.threads.deregister_current();
            Some(res)
        } else {
            None
        }
    }

    fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
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
        let (sender, receiver) = async_channel::bounded(1);
        FutureHolder::new(
            id,
            async move {
                let res = future.await;
                drop(sender.send(res).await);
            },
            self.threads.clone(),
            self.state.clone(),
            main_task,
        )
        .run();
        JoinHandle {
            id,
            receiver,
            state: self.state.clone(),
        }
    }
}

/// Wait for a spawned task to complete or cancel it.
pub struct JoinHandle<R> {
    id: u64,
    receiver: async_channel::Receiver<R>,
    state: State,
}

impl<R> JoinHandle<R> {
    /// Cancel a spawned task, returning its result if it was finished
    pub fn cancel(self) -> Option<R> {
        self.state.cancel(self.id);
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

/// Run a worker that will end once the given future is Ready on the current thread
pub fn block_on<R: Send + 'static, F: Future<Output = R> + Send + 'static>(future: F) -> R {
    EXECUTOR.block_on(future)
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

/// Spawn a Future on the current thread (thus not requiring it to be Send)
pub fn spawn_local<R: 'static, F: Future<Output = R> + 'static>(future: F) -> LocalJoinHandle<R> {
    LOCAL_EXECUTOR.with(|executor| LocalJoinHandle(executor.spawn(LocalFuture(future))))
}
