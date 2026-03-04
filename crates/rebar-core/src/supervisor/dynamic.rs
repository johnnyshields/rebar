use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::process::{ExitReason, ProcessId};
use crate::runtime::Runtime;
use super::common::{check_restart_limit, shutdown_child_task};
use super::spec::{ChildSpec, RestartType, SupervisorError};
use super::engine::{ChildFactory, ChildEntry};

static DYN_CHILD_PID_COUNTER: AtomicU64 = AtomicU64::new(2_000_000);

/// Specification for a dynamic supervisor.
pub struct DynamicSupervisorSpec {
    pub max_children: Option<usize>,
    pub max_restarts: u32,
    pub max_seconds: u32,
}

impl DynamicSupervisorSpec {
    pub fn new() -> Self {
        Self {
            max_children: None,
            max_restarts: 3,
            max_seconds: 5,
        }
    }

    pub fn max_children(mut self, n: usize) -> Self {
        self.max_children = Some(n);
        self
    }

    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    pub fn max_seconds(mut self, n: u32) -> Self {
        self.max_seconds = n;
        self
    }
}

impl Default for DynamicSupervisorSpec {
    fn default() -> Self {
        Self::new()
    }
}

struct DynChildState {
    spec: ChildSpec,
    factory: ChildFactory,
    pid: ProcessId,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

enum DynSupervisorMsg {
    ChildExited { pid: ProcessId, reason: ExitReason },
    StartChild { entry: ChildEntry, reply: oneshot::Sender<Result<ProcessId, SupervisorError>> },
    TerminateChild { pid: ProcessId, reply: oneshot::Sender<Result<(), SupervisorError>> },
    CountChildren { reply: oneshot::Sender<DynChildCounts> },
    WhichChildren { reply: oneshot::Sender<Vec<DynChildInfo>> },
    Shutdown,
}

/// Counts of children in a dynamic supervisor.
#[derive(Debug, Clone)]
pub struct DynChildCounts {
    pub active: usize,
}

/// Information about a child in a dynamic supervisor.
#[derive(Debug, Clone)]
pub struct DynChildInfo {
    pub pid: ProcessId,
    pub id: String,
    pub restart: RestartType,
}

/// Handle to a running dynamic supervisor.
#[derive(Clone)]
pub struct DynamicSupervisorHandle {
    pid: ProcessId,
    msg_tx: mpsc::UnboundedSender<DynSupervisorMsg>,
}

impl DynamicSupervisorHandle {
    /// The supervisor's own process ID.
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Start a new child in the dynamic supervisor.
    pub async fn start_child(&self, entry: ChildEntry) -> Result<ProcessId, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::StartChild { entry, reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    /// Terminate a child by PID.
    pub async fn terminate_child(&self, pid: ProcessId) -> Result<(), SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::TerminateChild { pid, reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    /// Get count of active children.
    pub async fn count_children(&self) -> Result<DynChildCounts, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::CountChildren { reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)
    }

    /// Get information about all children.
    pub async fn which_children(&self) -> Result<Vec<DynChildInfo>, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::WhichChildren { reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)
    }

