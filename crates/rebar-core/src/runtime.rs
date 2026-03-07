use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::process::mailbox::{Mailbox, MailboxRx};
use crate::process::table::{ProcessHandle, ProcessTable};
use crate::process::{Message, ProcessId, RegistryError, SendError};
use crate::router::{LocalRouter, MessageRouter};

/// Shared shutdown state with waker support for zero-latency cancellation.
struct ShutdownState {
    flag: Cell<bool>,
    waker: Cell<Option<Waker>>,
}

impl ShutdownState {
    fn new() -> Self {
        Self {
            flag: Cell::new(false),
            waker: Cell::new(None),
        }
    }

    fn shutdown(&self) {
        self.flag.set(true);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.flag.get()
    }
}

/// Future that resolves when the runtime begins shutting down.
struct CancelledFuture<'a> {
    state: &'a ShutdownState,
}

impl Future for CancelledFuture<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.state.is_shutting_down() {
            Poll::Ready(())
        } else {
            self.state.waker.set(Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

/// Context provided to each spawned process, giving it access to its own
/// PID, mailbox, and the ability to send messages to other processes.
pub struct ProcessContext {
    pid: ProcessId,
    rx: MailboxRx,
    router: Rc<dyn MessageRouter>,
    shutdown: Rc<ShutdownState>,
}

impl ProcessContext {
    /// Return this process's own PID.
    pub fn self_pid(&self) -> ProcessId {
        self.pid
    }

    /// Receive the next message from this process's mailbox.
    ///
    /// Returns `None` if the mailbox is closed (all senders dropped).
    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    /// Receive a message with a timeout.
    ///
    /// Returns `Some(msg)` if a message arrives within the duration,
    /// or `None` if the timeout expires or the mailbox is closed.
    pub async fn recv_timeout(&mut self, duration: Duration) -> Option<Message> {
        self.rx.recv_timeout(duration).await
    }

    /// Return a reference to the message router.
    pub fn router(&self) -> &Rc<dyn MessageRouter> {
        &self.router
    }

    /// Send a message to another process by PID.
    pub fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        self.router.route(self.pid, dest, payload)
    }

    /// Returns `true` if the runtime is shutting down.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_shutting_down()
    }

    /// Returns a future that completes when the runtime begins shutting down.
    ///
    /// Useful in `select!` to react to shutdown. Uses a proper waker
    /// pattern — no polling loop.
    pub async fn cancelled(&self) {
        CancelledFuture {
            state: &self.shutdown,
        }
        .await
    }
}

/// The Rebar runtime, responsible for spawning processes and routing messages.
///
/// In the thread-per-core model, each OS thread owns its own Runtime via `Rc`.
/// Uses `Rc` for shared ownership — `!Send` by design. All methods are synchronous.
pub struct Runtime {
    node_id: u64,
    #[allow(dead_code)]
    thread_id: u16,
    table: Rc<ProcessTable>,
    router: Rc<dyn MessageRouter>,
    shutdown: Rc<ShutdownState>,
    default_mailbox_capacity: Option<usize>,
}

impl Runtime {
    /// Create a new runtime for the given node ID.
    pub fn new(node_id: u64) -> Self {
        let table = Rc::new(ProcessTable::new(node_id, 0));
        let router = Rc::new(LocalRouter::new(Rc::clone(&table)));
        Self {
            node_id,
            thread_id: 0,
            table,
            router,
            shutdown: Rc::new(ShutdownState::new()),
            default_mailbox_capacity: None,
        }
    }

    /// Create a new runtime for the given node ID and thread ID.
    pub fn with_thread_id(node_id: u64, thread_id: u16) -> Self {
        let table = Rc::new(ProcessTable::new(node_id, thread_id));
        let router = Rc::new(LocalRouter::new(Rc::clone(&table)));
        Self {
            node_id,
            thread_id,
            table,
            router,
            shutdown: Rc::new(ShutdownState::new()),
            default_mailbox_capacity: None,
        }
    }

    /// Create a runtime with a custom message router.
    pub fn with_router(
        node_id: u64,
        table: Rc<ProcessTable>,
        router: Rc<dyn MessageRouter>,
    ) -> Self {
        Self {
            node_id,
            thread_id: 0,
            table,
            router,
            shutdown: Rc::new(ShutdownState::new()),
            default_mailbox_capacity: None,
        }
    }

