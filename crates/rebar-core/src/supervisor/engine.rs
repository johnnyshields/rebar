use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use crate::process::table::ProcessTable;
use crate::process::{ExitReason, ProcessId};
use crate::runtime::Runtime;
use crate::supervisor::common::{check_restart_limit, shutdown_child_task};
use crate::supervisor::spec::{
    AutoShutdown, ChildSpec, ChildType, RestartStrategy, RestartType, SupervisorError,
    SupervisorSpec,
};

pub type ChildFactory =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ExitReason> + Send>> + Send + Sync>;

pub struct ChildEntry {
    pub spec: ChildSpec,
    pub factory: ChildFactory,
}

impl ChildEntry {
    pub fn new<F, Fut>(spec: ChildSpec, factory: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ExitReason> + Send + 'static,
    {
        Self {
            spec,
            factory: Arc::new(move || Box::pin(factory())),
        }
    }
}

struct ChildState {
    spec: ChildSpec,
    factory: ChildFactory,
    pid: Option<ProcessId>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
    manually_stopped: bool,
}

#[derive(Debug, Clone)]
pub struct ChildCounts {
    pub specs: usize,
    pub active: usize,
    pub supervisors: usize,
    pub workers: usize,
}

#[derive(Debug, Clone)]
pub struct ChildInfo {
    pub id: String,
    pub pid: Option<ProcessId>,
    pub child_type: ChildType,
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
    msg_tx: mpsc::UnboundedSender<SupervisorMsg>,
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

pub async fn start_supervisor(
    runtime: Arc<Runtime>,
    spec: SupervisorSpec,
    children: Vec<ChildEntry>,
) -> SupervisorHandle {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let msg_tx_clone = msg_tx.clone();
    let table = Arc::clone(runtime.table());
    let pid = runtime
        .spawn(move |_ctx| async move {
            supervisor_loop(spec, children, msg_rx, msg_tx_clone, table).await;
        })
        .await;
    SupervisorHandle { pid, msg_tx }
}

async fn supervisor_loop(
    spec: SupervisorSpec,
    children: Vec<ChildEntry>,
    mut msg_rx: mpsc::UnboundedReceiver<SupervisorMsg>,
    msg_tx: mpsc::UnboundedSender<SupervisorMsg>,
    table: Arc<ProcessTable>,
) {
    let mut state = SupervisorState {
        strategy: spec.strategy,
        max_restarts: spec.max_restarts,
        max_seconds: spec.max_seconds,
        auto_shutdown: spec.auto_shutdown,
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
        start_child(&mut state.children[i], &child_id, &msg_tx, &table);
    }

    loop {
        match msg_rx.recv().await {
            Some(SupervisorMsg::ChildExited { child_id, pid, reason }) => {
                let index = match state.children.iter().position(|c| c.spec.id == child_id && c.pid == Some(pid)) {
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
                    if state.children[index].spec.significant {
                        match state.auto_shutdown {
                            AutoShutdown::AnySignificant => {
                                shutdown_all_children(&mut state.children).await;
                                break;
                            }
                            AutoShutdown::AllSignificant => {
                                let all_stopped = state.children.iter()
                                    .filter(|c| c.spec.significant)
                                    .all(|c| c.pid.is_none());
                                if all_stopped {
                                    shutdown_all_children(&mut state.children).await;
                                    break;
                                }
                            }
                            AutoShutdown::Never => {}
                        }
                    }
                    continue;
                }

                if !check_restart_limit(&mut state.restart_times, state.max_restarts, state.max_seconds) {
                    shutdown_all_children(&mut state.children).await;
                    break;
                }

                match state.strategy {
                    RestartStrategy::OneForOne => {
                        let id = state.children[index].spec.id.clone();
                        start_child(&mut state.children[index], &id, &msg_tx, &table);
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
                            start_child(&mut state.children[i], &id, &msg_tx, &table);
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
                            start_child(&mut state.children[i], &id, &msg_tx, &table);
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
                start_child(&mut child_state, &child_id, &msg_tx, &table);
                let pid = child_state.pid.unwrap();
                state.children.push(child_state);
                let _ = reply.send(Ok(pid));
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
                            start_child(&mut state.children[i], &child_id, &msg_tx, &table);
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
                    supervisors: state.children.iter().filter(|c| c.spec.child_type == ChildType::Supervisor).count(),
                    workers: state.children.iter().filter(|c| c.spec.child_type == ChildType::Worker).count(),
                };
                let _ = reply.send(counts);
            }
            Some(SupervisorMsg::WhichChildren { reply }) => {
                let infos = state.children.iter().map(|c| ChildInfo {
                    id: c.spec.id.clone(),
                    pid: c.pid,
                    child_type: c.spec.child_type,
                    restart: c.spec.restart,
                }).collect();
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
    auto_shutdown: AutoShutdown,
    children: Vec<ChildState>,
    restart_times: VecDeque<Instant>,
}

fn start_child(
    child: &mut ChildState,
    child_id: &str,
    msg_tx: &mpsc::UnboundedSender<SupervisorMsg>,
    table: &Arc<ProcessTable>,
) {
    let factory = Arc::clone(&child.factory);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let msg_tx = msg_tx.clone();
    let child_id = child_id.to_string();

    let pid = table.allocate_pid();

    // Register the child in the process table so it is reachable via table.send()
    let (mail_tx, _mail_rx) = crate::process::mailbox::Mailbox::unbounded();
    let proc_handle = crate::process::table::ProcessHandle::new(mail_tx);
    table.insert(pid, proc_handle);

    child.pid = Some(pid);
    child.shutdown_tx = Some(shutdown_tx);

    let table_clone = Arc::clone(table);
    let handle = tokio::spawn(async move {
        let child_future = factory();

        let reason = tokio::select! {
            reason = child_future => reason,
            _ = shutdown_rx => ExitReason::Normal,
        };

        // Remove child from process table on exit
        table_clone.remove(&pid);

        let _ = msg_tx.send(SupervisorMsg::ChildExited { child_id, pid, reason });
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
    use crate::supervisor::spec::{AutoShutdown, ChildType, RestartType, ShutdownStrategy};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    fn test_runtime() -> Arc<Runtime> { Arc::new(Runtime::new(1)) }

    #[tokio::test]
    async fn supervisor_starts_children_in_order() {
        let rt = test_runtime();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut entries = Vec::new();
        for i in 0..3u32 {
            let order = Arc::clone(&order);
            entries.push(ChildEntry::new(ChildSpec::new(format!("child_{}", i)), move || {
                let order = Arc::clone(&order);
                async move { order.lock().await.push(i); tokio::time::sleep(Duration::from_secs(10)).await; ExitReason::Normal }
            }));
        }
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*order.lock().await, vec![0, 1, 2]);
        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn supervisor_stops_children_in_reverse_order() {
        let rt = test_runtime();
        let stop_order = Arc::new(Mutex::new(Vec::<u32>::new()));
        struct DropGuard { id: u32, stop_order: Arc<Mutex<Vec<u32>>> }
        impl Drop for DropGuard {
            fn drop(&mut self) { if let Ok(mut v) = self.stop_order.try_lock() { v.push(self.id); } }
        }
        let mut entries = Vec::new();
        for i in 0..3u32 {
            let so = Arc::clone(&stop_order);
            entries.push(ChildEntry::new(
                ChildSpec::new(format!("child_{}", i)).restart(RestartType::Temporary).shutdown(ShutdownStrategy::Timeout(Duration::from_millis(50))),
                move || { let so = Arc::clone(&so); async move { let _g = DropGuard { id: i, stop_order: so }; tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } },
            ));
        }
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*stop_order.lock().await, vec![2, 1, 0]);
    }

    #[tokio::test]
    async fn supervisor_is_a_process_with_pid() {
        let rt = test_runtime();
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), vec![]).await;
        assert_eq!(handle.pid().node_id(), 1);
        assert!(handle.pid().local_id() > 0);
        handle.shutdown();
    }

    #[tokio::test]
    async fn one_for_one_restarts_only_failed_child() {
        let rt = test_runtime();
        let sc0 = Arc::new(AtomicU32::new(0)); let sc1 = Arc::new(AtomicU32::new(0));
        let sc0c = Arc::clone(&sc0); let sc1c = Arc::clone(&sc1);
        let entries = vec![
            ChildEntry::new(ChildSpec::new("child_0"), move || { let sc = Arc::clone(&sc0c); async move { let c = sc.fetch_add(1, Ordering::SeqCst); if c == 0 { ExitReason::Abnormal("crash".into()) } else { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } } }),
            ChildEntry::new(ChildSpec::new("child_1"), move || { let sc = Arc::clone(&sc1c); async move { sc.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(5).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc0.load(Ordering::SeqCst), 2);
        assert_eq!(sc1.load(Ordering::SeqCst), 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn one_for_one_permanent_child_always_restarts() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("p").restart(RestartType::Permanent), move || { let sc = Arc::clone(&scc); async move { let c = sc.fetch_add(1, Ordering::SeqCst); if c < 2 { ExitReason::Normal } else { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(sc.load(Ordering::SeqCst) >= 3);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn one_for_one_transient_child_normal_exit_no_restart() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("t").restart(RestartType::Transient), move || { let sc = Arc::clone(&scc); async move { sc.fetch_add(1, Ordering::SeqCst); ExitReason::Normal } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn one_for_one_transient_child_abnormal_restarts() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("t").restart(RestartType::Transient), move || { let sc = Arc::clone(&scc); async move { let c = sc.fetch_add(1, Ordering::SeqCst); if c == 0 { ExitReason::Abnormal("crash".into()) } else { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 2);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn one_for_one_temporary_child_never_restarts() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("tmp").restart(RestartType::Temporary), move || { let sc = Arc::clone(&scc); async move { sc.fetch_add(1, Ordering::SeqCst); ExitReason::Abnormal("crash".into()) } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn one_for_all_restarts_all_on_single_failure() {
        let rt = test_runtime();
        let sa = Arc::new(AtomicU32::new(0)); let sb = Arc::new(AtomicU32::new(0)); let sc = Arc::new(AtomicU32::new(0));
        let sac = Arc::clone(&sa); let sbc = Arc::clone(&sb); let scc = Arc::clone(&sc);
        let entries = vec![
            ChildEntry::new(ChildSpec::new("a"), move || { let s = Arc::clone(&sac); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } }),
            ChildEntry::new(ChildSpec::new("b"), move || { let s = Arc::clone(&sbc); async move { let c = s.fetch_add(1, Ordering::SeqCst); if c == 0 { tokio::time::sleep(Duration::from_millis(50)).await; ExitReason::Abnormal("crash".into()) } else { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } } }),
            ChildEntry::new(ChildSpec::new("c"), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForAll).max_restarts(5).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(sa.load(Ordering::SeqCst), 2); assert_eq!(sb.load(Ordering::SeqCst), 2); assert_eq!(sc.load(Ordering::SeqCst), 2);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn rest_for_one_restarts_failed_and_subsequent() {
        let rt = test_runtime();
        let sa = Arc::new(AtomicU32::new(0)); let sb = Arc::new(AtomicU32::new(0)); let sc = Arc::new(AtomicU32::new(0));
        let sac = Arc::clone(&sa); let sbc = Arc::clone(&sb); let scc = Arc::clone(&sc);
        let entries = vec![
            ChildEntry::new(ChildSpec::new("a"), move || { let s = Arc::clone(&sac); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } }),
            ChildEntry::new(ChildSpec::new("b"), move || { let s = Arc::clone(&sbc); async move { let c = s.fetch_add(1, Ordering::SeqCst); if c == 0 { tokio::time::sleep(Duration::from_millis(50)).await; ExitReason::Abnormal("crash".into()) } else { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } } }),
            ChildEntry::new(ChildSpec::new("c"), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::RestForOne).max_restarts(5).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(sa.load(Ordering::SeqCst), 1); assert_eq!(sb.load(Ordering::SeqCst), 2); assert_eq!(sc.load(Ordering::SeqCst), 2);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn max_restarts_within_window_escalates() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("crasher"), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); ExitReason::Abnormal("crash".into()) } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(2).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 3);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn max_restarts_zero_means_never_restart() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("crasher"), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); ExitReason::Abnormal("crash".into()) } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(0).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn shutdown_timeout_respected() {
        let rt = test_runtime();
        let s = Arc::new(AtomicBool::new(false)); let sc = Arc::clone(&s);
        let entries = vec![ChildEntry::new(
            ChildSpec::new("slow").restart(RestartType::Temporary).shutdown(ShutdownStrategy::Timeout(Duration::from_millis(200))),
            move || { let s = Arc::clone(&sc); async move { s.store(true, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } },
        )];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(s.load(Ordering::SeqCst));
        let before = Instant::now();
        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(before.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn brutal_kill_immediate() {
        let rt = test_runtime();
        let s = Arc::new(AtomicBool::new(false)); let sc = Arc::clone(&s);
        let entries = vec![ChildEntry::new(
            ChildSpec::new("k").restart(RestartType::Temporary).shutdown(ShutdownStrategy::BrutalKill),
            move || { let s = Arc::clone(&sc); async move { s.store(true, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } },
        )];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(s.load(Ordering::SeqCst));
        let before = Instant::now();
        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(before.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn add_child_dynamically() {
        let rt = test_runtime();
        let ds = Arc::new(AtomicBool::new(false)); let dsc = Arc::clone(&ds);
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(5).max_seconds(10), vec![]).await;
        let entry = ChildEntry::new(ChildSpec::new("dyn"), move || { let d = Arc::clone(&dsc); async move { d.store(true, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } });
        assert!(handle.add_child(entry).await.is_ok());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(ds.load(Ordering::SeqCst));
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn terminate_child_stops_and_prevents_restart() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("w").restart(RestartType::Permanent), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 1);
        assert!(handle.terminate_child("w").await.is_ok());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn restart_child_restarts_stopped_child() {
        let rt = test_runtime();
        let sc = Arc::new(AtomicU32::new(0)); let scc = Arc::clone(&sc);
        let entries = vec![ChildEntry::new(ChildSpec::new("w").restart(RestartType::Permanent), move || { let s = Arc::clone(&scc); async move { s.fetch_add(1, Ordering::SeqCst); tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal } })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.terminate_child("w").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.restart_child("w").await.is_ok());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 2);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn restart_child_errors_if_running() {
        let rt = test_runtime();
        let entries = vec![ChildEntry::new(ChildSpec::new("w"), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.restart_child("w").await.is_err());
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn delete_child_removes_stopped_child() {
        let rt = test_runtime();
        let entries = vec![ChildEntry::new(ChildSpec::new("w"), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.terminate_child("w").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.delete_child("w").await.is_ok());
        assert_eq!(handle.count_children().await.unwrap().specs, 0);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn count_children_returns_correct_counts() {
        let rt = test_runtime();
        let entries = vec![
            ChildEntry::new(ChildSpec::new("w1").child_type(ChildType::Worker), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
            ChildEntry::new(ChildSpec::new("w2").child_type(ChildType::Worker), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
            ChildEntry::new(ChildSpec::new("s1").child_type(ChildType::Supervisor), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let c = handle.count_children().await.unwrap();
        assert_eq!(c.specs, 3); assert_eq!(c.active, 3); assert_eq!(c.workers, 2); assert_eq!(c.supervisors, 1);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn which_children_returns_correct_info() {
        let rt = test_runtime();
        let entries = vec![
            ChildEntry::new(ChildSpec::new("w1").child_type(ChildType::Worker).restart(RestartType::Permanent), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
            ChildEntry::new(ChildSpec::new("s1").child_type(ChildType::Supervisor).restart(RestartType::Transient), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ch = handle.which_children().await.unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].id, "w1"); assert_eq!(ch[0].child_type, ChildType::Worker);
        assert_eq!(ch[1].id, "s1"); assert_eq!(ch[1].child_type, ChildType::Supervisor);
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn auto_shutdown_any_significant() {
        let rt = test_runtime();
        let entries = vec![
            ChildEntry::new(ChildSpec::new("sig").restart(RestartType::Temporary).significant(true), move || async move { ExitReason::Normal }),
            ChildEntry::new(ChildSpec::new("other"), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).auto_shutdown(AutoShutdown::AnySignificant).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        match handle.count_children().await { Err(_) => {} Ok(c) => assert_eq!(c.active, 0) }
    }

    #[tokio::test]
    async fn auto_shutdown_all_significant() {
        let rt = test_runtime();
        let s2e = Arc::new(AtomicBool::new(false)); let s2ec = Arc::clone(&s2e);
        let entries = vec![
            ChildEntry::new(ChildSpec::new("sig1").restart(RestartType::Temporary).significant(true), move || async move { ExitReason::Normal }),
            ChildEntry::new(ChildSpec::new("sig2").restart(RestartType::Temporary).significant(true), move || { let s = Arc::clone(&s2ec); async move { tokio::time::sleep(Duration::from_millis(200)).await; s.store(true, Ordering::SeqCst); ExitReason::Normal } }),
            ChildEntry::new(ChildSpec::new("nonsig").restart(RestartType::Temporary), move || async move { tokio::time::sleep(Duration::from_secs(60)).await; ExitReason::Normal }),
        ];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).auto_shutdown(AutoShutdown::AllSignificant).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.count_children().await.is_ok());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(s2e.load(Ordering::SeqCst));
        tokio::time::sleep(Duration::from_millis(200)).await;
        match handle.count_children().await { Err(_) => {} Ok(c) => assert_eq!(c.active, 0) }
    }

    #[tokio::test]
    async fn auto_shutdown_never_is_default() {
        let rt = test_runtime();
        let entries = vec![ChildEntry::new(ChildSpec::new("sig").restart(RestartType::Temporary).significant(true), move || async move { ExitReason::Normal })];
        let handle = start_supervisor(rt, SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(10).max_seconds(10), entries).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(handle.count_children().await.is_ok());
        handle.shutdown(); tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