    /// Request the dynamic supervisor to shut down.
    pub fn shutdown(&self) {
        let _ = self.msg_tx.send(DynSupervisorMsg::Shutdown);
    }
}

/// Start a dynamic supervisor as a process in the given runtime.
pub async fn start_dynamic_supervisor(
    runtime: Arc<Runtime>,
    spec: DynamicSupervisorSpec,
) -> DynamicSupervisorHandle {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let msg_tx_clone = msg_tx.clone();

    let pid = runtime
        .spawn(move |_ctx| async move {
            dynamic_supervisor_loop(spec, msg_rx, msg_tx_clone).await;
        })
        .await;

    DynamicSupervisorHandle { pid, msg_tx }
}

async fn dynamic_supervisor_loop(
    spec: DynamicSupervisorSpec,
    mut msg_rx: mpsc::UnboundedReceiver<DynSupervisorMsg>,
    msg_tx: mpsc::UnboundedSender<DynSupervisorMsg>,
) {
    let mut children: HashMap<ProcessId, DynChildState> = HashMap::new();
    let mut restart_times = std::collections::VecDeque::new();
    let max_restarts = spec.max_restarts;
    let max_seconds = spec.max_seconds;
    let max_children = spec.max_children;

    loop {
        match msg_rx.recv().await {
            Some(DynSupervisorMsg::StartChild { entry, reply }) => {
                if let Some(limit) = max_children
                    && children.len() >= limit
                {
                    let _ = reply.send(Err(SupervisorError::MaxChildren));
                    continue;
                }

                let (child_pid, shutdown_tx, join_handle) =
                    spawn_dyn_child(&entry.factory, &msg_tx);
                let child_state = DynChildState {
                    spec: entry.spec,
                    factory: entry.factory,
                    pid: child_pid,
                    shutdown_tx: Some(shutdown_tx),
                    join_handle: Some(join_handle),
                };
                children.insert(child_pid, child_state);
                let _ = reply.send(Ok(child_pid));
            }
            Some(DynSupervisorMsg::ChildExited { pid, reason }) => {
                let child = match children.remove(&pid) {
                    Some(c) => c,
                    None => continue,
                };

                if !child.spec.restart.should_restart(&reason) {
                    continue;
                }

                // Check restart limit
                if !check_restart_limit(&mut restart_times, max_restarts, max_seconds) {
                    shutdown_all_dyn_children(&mut children).await;
                    break;
                }

                // Restart the child
                let (new_pid, shutdown_tx, join_handle) =
                    spawn_dyn_child(&child.factory, &msg_tx);
                let new_state = DynChildState {
                    spec: child.spec,
                    factory: child.factory,
                    pid: new_pid,
                    shutdown_tx: Some(shutdown_tx),
                    join_handle: Some(join_handle),
                };
                children.insert(new_pid, new_state);
            }
            Some(DynSupervisorMsg::TerminateChild { pid, reply }) => {
                match children.remove(&pid) {
                    Some(mut child) => {
                        stop_dyn_child(&mut child).await;
                        let _ = reply.send(Ok(()));
                    }
                    None => {
                        let _ = reply.send(Err(SupervisorError::NotFound(pid.to_string())));
                    }
                }
            }
            Some(DynSupervisorMsg::CountChildren { reply }) => {
                let _ = reply.send(DynChildCounts {
                    active: children.len(),
                });
            }
            Some(DynSupervisorMsg::WhichChildren { reply }) => {
                let infos = children
                    .values()
                    .map(|c| DynChildInfo {
                        pid: c.pid,
                        id: c.spec.id.clone(),
                        restart: c.spec.restart,
                    })
                    .collect();
                let _ = reply.send(infos);
            }
            Some(DynSupervisorMsg::Shutdown) => {
                shutdown_all_dyn_children(&mut children).await;
                break;
            }
            None => {
                shutdown_all_dyn_children(&mut children).await;
                break;
            }
        }
    }
}

fn spawn_dyn_child(
    factory: &ChildFactory,
    msg_tx: &mpsc::UnboundedSender<DynSupervisorMsg>,
) -> (ProcessId, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let local_id = DYN_CHILD_PID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = ProcessId::new(0, local_id);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let factory = Arc::clone(factory);
    let msg_tx = msg_tx.clone();

    let handle = tokio::spawn(async move {
        let child_future = factory();
        tokio::select! {
            reason = child_future => {
                let _ = msg_tx.send(DynSupervisorMsg::ChildExited { pid, reason });
            }
            _ = shutdown_rx => {
                // Shutdown requested
            }
        }
    });

    (pid, shutdown_tx, handle)
}

async fn stop_dyn_child(child: &mut DynChildState) {
    let shutdown_tx = child.shutdown_tx.take();
    let join_handle = child.join_handle.take();
    shutdown_child_task(&child.spec.shutdown, shutdown_tx, join_handle).await;
}

async fn shutdown_all_dyn_children(children: &mut HashMap<ProcessId, DynChildState>) {
    for (_, mut child) in children.drain() {
        stop_dyn_child(&mut child).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::spec::ShutdownStrategy;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as AtomicOrdering};
    use std::time::Duration;

    fn make_runtime() -> Arc<Runtime> {
        Arc::new(Runtime::new(1))
    }

    #[tokio::test]
    async fn start_child_adds_child_dynamically() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(ChildSpec::new("worker1"), || async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let pid = handle.start_child(entry).await.unwrap();
        assert_eq!(pid.node_id(), 0);

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 1);

        handle.shutdown();
    }

