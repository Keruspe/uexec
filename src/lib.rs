//! uexec - simple work-stealing global and local executor
//!
//! The global executor is lazily spawned on first use.
//! It doesn't spawn any worker threads by default but you can use
//! [spawn_workers] to do that.
//!
//! # Examples
//!
//! ```
//! # use futures_lite::future;
//! // spawn several worker threads
//! uexec::spawn_workers(4);
//!
//! // spawn a task on the multi-threaded executor
//! let task1 = uexec::spawn(async {
//!     1 + 2
//! });
//! // spawn a task on the local executor (same thread)
//! let task2 = uexec::spawn_local(async {
//!     3 + 4
//! });
//! let task = future::zip(task1, task2);
//!
//! // run the executor
//! uexec::block_on(async {
//!     assert_eq!(task.await, (3, 7));
//! });
//!
//! // terminate our worker threads
//! uexec::terminate_workers();
//! ```

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

mod executor;
mod future_holder;
mod handle;
mod local_future;
mod main_task;
mod receiver;
mod state;
mod threads;
mod waker;
mod workers;

use executor::Executor;
use local_future::LocalFuture;
use waker::DummyWaker;
use workers::Workers;

use std::{future::Future, sync::Arc, task::Waker};

use crossbeam_utils::sync::Parker;
use once_cell::sync::Lazy;

/* Implicit global Executor */
static EXECUTOR: Lazy<Executor> = Lazy::new(Default::default);

/* Internal noop waker */
static DUMMY_WAKER: Lazy<Waker> = Lazy::new(|| Arc::new(DummyWaker).into());

/* List of wokers we spawned */
static WORKERS: Lazy<Workers> = Lazy::new(Default::default);

/* Implicit State for futures local to this thread */
thread_local! {
    static PARKER: Parker = Parker::new();
    static LOCAL_EXECUTOR: Executor = Executor::local();
}

pub use handle::{JoinHandle, LocalJoinHandle};

/// Runs the global and the local executor on the current thread until the given future is Ready
///
/// # Examples
///
/// ```
/// let task = uexec::spawn(async {
///     1 + 2
/// });
/// uexec::block_on(async {
///     assert_eq!(task.await, 3);
/// });
/// ```
pub fn block_on<R: 'static, F: Future<Output = R> + 'static>(future: F) -> R {
    LOCAL_EXECUTOR.with(|executor| executor.block_on(future))
}

/// Spawns a task onto the multi-threaded global executor.
///
/// # Examples
///
/// ```
/// # use futures_lite::future;
/// let task1 = uexec::spawn(async {
///     1 + 2
/// });
/// let task2 = uexec::spawn(async {
///     3 + 4
/// });
/// let task = future::zip(task1, task2);
///
/// uexec::block_on(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
    future: F,
) -> JoinHandle<R> {
    EXECUTOR.spawn(future)
}

/// Spawns a task onto the local executor.
///
/// The task does not need to be `Send` as it will be spawned on the same thread.
///
/// # Examples
///
/// ```
/// # use futures_lite::future;
/// let task1 = uexec::spawn_local(async {
///     1 + 2
/// });
/// let task2 = uexec::spawn_local(async {
///     3 + 4
/// });
/// let task = future::zip(task1, task2);
///
/// uexec::block_on(async {
///     assert_eq!(task.await, (3, 7));
/// });
/// ```
pub fn spawn_local<R: 'static, F: Future<Output = R> + 'static>(future: F) -> LocalJoinHandle<R> {
    LOCAL_EXECUTOR.with(|executor| LocalJoinHandle(executor.spawn(LocalFuture(future))))
}

/// Spawn new worker threads
///
/// New futures spawned using [spawn] may be executed on several of the spawned workers
///
/// # Examples
/// ```
/// uexec::spawn_workers(4);
/// # uexec::terminate_workers();
/// ```
pub fn spawn_workers(threads: u8) {
    WORKERS.spawn(threads);
}

/// Terminate all worker threads
///
/// Pending futures will be dropped
///
/// # Examples
/// ```
/// # uexec::spawn_workers(4);
/// uexec::terminate_workers();
/// ```
pub fn terminate_workers() {
    WORKERS.terminate();
}
