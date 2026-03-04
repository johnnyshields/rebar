use serde::Serialize;
use tokio::sync::broadcast;

use crate::process::{ExitReason, ProcessId};
use crate::supervisor::spec::RestartStrategy;

/// Lifecycle events emitted by the runtime and supervisor.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum LifecycleEvent {
    ProcessSpawned {
        pid: ProcessId,
        parent: Option<ProcessId>,
        name: Option<String>,
        timestamp: u64,
    },
    ProcessExited {
        pid: ProcessId,
        reason: ExitReason,
        timestamp: u64,
    },
    SupervisorStarted {
        pid: ProcessId,
        strategy: RestartStrategy,
        child_ids: Vec<String>,
        timestamp: u64,
    },
    ChildStarted {
        supervisor_pid: ProcessId,
        child_pid: ProcessId,
        child_id: String,
        timestamp: u64,
    },
    ChildExited {
        supervisor_pid: ProcessId,
        child_pid: ProcessId,
        child_id: String,
        reason: ExitReason,
        timestamp: u64,
    },
    ChildRestarted {
        supervisor_pid: ProcessId,
        old_pid: ProcessId,
        new_pid: ProcessId,
        child_id: String,
        restart_count: u32,
        timestamp: u64,
    },
    SupervisorMaxRestartsExceeded {
        pid: ProcessId,
        timestamp: u64,
    },
}

impl LifecycleEvent {
    /// Current timestamp in milliseconds since UNIX epoch.
    pub fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}

/// Broadcast-based event bus for lifecycle events.
///
/// Uses `tokio::sync::broadcast` so multiple subscribers can listen.
/// Lagging subscribers lose old events rather than blocking the runtime.
/// Zero cost when no subscribers are attached.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<LifecycleEvent>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Emit an event. Does nothing if there are no subscribers.
    pub fn emit(&self, event: LifecycleEvent) {
        // Ignore send errors (no active receivers).
        let _ = self.tx.send(event);
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<LifecycleEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_and_subscribe() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        let pid = ProcessId::new(1, 1);
        bus.emit(LifecycleEvent::ProcessSpawned {
            pid,
            parent: None,
            name: Some("test".into()),
            timestamp: LifecycleEvent::now(),
        });

        let event = rx.recv().await.unwrap();
        match event {
            LifecycleEvent::ProcessSpawned { pid: p, name, .. } => {
                assert_eq!(p, pid);
                assert_eq!(name.as_deref(), Some("test"));
            }
            _ => panic!("unexpected event"),
        }
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.emit(LifecycleEvent::ProcessExited {
            pid: ProcessId::new(1, 1),
            reason: ExitReason::Normal,
            timestamp: LifecycleEvent::now(),
        });
    }

    #[tokio::test]
    async fn runtime_emits_spawn_and_exit_events() {
        use crate::runtime::Runtime;

        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();

        let rt = Runtime::new(1).with_event_bus(bus);
        let pid = rt
            .spawn(|_ctx| async {
                // exit immediately
            })
            .await;

        // Should receive ProcessSpawned
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            LifecycleEvent::ProcessSpawned { pid: p, .. } => assert_eq!(p, pid),
            other => panic!("expected ProcessSpawned, got {:?}", other),
        }

        // Should receive ProcessExited
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            LifecycleEvent::ProcessExited {
                pid: p,
                reason: ExitReason::Normal,
                ..
            } => assert_eq!(p, pid),
            other => panic!("expected ProcessExited(Normal), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.emit(LifecycleEvent::ProcessSpawned {
            pid: ProcessId::new(1, 1),
            parent: None,
            name: None,
            timestamp: LifecycleEvent::now(),
        });

        assert!(rx1.recv().await.is_ok());
        assert!(rx2.recv().await.is_ok());
    }
}
