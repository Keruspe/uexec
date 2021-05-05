#![warn(missing_docs, rust_2018_idioms)]
//! # uexec - minimal executor

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
use handle::{JoinHandle, LocalJoinHandle};
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

/// Run a worker that will end once the given future is Ready on the current thread
pub fn block_on<R: 'static, F: Future<Output = R> + 'static>(future: F) -> R {
    EXECUTOR.block_on(future)
}

/// Spawn a Future on the global executor ran by the pool of workers (block_on)
pub fn spawn<R: Send + 'static, F: Future<Output = R> + Send + 'static>(
    future: F,
) -> JoinHandle<R> {
    EXECUTOR.spawn(future)
}

/// Spawn a Future on the current thread (thus not requiring it to be Send)
pub fn spawn_local<R: 'static, F: Future<Output = R> + 'static>(future: F) -> LocalJoinHandle<R> {
    LOCAL_EXECUTOR.with(|executor| LocalJoinHandle(executor.spawn(LocalFuture(future))))
}

/// Run new worker threads
pub fn spawn_workers(threads: u8) {
    WORKERS.spawn(threads);
}

/// Terminate all worker threads
pub fn terminate_workers() {
    WORKERS.terminate();
}
