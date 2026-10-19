//! The shared tokio runtime for networking. GTK stays on the main thread;
//! futures that only use channels may also be awaited on the GLib loop.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("linuxviewer-net")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Runs `future` on the network runtime and awaits its result from the GLib
/// main loop.
pub async fn run<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match runtime().spawn(future).await {
        Ok(output) => output,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// Fire-and-forget work on the network runtime.
pub fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime().spawn(future);
}