    /// Set the default mailbox capacity for newly spawned processes.
    ///
    /// `None` (the default) uses unbounded mailboxes for backward compatibility.
    /// `Some(cap)` creates bounded mailboxes with the given capacity.
    pub fn with_mailbox_capacity(mut self, capacity: usize) -> Self {
        self.default_mailbox_capacity = Some(capacity);
        self
    }

    /// Return a reference to the process table.
    pub fn table(&self) -> &Rc<ProcessTable> {
        &self.table
    }

    /// Return this runtime's node ID.
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// Spawn a new process that runs the given async handler.
    ///
    /// The handler receives a `ProcessContext` and can use it to send/receive
    /// messages. Returns the new process's PID.
    ///
    /// In the thread-per-core model, spawn is synchronous — it allocates the
    /// PID, inserts into the process table, and enqueues the task on the local
    /// executor. The future need only be `'static`, not `Send`.
    pub fn spawn<F, Fut>(&self, handler: F) -> ProcessId
    where
        F: FnOnce(ProcessContext) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let pid = self.table.allocate_pid();
        let (tx, rx) = match self.default_mailbox_capacity {
            Some(cap) => Mailbox::bounded(cap),
            None => Mailbox::unbounded(),
        };

        self.table.insert(pid, ProcessHandle::new(tx));

        let ctx = ProcessContext {
            pid,
            rx,
            router: Rc::clone(&self.router),
            shutdown: Rc::clone(&self.shutdown),
        };

        let table = Rc::clone(&self.table);
        crate::executor::spawn(async move {
            handler(ctx).await;
            table.remove(&pid);
        })
        .detach();

