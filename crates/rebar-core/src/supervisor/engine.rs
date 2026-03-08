use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Instant;

use crate::channel::mpsc::unbounded as local_mpsc;
use crate::channel::oneshot;

use crate::process::{ExitReason, ProcessId};
use crate::runtime::Runtime;
use crate::supervisor::common::{check_restart_limit, shutdown_child_task};
use crate::supervisor::spec::{
    ChildSpec, RestartStrategy, RestartType, SupervisorError, SupervisorSpec,
};

pub type ChildFactory =
    Rc<dyn Fn() -> Pin<Box<dyn Future<Output = ExitReason>>>>;

pub struct ChildEntry {
    pub spec: ChildSpec,
    pub factory: ChildFactory,
}

impl ChildEntry {
    pub fn new<F, Fut>(spec: ChildSpec, factory: F) -> Self
    where
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = ExitReason> + 'static,
    {
        Self {
            spec,
            factory: Rc::new(move || Box::pin(factory())),
        }
    }
}

struct ChildState {
    spec: ChildSpec,
    factory: ChildFactory,
    pid: Option<ProcessId>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<crate::task::JoinHandle<()>>,
    manually_stopped: bool,
}

#[derive(Debug, Clone)]
pub struct ChildCounts {
    pub specs: usize,
    pub active: usize,
}

#[derive(Debug, Clone)]
pub struct ChildInfo {
    pub id: String,
    pub pid: Option<ProcessId>,
    pub restart: RestartType,
}

enum SupervisorMsg {
    ChildExited {
        child_id: String,
        pid: ProcessId,
        reason: ExitReason,
    },
    AddChild {
        entry: ChildEntry,
        reply: oneshot::Sender<Result<ProcessId, SupervisorError>>,
    },
    AddChildren {
        entries: Vec<ChildEntry>,
        reply: oneshot::Sender<Vec<Result<ProcessId, String>>>,
    },
    Shutdown,
    TerminateChild {
        id: String,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    RestartChild {
        id: String,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    DeleteChild {
        id: String,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    CountChildren {
        reply: oneshot::Sender<ChildCounts>,
    },
    WhichChildren {
        reply: oneshot::Sender<Vec<ChildInfo>>,
    },
}

#[derive(Clone)]
pub struct SupervisorHandle {
    pid: ProcessId,
    msg_tx: local_mpsc::Tx<SupervisorMsg>,
}

impl SupervisorHandle {
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    pub async fn add_child(&self, entry: ChildEntry) -> Result<ProcessId, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::AddChild {
                entry,
                reply: reply_tx,
            })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    pub async fn add_children(
        &self,
        entries: Vec<ChildEntry>,
    ) -> Vec<Result<ProcessId, String>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .msg_tx
            .send(SupervisorMsg::AddChildren {
                entries,
                reply: reply_tx,
            })
            .is_err()
        {
            return vec![Err("supervisor gone".to_string())];
        }
        reply_rx
            .await
            .unwrap_or_else(|_| vec![Err("supervisor gone".to_string())])
    }

    pub fn shutdown(&self) {
        let _ = self.msg_tx.send(SupervisorMsg::Shutdown);
    }

    pub async fn terminate_child(&self, id: &str) -> Result<(), SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::TerminateChild {
                id: id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    pub async fn restart_child(&self, id: &str) -> Result<(), SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::RestartChild {
                id: id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    pub async fn delete_child(&self, id: &str) -> Result<(), SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::DeleteChild {
                id: id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)?
    }

    pub async fn count_children(&self) -> Result<ChildCounts, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::CountChildren { reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)
    }

    pub async fn which_children(&self) -> Result<Vec<ChildInfo>, SupervisorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(SupervisorMsg::WhichChildren { reply: reply_tx })
            .map_err(|_| SupervisorError::Gone)?;
        reply_rx.await.map_err(|_| SupervisorError::Gone)
    }
}

