use std::{future::Future, sync::OnceLock};

use tokio::runtime::Handle;
use tokio::runtime::Runtime;
pub static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn default_runtime() -> Runtime {
    Runtime::new().unwrap()
}

/// Runs a future to completion on runtime.
pub fn block_on<F: Future>(task: F) -> F::Output {
    // prefer current
    match Handle::try_current() {
        Ok(handle) => handle.block_on(task),
        Err(_) => {
            let runtime = RUNTIME.get_or_init(default_runtime);
            runtime.block_on(task)
        }
    }
}

pub fn spawn<F>(task: F)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // prefer current runtime
    match Handle::try_current() {
        Ok(handle) => {
            handle.spawn(task);
        }
        Err(_) => {
            let runtime = RUNTIME.get_or_init(default_runtime);
            runtime.spawn(task);
        }
    }
}