        pid
    }

    /// Check whether a process is alive.
    pub fn is_alive(&self, pid: ProcessId) -> bool {
        self.table.is_alive(&pid)
    }

    /// Kill a process, closing its mailbox.
    pub fn kill(&self, pid: ProcessId) -> bool {
        self.table.kill(&pid)
    }

    /// Return a list of all live process IDs.
    pub fn list_processes(&self) -> Vec<ProcessId> {
        self.table.list_pids()
    }

    /// Send a message to a process by PID from outside any process context.
    ///
    /// Uses a synthetic PID of <node_id, 0, 0> as the sender.
    pub fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        let from = ProcessId::new(self.node_id, 0, 0);
        self.router.route(from, dest, payload)
    }

    /// Register a name for a process.
    pub fn register(&self, name: String, pid: ProcessId) -> Result<(), RegistryError> {
        self.table.register_name(name, pid)
    }

    /// Unregister a name, returning the PID it was associated with.
    pub fn unregister(&self, name: &str) -> Result<ProcessId, RegistryError> {
        self.table.unregister_name(name)
    }

    /// Look up a PID by its registered name.
    pub fn whereis(&self, name: &str) -> Option<ProcessId> {
        self.table.whereis(name)
    }

    /// Send a message to a named process.
    pub fn send_named(&self, name: &str, payload: rmpv::Value) -> Result<(), SendError> {
        let pid = self
            .table
            .whereis(name)
            .ok_or_else(|| SendError::NameNotFound(name.to_owned()))?;
        let from = ProcessId::new(self.node_id, 0, 0);
        self.router.route(from, pid, payload)
    }

    /// Signal the runtime to shut down.
    ///
    /// Immediately wakes any future waiting on `ProcessContext::cancelled()`.
    pub fn shutdown(&self) {
        self.shutdown.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorConfig, RebarExecutor};
    use crate::time::sleep;
    use std::cell::RefCell;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    #[test]
    fn spawn_returns_pid() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let pid = rt.spawn(|_ctx| async {});
            assert_eq!(pid.node_id(), 1);
            assert_eq!(pid.local_id(), 1);
        });
    }

    #[test]
    fn spawn_multiple_unique_pids() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let pid1 = rt.spawn(|_ctx| async {});
            let pid2 = rt.spawn(|_ctx| async {});
            assert_ne!(pid1, pid2);
        });
    }

    #[test]
    fn send_message_between_processes() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let result = Rc::new(RefCell::new(None));
            let result_clone = Rc::clone(&result);
            let receiver = rt.spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                *result_clone.borrow_mut() = Some(msg.payload().as_str().unwrap().to_string());
            });
            rt.spawn(move |ctx| async move {
                ctx.send(receiver, rmpv::Value::String("hello".into()))
                    .unwrap();
            });
            sleep(Duration::from_millis(50)).await;
            assert_eq!(*result.borrow(), Some("hello".to_string()));
        });
    }

    #[test]
    fn self_pid_is_correct() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let reported = Rc::new(Cell::new(None));
            let reported_clone = Rc::clone(&reported);
            let pid = rt.spawn(move |ctx| async move {
                reported_clone.set(Some(ctx.self_pid()));
            });
            sleep(Duration::from_millis(50)).await;
            assert_eq!(reported.get(), Some(pid));
        });
    }

    #[test]
    fn send_to_dead_process_returns_error() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let result = rt.send(ProcessId::new(1, 0, 999), rmpv::Value::Nil);
            assert!(result.is_err());
        });
    }

    #[test]
    fn recv_timeout_in_process() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let was_none = Rc::new(Cell::new(false));
            let was_none_clone = Rc::clone(&was_none);
            rt.spawn(move |mut ctx| async move {
                let result = ctx.recv_timeout(Duration::from_millis(10)).await;
                was_none_clone.set(result.is_none());
            });
            sleep(Duration::from_millis(100)).await;
            assert!(was_none.get());
        });
    }

    #[test]
    fn process_can_send_to_self() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let result = Rc::new(RefCell::new(None));
            let result_clone = Rc::clone(&result);
            rt.spawn(move |mut ctx| async move {
                let me = ctx.self_pid();
                ctx.send(me, rmpv::Value::String("self-msg".into()))
                    .unwrap();
                let msg = ctx.recv().await.unwrap();
                *result_clone.borrow_mut() = Some(msg.payload().as_str().unwrap().to_string());
            });
            sleep(Duration::from_millis(50)).await;
            assert_eq!(*result.borrow(), Some("self-msg".to_string()));
        });
    }

    #[test]
    fn chain_of_three_processes() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let result = Rc::new(Cell::new(0u64));
            let result_clone = Rc::clone(&result);
            let c = rt.spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                result_clone.set(msg.payload().as_u64().unwrap());
            });
            let b = rt.spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                let val = msg.payload().as_u64().unwrap();
                ctx.send(c, rmpv::Value::Integer((val + 1).into()))
                    .unwrap();
            });
            rt.spawn(move |ctx| async move {
                ctx.send(b, rmpv::Value::Integer(1u64.into())).unwrap();
            });
            sleep(Duration::from_millis(100)).await;
            assert_eq!(result.get(), 2);
        });
    }

    #[test]
    fn fan_out_fan_in() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let results = Rc::new(RefCell::new(Vec::new()));
            let mut workers = Vec::new();
            for _ in 0..5 {
                let results = Rc::clone(&results);
                let pid = rt.spawn(move |mut ctx| async move {
                    let msg = ctx.recv().await.unwrap();
                    let val = msg.payload().as_u64().unwrap();
                    results.borrow_mut().push(val * 2);
                });
                workers.push(pid);
            }
            rt.spawn(move |ctx| async move {
                for (i, pid) in workers.iter().enumerate() {
                    ctx.send(*pid, rmpv::Value::Integer((i as u64).into()))
                        .unwrap();
                }
            });
            sleep(Duration::from_millis(100)).await;
            let mut r = results.borrow().clone();
            r.sort();
            assert_eq!(r, vec![0, 2, 4, 6, 8]);
        });
    }

    #[test]
    fn spawn_100_processes() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let count = Rc::new(Cell::new(0u64));
            for _ in 0..100u64 {
                let count = Rc::clone(&count);
                rt.spawn(move |_ctx| async move {
                    count.set(count.get() + 1);
                });
            }
            sleep(Duration::from_millis(100)).await;
            assert_eq!(count.get(), 100);
        });
    }

    #[test]
    fn node_id_accessor() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(42);
            assert_eq!(rt.node_id(), 42);
        });
    }

    #[test]
    fn multiple_messages_to_same_process() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let result = Rc::new(Cell::new(0u64));
            let result_clone = Rc::clone(&result);
            let receiver = rt.spawn(move |mut ctx| async move {
                let mut sum = 0u64;
                for _ in 0..5 {
                    let msg = ctx.recv().await.unwrap();
                    sum += msg.payload().as_u64().unwrap();
                }
                result_clone.set(sum);
            });
            for i in 1..=5u64 {
                rt.send(receiver, rmpv::Value::Integer(i.into())).unwrap();
            }
            sleep(Duration::from_millis(100)).await;
            assert_eq!(result.get(), 15);
        });
    }

    #[test]
    fn process_context_send_returns_error_for_dead_target() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let was_err = Rc::new(Cell::new(false));
            let was_err_clone = Rc::clone(&was_err);
            rt.spawn(move |ctx| async move {
                let result = ctx.send(ProcessId::new(1, 0, 999), rmpv::Value::Nil);
                was_err_clone.set(result.is_err());
            });
            sleep(Duration::from_millis(50)).await;
            assert!(was_err.get());
        });
    }

    #[test]
    fn cancellation_flag_propagates() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let initially_not_shutting = Rc::new(Cell::new(false));
            let initially_clone = Rc::clone(&initially_not_shutting);

            rt.spawn(move |ctx| async move {
                initially_clone.set(!ctx.is_shutting_down());
                ctx.cancelled().await;
            });

            sleep(Duration::from_millis(50)).await;
            assert!(initially_not_shutting.get());

            rt.shutdown();
            sleep(Duration::from_millis(50)).await;
        });
    }

    #[test]
    fn bounded_mailbox_capacity_respected() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1).with_mailbox_capacity(2);
            let received = Rc::new(Cell::new(0u64));
            let received_clone = Rc::clone(&received);

            let receiver = rt.spawn(move |mut ctx| async move {
                sleep(Duration::from_millis(200)).await;
                let mut count = 0u64;
                while let Some(_msg) = ctx.recv_timeout(Duration::from_millis(50)).await {
                    count += 1;
                }
                received_clone.set(count);
            });

            let mut sent = 0u64;
            for i in 0..5u64 {
                match rt.send(receiver, rmpv::Value::Integer(i.into())) {
                    Ok(()) => sent += 1,
                    Err(SendError::MailboxFull(_)) => {}
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }

            assert_eq!(sent, 2);

            sleep(Duration::from_millis(500)).await;
            assert_eq!(received.get(), 2);
        });
    }

    #[test]
    fn unbounded_mailbox_default_preserved() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let received = Rc::new(Cell::new(0u64));
            let received_clone = Rc::clone(&received);

            let receiver = rt.spawn(move |mut ctx| async move {
                sleep(Duration::from_millis(100)).await;
                let mut count = 0u64;
                while let Some(_msg) = ctx.recv_timeout(Duration::from_millis(50)).await {
                    count += 1;
                }
                received_clone.set(count);
            });

            for i in 0..100u64 {
                rt.send(receiver, rmpv::Value::Integer(i.into())).unwrap();
            }

            sleep(Duration::from_millis(500)).await;
            assert_eq!(received.get(), 100);
        });
    }

    #[test]
    fn runtime_with_custom_router() {
        use crate::router::MessageRouter;
        use std::sync::atomic::{AtomicU64, Ordering};

        let ex = test_executor();
        ex.block_on(async {
            struct CountingRouter {
                count: AtomicU64,
                inner: crate::router::LocalRouter,
            }
            impl MessageRouter for CountingRouter {
                fn route(
                    &self,
                    from: ProcessId,
                    to: ProcessId,
                    payload: rmpv::Value,
                ) -> Result<(), SendError> {
                    self.count.fetch_add(1, Ordering::Relaxed);
                    self.inner.route(from, to, payload)
                }
            }

            let table = Rc::new(crate::process::table::ProcessTable::new(1, 0));
            let router = Rc::new(CountingRouter {
                count: AtomicU64::new(0),
                inner: crate::router::LocalRouter::new(Rc::clone(&table)),
            });
            let counter_ref = Rc::clone(&router);

            let rt = Runtime::with_router(1, Rc::clone(&table), router as Rc<dyn MessageRouter>);
            let result = Rc::new(RefCell::new(None));
            let result_clone = Rc::clone(&result);

            let receiver = rt.spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                *result_clone.borrow_mut() = Some(msg.payload().as_str().unwrap().to_string());
            });

            rt.spawn(move |ctx| async move {
                ctx.send(receiver, rmpv::Value::String("routed".into()))
                    .unwrap();
            });

            sleep(Duration::from_millis(100)).await;
            assert_eq!(*result.borrow(), Some("routed".to_string()));
            assert!(counter_ref.count.load(std::sync::atomic::Ordering::Relaxed) > 0);
        });
    }
}