pub fn start_supervisor(
    runtime: &Runtime,
    spec: SupervisorSpec,
    children: Vec<ChildEntry>,
) -> SupervisorHandle {
    let (msg_tx, msg_rx) = local_mpsc::channel();
    let msg_tx_clone = msg_tx.clone();
    let pid = runtime.spawn(move |_ctx| async move {
        supervisor_loop(spec, children, msg_rx, msg_tx_clone).await;
    });
    SupervisorHandle { pid, msg_tx }
}

async fn supervisor_loop(
    spec: SupervisorSpec,
    children: Vec<ChildEntry>,
    mut msg_rx: local_mpsc::Rx<SupervisorMsg>,
    msg_tx: local_mpsc::Tx<SupervisorMsg>,
) {
    let mut state = SupervisorState {
        strategy: spec.strategy,
        max_restarts: spec.max_restarts,
        max_seconds: spec.max_seconds,
        children: Vec::new(),
        restart_times: VecDeque::new(),
    };

    for entry in children {
        state.children.push(ChildState {
            spec: entry.spec,
            factory: entry.factory,
            pid: None,
            shutdown_tx: None,
            join_handle: None,
            manually_stopped: false,
        });
    }

    for i in 0..state.children.len() {
        let child_id = state.children[i].spec.id.clone();
        start_child(&mut state.children[i], &child_id, &msg_tx);
    }

    loop {
        match msg_rx.recv().await {
            Some(SupervisorMsg::ChildExited {
                child_id,
                pid,
                reason,
            }) => {
                let index = match state
                    .children
                    .iter()
                    .position(|c| c.spec.id == child_id && c.pid == Some(pid))
                {
                    Some(i) => i,
                    None => continue,
                };
                state.children[index].pid = None;
                state.children[index].shutdown_tx = None;
                state.children[index].join_handle = None;

                if state.children[index].manually_stopped {
                    continue;
                }

                let should_restart = state.children[index].spec.restart.should_restart(&reason);

                if !should_restart {
                    continue;
                }

                if !check_restart_limit(
                    &mut state.restart_times,
                    state.max_restarts,
                    state.max_seconds,
                ) {
                    shutdown_all_children(&mut state.children).await;
                    break;
                }

                match state.strategy {
                    RestartStrategy::OneForOne => {
                        let id = state.children[index].spec.id.clone();
                        start_child(&mut state.children[index], &id, &msg_tx);
                    }
                    RestartStrategy::OneForAll => {
                        let len = state.children.len();
                        for i in (0..len).rev() {
                            if i != index && state.children[i].pid.is_some() {
                                stop_child(&mut state.children[i]).await;
                            }
                        }
                        for i in 0..len {
                            let id = state.children[i].spec.id.clone();
                            start_child(&mut state.children[i], &id, &msg_tx);
                        }
                    }
                    RestartStrategy::RestForOne => {
                        let len = state.children.len();
                        for i in (index + 1..len).rev() {
                            if state.children[i].pid.is_some() {
                                stop_child(&mut state.children[i]).await;
                            }
                        }
                        for i in index..len {
                            let id = state.children[i].spec.id.clone();
                            start_child(&mut state.children[i], &id, &msg_tx);
                        }
                    }
                }
            }
            Some(SupervisorMsg::AddChild { entry, reply }) => {
                let child_id = entry.spec.id.clone();
                let mut child_state = ChildState {
                    spec: entry.spec,
                    factory: entry.factory,
                    pid: None,
                    shutdown_tx: None,
                    join_handle: None,
                    manually_stopped: false,
                };
                start_child(&mut child_state, &child_id, &msg_tx);
                let pid = child_state.pid.unwrap();
                state.children.push(child_state);
                let _ = reply.send(Ok(pid));
            }
            Some(SupervisorMsg::AddChildren { entries, reply }) => {
                let mut results = Vec::with_capacity(entries.len());
                for entry in entries {
                    let child_id = entry.spec.id.clone();
                    let mut child_state = ChildState {
                        spec: entry.spec,
                        factory: entry.factory,
                        pid: None,
                        shutdown_tx: None,
                        join_handle: None,
                        manually_stopped: false,
                    };
                    start_child(&mut child_state, &child_id, &msg_tx);
                    let pid = child_state.pid.unwrap();
                    state.children.push(child_state);
                    results.push(Ok(pid));
                }
                let _ = reply.send(results);
            }
            Some(SupervisorMsg::TerminateChild { id, reply }) => {
                match state.children.iter().position(|c| c.spec.id == id) {
                    Some(i) => {
                        if state.children[i].pid.is_some() {
                            stop_child(&mut state.children[i]).await;
                        }
                        state.children[i].manually_stopped = true;
                        let _ = reply.send(Ok(()));
                    }
                    None => {
                        let _ = reply.send(Err(SupervisorError::NotFound(id)));
                    }
                }
            }
            Some(SupervisorMsg::RestartChild { id, reply }) => {
                match state.children.iter().position(|c| c.spec.id == id) {
                    Some(i) => {
                        if state.children[i].pid.is_some() {
                            let _ = reply.send(Err(SupervisorError::StillRunning(id)));
                        } else {
                            let child_id = state.children[i].spec.id.clone();
                            start_child(&mut state.children[i], &child_id, &msg_tx);
                            state.children[i].manually_stopped = false;
                            let _ = reply.send(Ok(()));
                        }
                    }
                    None => {
                        let _ = reply.send(Err(SupervisorError::NotFound(id)));
                    }
                }
            }
            Some(SupervisorMsg::DeleteChild { id, reply }) => {
                match state.children.iter().position(|c| c.spec.id == id) {
                    Some(i) => {
                        if state.children[i].pid.is_some() {
                            let _ = reply.send(Err(SupervisorError::StillRunning(id)));
                        } else {
                            state.children.remove(i);
                            let _ = reply.send(Ok(()));
                        }
                    }
                    None => {
                        let _ = reply.send(Err(SupervisorError::NotFound(id)));
                    }
                }
            }
            Some(SupervisorMsg::CountChildren { reply }) => {
                let counts = ChildCounts {
                    specs: state.children.len(),
                    active: state.children.iter().filter(|c| c.pid.is_some()).count(),
                };
                let _ = reply.send(counts);
            }
            Some(SupervisorMsg::WhichChildren { reply }) => {
                let infos = state
                    .children
                    .iter()
                    .map(|c| ChildInfo {
                        id: c.spec.id.clone(),
                        pid: c.pid,
                        restart: c.spec.restart,
                    })
                    .collect();
                let _ = reply.send(infos);
            }
            Some(SupervisorMsg::Shutdown) => {
                shutdown_all_children(&mut state.children).await;
                break;
            }
            None => {
                shutdown_all_children(&mut state.children).await;
                break;
            }
        }
    }
}

