use std::{future::Future, sync::OnceLock};

use tokio::runtime::Runtime;

pub static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn default_runtime() -> Runtime {
    Runtime::new().unwrap()
}

/// Runs a future to completion on runtime.
pub fn block_on<F: Future>(task: F) -> F::Output {
    let runtime = RUNTIME.get_or_init(default_runtime);
    runtime.block_on(task)
}

pub fn spawn<F>(task: F)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let runtime = RUNTIME.get_or_init(default_runtime);
    runtime.spawn(task);
}