    #[tokio::test]
    async fn terminate_child_removes_by_pid() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(ChildSpec::new("worker1"), || async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let pid = handle.start_child(entry).await.unwrap();

        let result = handle.terminate_child(pid).await;
        assert!(result.is_ok());

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        handle.shutdown();
    }

    #[tokio::test]
    async fn terminate_nonexistent_child_returns_error() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let fake_pid = ProcessId::new(0, 999_999);
        let result = handle.terminate_child(fake_pid).await;
        assert!(result.is_err());

        handle.shutdown();
    }

    #[tokio::test]
    async fn max_children_limit_enforced() {
        let rt = make_runtime();
        let spec = DynamicSupervisorSpec::new().max_children(2);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let e1 = ChildEntry::new(ChildSpec::new("w1"), || async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let e2 = ChildEntry::new(ChildSpec::new("w2"), || async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let e3 = ChildEntry::new(ChildSpec::new("w3"), || async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        assert!(handle.start_child(e1).await.is_ok());
        assert!(handle.start_child(e2).await.is_ok());
        let result = handle.start_child(e3).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SupervisorError::MaxChildren));

        handle.shutdown();
    }

    #[tokio::test]
    async fn crashed_permanent_child_is_restarted() {
        let rt = make_runtime();
        let restart_count = Arc::new(AtomicU32::new(0));
        let restart_count_clone = Arc::clone(&restart_count);

        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let entry = ChildEntry::new(
            ChildSpec::new("crasher").restart(RestartType::Permanent),
            move || {
                let count = Arc::clone(&restart_count_clone);
                async move {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    ExitReason::Abnormal("crash".into())
                }
            },
        );
        handle.start_child(entry).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let count = restart_count.load(AtomicOrdering::SeqCst);
        assert!(count > 1, "expected restarts, got count={count}");

        handle.shutdown();
    }

    #[tokio::test]
    async fn crashed_transient_child_restarts_on_abnormal() {
        let rt = make_runtime();
        let restart_count = Arc::new(AtomicU32::new(0));
        let restart_count_clone = Arc::clone(&restart_count);

        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let entry = ChildEntry::new(
            ChildSpec::new("transient_crasher").restart(RestartType::Transient),
            move || {
                let count = Arc::clone(&restart_count_clone);
                async move {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    ExitReason::Abnormal("crash".into())
                }
            },
        );
        handle.start_child(entry).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let count = restart_count.load(AtomicOrdering::SeqCst);
        assert!(
            count > 1,
            "transient child should restart on abnormal exit, got count={count}"
        );

        handle.shutdown();
    }

    #[tokio::test]
    async fn crashed_transient_child_not_restarted_on_normal() {
        let rt = make_runtime();
        let start_count = Arc::new(AtomicU32::new(0));
        let start_count_clone = Arc::clone(&start_count);

        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let entry = ChildEntry::new(
            ChildSpec::new("transient_normal").restart(RestartType::Transient),
            move || {
                let count = Arc::clone(&start_count_clone);
                async move {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    ExitReason::Normal
                }
            },
        );
        handle.start_child(entry).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let count = start_count.load(AtomicOrdering::SeqCst);
        assert_eq!(count, 1, "transient child should not restart on normal exit");

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        handle.shutdown();
    }

    #[tokio::test]
    async fn crashed_temporary_child_never_restarted() {
        let rt = make_runtime();
        let start_count = Arc::new(AtomicU32::new(0));
        let start_count_clone = Arc::clone(&start_count);

        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let entry = ChildEntry::new(
            ChildSpec::new("temporary").restart(RestartType::Temporary),
            move || {
                let count = Arc::clone(&start_count_clone);
                async move {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    ExitReason::Abnormal("crash".into())
                }
            },
        );
        handle.start_child(entry).await.unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let count = start_count.load(AtomicOrdering::SeqCst);
        assert_eq!(count, 1, "temporary child should never restart");

        handle.shutdown();
    }

    #[tokio::test]
    async fn count_children_returns_correct_count() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        for i in 0..3 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{i}")), || async {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
            handle.start_child(entry).await.unwrap();
        }

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 3);

        handle.shutdown();
    }

    #[tokio::test]
    async fn which_children_returns_correct_info() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(
            ChildSpec::new("my_worker").restart(RestartType::Transient),
            || async {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            },
        );
        let pid = handle.start_child(entry).await.unwrap();

        let infos = handle.which_children().await.unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].pid, pid);
        assert_eq!(infos[0].id, "my_worker");
        assert!(matches!(infos[0].restart, RestartType::Transient));

        handle.shutdown();
    }

    #[tokio::test]
    async fn restart_limit_causes_supervisor_shutdown() {
        let rt = make_runtime();
        let spec = DynamicSupervisorSpec::new()
            .max_restarts(2)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(rt, spec).await;

        let entry = ChildEntry::new(
            ChildSpec::new("crasher").restart(RestartType::Permanent),
            || async { ExitReason::Abnormal("crash".into()) },
        );
        handle.start_child(entry).await.unwrap();

        // Wait for restart limit to be exceeded and supervisor to shut down
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Supervisor should be gone
        let result = handle.count_children().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shutdown_stops_all_children() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        for i in 0..5 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{i}")), || async {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
            handle.start_child(entry).await.unwrap();
        }

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 5);

        handle.shutdown();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = handle.count_children().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn brutal_kill_terminates_child_immediately() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;
        let started = Arc::new(AtomicBool::new(false));
        let started_clone = Arc::clone(&started);

        let entry = ChildEntry::new(
            ChildSpec::new("brutal")
                .restart(RestartType::Temporary)
                .shutdown(ShutdownStrategy::BrutalKill),
            move || {
                let s = Arc::clone(&started_clone);
                async move {
                    s.store(true, AtomicOrdering::SeqCst);
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                }
            },
        );
        let pid = handle.start_child(entry).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(started.load(AtomicOrdering::SeqCst));

        let before = std::time::Instant::now();
        handle.terminate_child(pid).await.unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        handle.shutdown();
    }

    #[tokio::test]
    async fn timeout_shutdown_sends_signal_and_waits() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;
        let started = Arc::new(AtomicBool::new(false));
        let started_clone = Arc::clone(&started);

        let entry = ChildEntry::new(
            ChildSpec::new("timeout_child")
                .restart(RestartType::Temporary)
                .shutdown(ShutdownStrategy::Timeout(Duration::from_millis(200))),
            move || {
                let s = Arc::clone(&started_clone);
                async move {
                    s.store(true, AtomicOrdering::SeqCst);
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                }
            },
        );
        let pid = handle.start_child(entry).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(started.load(AtomicOrdering::SeqCst));

        let before = std::time::Instant::now();
        handle.terminate_child(pid).await.unwrap();
        assert!(before.elapsed() < Duration::from_secs(2));

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        handle.shutdown();
    }

    #[tokio::test]
    async fn infinity_shutdown_awaits_child_completion() {
        let rt = make_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;
        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = Arc::clone(&completed);

        let entry = ChildEntry::new(
            ChildSpec::new("infinity_child")
                .restart(RestartType::Temporary)
                .shutdown(ShutdownStrategy::Infinity),
            move || {
                let c = Arc::clone(&completed_clone);
                async move {
                    // Simulate work then exit when shutdown signal received
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    c.store(true, AtomicOrdering::SeqCst);
                    ExitReason::Normal
                }
            },
        );
        let pid = handle.start_child(entry).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.terminate_child(pid).await.unwrap();

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);

        handle.shutdown();
    }
}