struct SupervisorState {
    strategy: RestartStrategy,
    max_restarts: u32,
    max_seconds: u32,
    children: Vec<ChildState>,
    restart_times: VecDeque<Instant>,
}

/// Guard that sends a `ChildExited` message on drop if not disarmed.
/// Fires when `CatchUnwind` drops the inner future on panic — immediately,
/// before `JoinHandle` resolves.
struct PanicGuard {
    msg_tx: local_mpsc::Tx<SupervisorMsg>,
    child_id: String,
    pid: ProcessId,
    armed: bool,
}

impl PanicGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PanicGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.msg_tx.send(SupervisorMsg::ChildExited {
                child_id: self.child_id.clone(),
                pid: self.pid,
                reason: ExitReason::Abnormal("panicked".into()),
            });
        }
    }
}

fn start_child(
    child: &mut ChildState,
    child_id: &str,
    msg_tx: &local_mpsc::Tx<SupervisorMsg>,
) {
    let factory = Rc::clone(&child.factory);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let msg_tx = msg_tx.clone();
    let child_id = child_id.to_string();

    // Use a simple incrementing counter for PIDs
    static PID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000_000);
    let local_id = PID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = ProcessId::new(0, 0, local_id);

    child.pid = Some(pid);
    child.shutdown_tx = Some(shutdown_tx);

    let handle = crate::executor::spawn(async move {
        let mut guard = PanicGuard {
            msg_tx: msg_tx.clone(),
            child_id: child_id.clone(),
            pid,
            armed: true,
        };

        let child_future = factory();

        use futures::future::{select, Either};
        let reason = match select(std::pin::pin!(shutdown_rx), std::pin::pin!(child_future)).await {
            Either::Left((_shutdown, _child)) => ExitReason::Normal,
            Either::Right((exit, _shutdown)) => exit,
        };

        // Normal exit — disarm guard, send reason ourselves
        guard.disarm();
        let _ = msg_tx.send(SupervisorMsg::ChildExited {
            child_id,
            pid,
            reason,
        });
    });
    child.join_handle = Some(handle);
}

