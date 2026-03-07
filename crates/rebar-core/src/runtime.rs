use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::process::mailbox::{Mailbox, MailboxRx};
use crate::process::table::{ProcessHandle, ProcessTable};
use crate::process::{Message, ProcessId, RegistryError, SendError};
use crate::router::{LocalRouter, MessageRouter};

/// Context provided to each spawned process, giving it access to its own
/// PID, mailbox, and the ability to send messages to other processes.
pub struct ProcessContext {
    pid: ProcessId,
    rx: MailboxRx,
    router: Arc<dyn MessageRouter>,
    cancel_token: CancellationToken,
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
    pub fn router(&self) -> &Arc<dyn MessageRouter> {
        &self.router
    }

    /// Send a message to another process by PID.
    pub async fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        self.router.route(self.pid, dest, payload)
    }

    /// Returns `true` if the runtime is shutting down.
    pub fn is_shutting_down(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Returns a future that completes when the runtime begins shutting down.
    ///
    /// Useful in `tokio::select!` to react to shutdown:
    /// ```ignore
    /// tokio::select! {
    ///     msg = ctx.recv() => { /* handle message */ }
    ///     _ = ctx.cancelled() => { /* clean up and exit */ }
    /// }
    /// ```
    pub async fn cancelled(&self) {
        self.cancel_token.cancelled().await
    }
}

/// The Rebar runtime, responsible for spawning processes and routing messages.
pub struct Runtime {
    node_id: u64,
    table: Arc<ProcessTable>,
    router: Arc<dyn MessageRouter>,
    cancel_token: CancellationToken,
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    default_mailbox_capacity: Option<usize>,
}

impl Runtime {
    /// Create a new runtime for the given node ID.
    pub fn new(node_id: u64) -> Self {
        let table = Arc::new(ProcessTable::new(node_id));
        let router = Arc::new(LocalRouter::new(Arc::clone(&table)));
        Self {
            node_id,
            table,
            router,
            cancel_token: CancellationToken::new(),
            task_handles: Arc::new(Mutex::new(Vec::new())),
            default_mailbox_capacity: None,
        }
    }

