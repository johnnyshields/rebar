use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel;

use crate::bridge::{create_wake_fds, CrossThreadMessage, ThreadBridge, ThreadBridgeRouter};
use crate::process::table::ProcessTable;
use crate::runtime::Runtime;

/// Handle to a set of threaded runtimes.
///
/// Provides coordinated shutdown via a shared `AtomicBool`. When `shutdown()`
/// is called, all threads' drain tasks will detect the flag and trigger local
/// runtime shutdown.
pub struct ThreadedHandle {
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl ThreadedHandle {
    /// Signal all threads to shut down.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Wait for all threads to finish. Call after `shutdown()`.
    pub fn join(self) {
        for handle in self.threads {
            let _ = handle.join();
        }
    }

    /// Check if shutdown has been requested.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

/// Start a multi-threaded runtime with N OS threads, each running a rebar
/// executor event loop with its own `Rc<Runtime>` and `ThreadBridge` wiring.
///
/// The `init_fn` is called on each thread with a fully-wired `Rc<Runtime>`
/// that has a `ThreadBridgeRouter` for cross-thread message delivery.
///
/// Returns a `ThreadedHandle` for coordinated shutdown.
pub fn start_threaded<F>(node_id: u64, num_threads: usize, init_fn: F) -> ThreadedHandle
where
    F: Fn(Rc<Runtime>) + Send + Clone + 'static,
{
    let shutdown = Arc::new(AtomicBool::new(false));

    // Create crossbeam channels for each thread
    let mut senders = Vec::with_capacity(num_threads);
    let mut receivers = Vec::with_capacity(num_threads);
    for _ in 0..num_threads {
        let (tx, rx) = crossbeam_channel::unbounded::<CrossThreadMessage>();
        senders.push(tx);
        receivers.push(Some(rx));
    }
    let senders = Arc::new(senders);

    // Create wake fds for each thread
    let eventfds = Arc::new(create_wake_fds(num_threads));

    let mut threads = Vec::with_capacity(num_threads);

    for (thread_idx, receiver_slot) in receivers.iter_mut().enumerate() {
        let thread_id = thread_idx as u16;
        let senders = Arc::clone(&senders);
        let eventfds = Arc::clone(&eventfds);
        let local_rx = receiver_slot.take().unwrap();
        let shutdown = Arc::clone(&shutdown);
        let init_fn = init_fn.clone();

        let handle = std::thread::Builder::new()
            .name(format!("rebar-{}", thread_idx))
            .spawn(move || {
                let ex = crate::executor::RebarExecutor::new(
                    crate::executor::ExecutorConfig::default(),
                )
                .expect("failed to build RebarExecutor");

                ex.block_on(async move {
                    let table = Rc::new(ProcessTable::new(node_id, thread_id));
                    let bridge = Rc::new(ThreadBridge::new(
                        senders,
                        local_rx,
                        thread_id,
                        eventfds,
                    ));
                    let router = Rc::new(ThreadBridgeRouter::new(
                        thread_id,
                        Rc::clone(&table),
                        Rc::clone(&bridge),
                    ));
                    let runtime = Rc::new(
                        Runtime::with_router(node_id, Rc::clone(&table), router),
                    );

                    // Spawn drain task: periodically checks for cross-thread messages
                    // and shutdown flag
                    let bridge_drain = Rc::clone(&bridge);
                    let table_drain = Rc::clone(&table);
                    let shutdown_drain = Arc::clone(&shutdown);
                    let runtime_drain = Rc::clone(&runtime);
                    crate::executor::spawn(async move {
                        loop {
                            if shutdown_drain.load(Ordering::SeqCst) {
                                runtime_drain.shutdown();
                                break;
                            }
                            // Drain any pending cross-thread messages
                            bridge_drain.drain_into(&table_drain);
                            // Yield to allow other tasks to run, then check again
                            crate::time::sleep(std::time::Duration::from_micros(100)).await;
                        }
                    })
                    .detach();

                    init_fn(runtime);

                    // Keep the event loop alive until shutdown
                    while !shutdown.load(Ordering::SeqCst) {
                        crate::time::sleep(std::time::Duration::from_millis(10)).await;
                        bridge.drain_into(&table);
                    }
                    // Final drain
                    bridge.drain_into(&table);
                });
            })
            .expect("failed to spawn thread");

        threads.push(handle);
    }

    ThreadedHandle { shutdown, threads }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn start_threaded_basic() {
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = Arc::clone(&counter);

        let handle = start_threaded(1, 2, move |_rt| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Give threads time to initialize
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Both threads should have called init_fn
        assert_eq!(counter.load(Ordering::SeqCst), 2);

        handle.shutdown();
        handle.join();
    }

    #[test]
    fn start_threaded_shutdown() {
        let handle = start_threaded(1, 2, |_rt| {});
        assert!(!handle.is_shutting_down());
        handle.shutdown();
        assert!(handle.is_shutting_down());
        handle.join();
    }
}