async fn stop_child(child: &mut ChildState) {
    let shutdown_tx = child.shutdown_tx.take();
    let join_handle = child.join_handle.take();
    shutdown_child_task(&child.spec.shutdown, shutdown_tx, join_handle).await;
    child.pid = None;
}

async fn shutdown_all_children(children: &mut [ChildState]) {
    for i in (0..children.len()).rev() {
        if children[i].pid.is_some() {
            stop_child(&mut children[i]).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorConfig, RebarExecutor};
    use crate::supervisor::spec::RestartType;
    use crate::time::sleep;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    #[test]
    fn supervisor_starts_children_in_order() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let order = Rc::new(RefCell::new(Vec::new()));
            let mut entries = Vec::new();
            for i in 0..3u32 {
                let order = Rc::clone(&order);
                entries.push(ChildEntry::new(
                    ChildSpec::new(format!("child_{}", i)),
                    move || {
                        let order = Rc::clone(&order);
                        async move {
                            order.borrow_mut().push(i);
                            sleep(Duration::from_secs(10)).await;
                            ExitReason::Normal
                        }
                    },
                ));
            }
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne),
                entries,
            );
            sleep(Duration::from_millis(50)).await;
            assert_eq!(*order.borrow(), vec![0, 1, 2]);
            handle.shutdown();
            sleep(Duration::from_millis(50)).await;
        });
    }

    #[test]
    fn supervisor_is_a_process_with_pid() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne),
                vec![],
            );
            assert_eq!(handle.pid().node_id(), 1);
            assert!(handle.pid().local_id() > 0);
            handle.shutdown();
        });
    }

    #[test]
    fn one_for_one_restarts_only_failed_child() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let sc0 = Arc::new(AtomicU32::new(0));
            let sc1 = Arc::new(AtomicU32::new(0));
            let sc0c = Arc::clone(&sc0);
            let sc1c = Arc::clone(&sc1);
            let entries = vec![
                ChildEntry::new(ChildSpec::new("child_0"), move || {
                    let sc = Arc::clone(&sc0c);
                    async move {
                        let c = sc.fetch_add(1, Ordering::SeqCst);
                        if c == 0 {
                            ExitReason::Abnormal("crash".into())
                        } else {
                            sleep(Duration::from_secs(60)).await;
                            ExitReason::Normal
                        }
                    }
                }),
                ChildEntry::new(ChildSpec::new("child_1"), move || {
                    let sc = Arc::clone(&sc1c);
                    async move {
                        sc.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_secs(60)).await;
                        ExitReason::Normal
                    }
                }),
            ];
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne)
                    .max_restarts(5)
                    .max_seconds(10),
                entries,
            );
            sleep(Duration::from_millis(500)).await;
            assert_eq!(sc0.load(Ordering::SeqCst), 2);
            assert_eq!(sc1.load(Ordering::SeqCst), 1);
            handle.shutdown();
            sleep(Duration::from_millis(100)).await;
        });
    }

    #[test]
    fn max_restarts_within_window_escalates() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let sc = Arc::new(AtomicU32::new(0));
            let scc = Arc::clone(&sc);
            let entries = vec![ChildEntry::new(ChildSpec::new("crasher"), move || {
                let s = Arc::clone(&scc);
                async move {
                    s.fetch_add(1, Ordering::SeqCst);
                    ExitReason::Abnormal("crash".into())
                }
            })];
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne)
                    .max_restarts(2)
                    .max_seconds(10),
                entries,
            );
            sleep(Duration::from_millis(500)).await;
            assert_eq!(sc.load(Ordering::SeqCst), 3);
            handle.shutdown();
            sleep(Duration::from_millis(100)).await;
        });
    }

    #[test]
    fn one_for_one_temporary_child_never_restarts() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let sc = Arc::new(AtomicU32::new(0));
            let scc = Arc::clone(&sc);
            let entries = vec![ChildEntry::new(
                ChildSpec::new("tmp").restart(RestartType::Temporary),
                move || {
                    let sc = Arc::clone(&scc);
                    async move {
                        sc.fetch_add(1, Ordering::SeqCst);
                        ExitReason::Abnormal("crash".into())
                    }
                },
            )];
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne)
                    .max_restarts(10)
                    .max_seconds(10),
                entries,
            );
            sleep(Duration::from_millis(300)).await;
            assert_eq!(sc.load(Ordering::SeqCst), 1);
            handle.shutdown();
            sleep(Duration::from_millis(100)).await;
        });
    }

    #[test]
    fn supervisor_restarts_panicked_child() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let sc = Arc::new(AtomicU32::new(0));
            let scc = Arc::clone(&sc);
            let entries = vec![ChildEntry::new(ChildSpec::new("panicker"), move || {
                let s = Arc::clone(&scc);
                async move {
                    let count = s.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        panic!("first run panics");
                    }
                    // Second run succeeds and stays alive
                    sleep(Duration::from_secs(60)).await;
                    ExitReason::Normal
                }
            })];
            let handle = start_supervisor(
                &rt,
                SupervisorSpec::new(RestartStrategy::OneForOne)
                    .max_restarts(5)
                    .max_seconds(10),
                entries,
            );
            sleep(Duration::from_millis(500)).await;
            // Should have run twice: first panicked, then restarted
            assert_eq!(sc.load(Ordering::SeqCst), 2);
            handle.shutdown();
            sleep(Duration::from_millis(100)).await;
        });
    }

    #[test]
    fn add_children_batch() {
        let ex = test_executor();
        ex.block_on(async {
            let rt = Runtime::new(1);
            let started_count = Arc::new(AtomicU32::new(0));

            let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
                .max_restarts(5)
                .max_seconds(10);
            let handle = start_supervisor(&rt, spec, vec![]);

            let entries: Vec<ChildEntry> = (0..5)
                .map(|i| {
                    let sc = Arc::clone(&started_count);
                    ChildEntry::new(ChildSpec::new(format!("batch-{}", i)), move || {
                        let sc = Arc::clone(&sc);
                        async move {
                            sc.fetch_add(1, Ordering::SeqCst);
                            sleep(Duration::from_secs(60)).await;
                            ExitReason::Normal
                        }
                    })
                })
                .collect();

            let results = handle.add_children(entries).await;
            assert_eq!(results.len(), 5);
            for result in &results {
                assert!(result.is_ok());
            }

            sleep(Duration::from_millis(100)).await;
            assert_eq!(started_count.load(Ordering::SeqCst), 5);

            handle.shutdown();
            sleep(Duration::from_millis(50)).await;
        });
    }
}