    /// Create a runtime with a custom message router.
    pub fn with_router(
        node_id: u64,
        table: Arc<ProcessTable>,
        router: Arc<dyn MessageRouter>,
    ) -> Self {
        Self {
            node_id,
            table,
            router,
            cancel_token: CancellationToken::new(),
            task_handles: Arc::new(Mutex::new(Vec::new())),
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
    pub fn table(&self) -> &Arc<ProcessTable> {
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
    /// The spawned task is wrapped so that panics are caught and do not
    /// crash the runtime. After the handler completes (normally or via panic),
    /// the process is removed from the process table.
    pub async fn spawn<F, Fut>(&self, handler: F) -> ProcessId
    where
        F: FnOnce(ProcessContext) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pid = self.table.allocate_pid();
        let (tx, rx) = match self.default_mailbox_capacity {
            Some(cap) => Mailbox::bounded(cap),
            None => Mailbox::unbounded(),
        };

        let handle = ProcessHandle::new(tx);
        self.table.insert(pid, handle);

        let child_token = self.cancel_token.child_token();
        let ctx = ProcessContext {
            pid,
            rx,
            router: Arc::clone(&self.router),
            cancel_token: child_token,
        };

        let table = Arc::clone(&self.table);

        // Spawn a wrapper task that catches panics via the JoinHandle.
        // tokio::spawn catches panics in the spawned task and returns
        // JoinError instead of propagating them, so we spawn the handler
        // inside an inner task and await its JoinHandle.
        let join_handle = tokio::spawn(async move {
            let inner = tokio::spawn(handler(ctx));
            // Whether the handler completes normally or panics,
            // we always clean up by removing from the process table.
            let _ = inner.await;
            table.remove(&pid);
        });

        let mut handles = self.task_handles.lock().unwrap_or_else(|e| e.into_inner());
        // Prune completed handles to prevent unbounded growth
        handles.retain(|h| !h.is_finished());
        handles.push(join_handle);

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
    /// Uses a synthetic PID of <node_id, 0> as the sender.
    pub async fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        let from = ProcessId::new(self.node_id, 0);
        self.router.route(from, dest, payload)
    }

    /// Send a message with an ack channel that is signaled after processing.
    pub async fn send_with_ack(
        &self,
        dest: ProcessId,
        payload: rmpv::Value,
        ack: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), SendError> {
        let from = ProcessId::new(self.node_id, 0);
        self.router.route_with_ack(from, dest, payload, ack)
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
    pub async fn send_named(&self, name: &str, payload: rmpv::Value) -> Result<(), SendError> {
        let pid = self.table.whereis(name)
            .ok_or_else(|| SendError::NameNotFound(name.to_owned()))?;
        let from = ProcessId::new(self.node_id, 0);
        self.router.route(from, pid, payload)
    }

    /// Gracefully shut down the runtime, waiting for all processes to exit.
    ///
    /// Cancels the cancellation token (signaling all processes to stop),
    /// then waits up to `timeout` for all spawned tasks to complete.
    pub async fn shutdown(self, timeout: Duration) {
        self.cancel_token.cancel();
        let handles = std::mem::take(&mut *self.task_handles.lock().unwrap_or_else(|e| e.into_inner()));
        let _ = tokio::time::timeout(timeout, async {
            for h in handles {
                let _ = h.await;
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_returns_pid() {
        let rt = Runtime::new(1);
        let pid = rt.spawn(|_ctx| async {}).await;
        assert_eq!(pid.node_id(), 1);
        assert_eq!(pid.local_id(), 1);
    }

    #[tokio::test]
    async fn spawn_multiple_unique_pids() {
        let rt = Runtime::new(1);
        let pid1 = rt.spawn(|_ctx| async {}).await;
        let pid2 = rt.spawn(|_ctx| async {}).await;
        assert_ne!(pid1, pid2);
    }

    #[tokio::test]
    async fn send_message_between_processes() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let receiver = rt
            .spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                done_tx
                    .send(msg.payload().as_str().unwrap().to_string())
                    .unwrap();
            })
            .await;
        rt.spawn(move |ctx| async move {
            ctx.send(receiver, rmpv::Value::String("hello".into()))
                .await
                .unwrap();
        })
        .await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn self_pid_is_correct() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let pid = rt
            .spawn(move |ctx| async move {
                done_tx.send(ctx.self_pid()).unwrap();
            })
            .await;
        let reported = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pid, reported);
    }

    #[tokio::test]
    async fn send_to_dead_process_returns_error() {
        let rt = Runtime::new(1);
        let result = rt.send(ProcessId::new(1, 999), rmpv::Value::Nil).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn recv_timeout_in_process() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        rt.spawn(move |mut ctx| async move {
            let result = ctx.recv_timeout(std::time::Duration::from_millis(10)).await;
            done_tx.send(result.is_none()).unwrap();
        })
        .await;
        let was_none = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(was_none);
    }

    #[tokio::test]
    async fn process_can_send_to_self() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        rt.spawn(move |mut ctx| async move {
            let me = ctx.self_pid();
            ctx.send(me, rmpv::Value::String("self-msg".into()))
                .await
                .unwrap();
            let msg = ctx.recv().await.unwrap();
            done_tx
                .send(msg.payload().as_str().unwrap().to_string())
                .unwrap();
        })
        .await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, "self-msg");
    }

    #[tokio::test]
    async fn chain_of_three_processes() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let c = rt
            .spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                done_tx.send(msg.payload().as_u64().unwrap()).unwrap();
            })
            .await;
        let b = rt
            .spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                let val = msg.payload().as_u64().unwrap();
                ctx.send(c, rmpv::Value::Integer((val + 1).into()))
                    .await
                    .unwrap();
            })
            .await;
        rt.spawn(move |ctx| async move {
            ctx.send(b, rmpv::Value::Integer(1u64.into()))
                .await
                .unwrap();
        })
        .await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, 2);
    }

    #[tokio::test]
    async fn fan_out_fan_in() {
        let rt = Runtime::new(1);
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let mut workers = Vec::new();
        for _ in 0..5 {
            let tx = tx.clone();
            let pid = rt
                .spawn(move |mut ctx| async move {
                    let msg = ctx.recv().await.unwrap();
                    let val = msg.payload().as_u64().unwrap();
                    tx.send(val * 2).await.unwrap();
                })
                .await;
            workers.push(pid);
        }
        drop(tx);
        rt.spawn(move |ctx| async move {
            for (i, pid) in workers.iter().enumerate() {
                ctx.send(*pid, rmpv::Value::Integer((i as u64).into()))
                    .await
                    .unwrap();
            }
        })
        .await;
        let mut results = Vec::new();
        while let Ok(Some(val)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
        {
            results.push(val);
        }
        results.sort();
        assert_eq!(results, vec![0, 2, 4, 6, 8]);
    }

    #[tokio::test]
    async fn spawn_100_processes() {
        let rt = Runtime::new(1);
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        for i in 0..100u64 {
            let tx = tx.clone();
            rt.spawn(move |_ctx| async move {
                tx.send(i).await.unwrap();
            })
            .await;
        }
        drop(tx);
        let mut count = 0;
        while let Ok(Some(_)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
        {
            count += 1;
        }
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn process_panic_does_not_crash_runtime() {
        let rt = Runtime::new(1);
        rt.spawn(|_ctx| async move {
            panic!("intentional panic");
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        rt.spawn(move |_ctx| async move {
            done_tx.send(42u64).unwrap();
        })
        .await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn node_id_accessor() {
        let rt = Runtime::new(42);
        assert_eq!(rt.node_id(), 42);
    }

    #[tokio::test]
    async fn multiple_messages_to_same_process() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let receiver = rt
            .spawn(move |mut ctx| async move {
                let mut sum = 0u64;
                for _ in 0..5 {
                    let msg = ctx.recv().await.unwrap();
                    sum += msg.payload().as_u64().unwrap();
                }
                done_tx.send(sum).unwrap();
            })
            .await;
        for i in 1..=5u64 {
            rt.send(receiver, rmpv::Value::Integer(i.into()))
                .await
                .unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, 15);
    }

    #[tokio::test]
    async fn process_context_send_returns_error_for_dead_target() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        rt.spawn(move |ctx| async move {
            let result = ctx.send(ProcessId::new(1, 999), rmpv::Value::Nil).await;
            done_tx.send(result.is_err()).unwrap();
        })
        .await;
        let was_err = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(was_err);
    }

    #[tokio::test]
    async fn shutdown_awaits_processes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        let rt = Runtime::new(1);
        rt.spawn(move |ctx| async move {
            // Wait for cancellation, then set flag
            ctx.cancelled().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            flag_clone.store(true, Ordering::SeqCst);
        })
        .await;

        rt.shutdown(Duration::from_secs(5)).await;
        assert!(flag.load(Ordering::SeqCst), "process should have completed before shutdown returned");
    }

    #[tokio::test]
    async fn cancellation_token_propagates() {
        let rt = Runtime::new(1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        rt.spawn(move |ctx| async move {
            let shutting_down = ctx.is_shutting_down();
            tx.send(shutting_down).unwrap();
            // Keep running so we can test cancellation
            ctx.cancelled().await;
        })
        .await;

        // Process should report not shutting down initially
        let was_shutting_down = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!was_shutting_down);

        rt.shutdown(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn empty_runtime_shuts_down_immediately() {
        let rt = Runtime::new(1);
        let start = tokio::time::Instant::now();
        rt.shutdown(Duration::from_secs(5)).await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn bounded_mailbox_capacity_respected() {
        let rt = Runtime::new(1).with_mailbox_capacity(2);
        let (tx, rx) = tokio::sync::oneshot::channel();

        let receiver = rt
            .spawn(move |mut ctx| async move {
                // Don't read messages immediately - let mailbox fill up
                tokio::time::sleep(Duration::from_millis(200)).await;
                let mut count = 0u64;
                while let Some(_msg) = ctx.recv_timeout(Duration::from_millis(50)).await {
                    count += 1;
                }
                tx.send(count).unwrap();
            })
            .await;

        // Try to send 5 messages to a capacity-2 mailbox
        let mut sent = 0u64;
        for i in 0..5u64 {
            match rt.send(receiver, rmpv::Value::Integer(i.into())).await {
                Ok(()) => sent += 1,
                Err(SendError::MailboxFull(_)) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        // Only 2 should have been accepted (bounded capacity)
        assert_eq!(sent, 2);

        let received = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, 2);
    }

    #[tokio::test]
    async fn unbounded_mailbox_default_preserved() {
        let rt = Runtime::new(1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        let receiver = rt
            .spawn(move |mut ctx| async move {
                // Don't read immediately
                tokio::time::sleep(Duration::from_millis(100)).await;
                let mut count = 0u64;
                while let Some(_msg) = ctx.recv_timeout(Duration::from_millis(50)).await {
                    count += 1;
                }
                tx.send(count).unwrap();
            })
            .await;

        // Send 100 messages - should all succeed with unbounded mailbox
        for i in 0..100u64 {
            rt.send(receiver, rmpv::Value::Integer(i.into()))
                .await
                .unwrap();
        }

        let received = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, 100);
    }

    #[tokio::test]
    async fn runtime_with_custom_router() {
        use crate::router::MessageRouter;
        use std::sync::atomic::{AtomicU64, Ordering};

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

        let table = Arc::new(crate::process::table::ProcessTable::new(1));
        let router = Arc::new(CountingRouter {
            count: AtomicU64::new(0),
            inner: crate::router::LocalRouter::new(Arc::clone(&table)),
        });
        let counter_ref = Arc::clone(&router);

        let rt = Runtime::with_router(1, Arc::clone(&table), router as Arc<dyn MessageRouter>);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let receiver = rt
            .spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                done_tx
                    .send(msg.payload().as_str().unwrap().to_string())
                    .unwrap();
            })
            .await;

        rt.spawn(move |ctx| async move {
            ctx.send(receiver, rmpv::Value::String("routed".into()))
                .await
                .unwrap();
        })
        .await;

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, "routed");
        assert!(counter_ref.count.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn shutdown_timeout_enforced_on_hung_process() {
        let rt = Runtime::new(1);
        rt.spawn(|_ctx| async move {
            // Ignore cancellation token entirely — just sleep forever
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        })
        .await;

        let start = tokio::time::Instant::now();
        rt.shutdown(Duration::from_millis(100)).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "shutdown should have returned within 500ms, but took {:?}",
            elapsed,
        );
    }

    #[tokio::test]
    async fn shutdown_after_process_panic() {
        let rt = Runtime::new(1);
        rt.spawn(|_ctx| async move {
            panic!("intentional panic in shutdown test");
        })
        .await;

        // Give the panic time to resolve
        tokio::time::sleep(Duration::from_millis(50)).await;

        let start = tokio::time::Instant::now();
        rt.shutdown(Duration::from_secs(5)).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "shutdown after panic should complete quickly, but took {:?}",
            elapsed,
        );
    }
}
