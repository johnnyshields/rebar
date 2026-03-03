use std::net::SocketAddr;
use std::time::Duration;

use crate::registry::Registry;
use crate::swim::gossip::{GossipQueue, GossipUpdate};

/// Configuration for the three-phase drain protocol.
#[derive(Debug, Clone)]
pub struct DrainConfig {
    /// Time to propagate Leave gossip (phase 1).
    pub announce_timeout: Duration,
    /// Time to wait for in-flight messages (phase 2).
    pub drain_timeout: Duration,
    /// Time for supervisor shutdown (phase 3).
    pub shutdown_timeout: Duration,
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self {
            announce_timeout: Duration::from_secs(5),
            drain_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(10),
        }
    }
}

/// Result of a completed drain operation.
#[derive(Debug)]
pub struct DrainResult {
    /// Number of processes stopped during shutdown.
    pub processes_stopped: usize,
    /// Number of outbound messages drained.
    pub messages_drained: usize,
    /// Duration of each phase: [announce, drain, shutdown].
    pub phase_durations: [Duration; 3],
    /// Whether any phase hit its timeout.
    pub timed_out: bool,
}

/// Orchestrates the three-phase drain protocol.
pub struct NodeDrain {
    config: DrainConfig,
}

impl NodeDrain {
    pub fn new(config: DrainConfig) -> Self {
        Self { config }
    }

    /// Phase 1: Announce departure to the cluster.
    /// - Broadcasts Leave via SWIM gossip
    /// - Unregisters all names from the registry
    /// Returns the number of names unregistered.
    pub fn announce(
        &self,
        node_id: u64,
        addr: SocketAddr,
        gossip: &mut GossipQueue,
        registry: &mut Registry,
    ) -> usize {
        gossip.add(GossipUpdate::Leave { node_id, addr });

        let names_before = registry.registered().len();
        registry.remove_by_node(node_id);
        let names_after = registry.registered().len();

        names_before - names_after
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use crate::swim::gossip::{GossipQueue, GossipUpdate};
    use rebar_core::process::ProcessId;
    use std::net::SocketAddr;

    fn test_addr() -> SocketAddr {
        "127.0.0.1:4000".parse().unwrap()
    }

    #[test]
    fn drain_config_defaults() {
        let config = DrainConfig::default();
        assert_eq!(config.announce_timeout, Duration::from_secs(5));
        assert_eq!(config.drain_timeout, Duration::from_secs(30));
        assert_eq!(config.shutdown_timeout, Duration::from_secs(10));
    }

    #[test]
    fn drain_config_custom() {
        let config = DrainConfig {
            announce_timeout: Duration::from_secs(1),
            drain_timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(5),
        };
        assert_eq!(config.announce_timeout, Duration::from_secs(1));
    }

    #[test]
    fn drain_result_fields() {
        let result = DrainResult {
            processes_stopped: 10,
            messages_drained: 50,
            phase_durations: [
                Duration::from_millis(100),
                Duration::from_millis(500),
                Duration::from_millis(200),
            ],
            timed_out: false,
        };
        assert_eq!(result.processes_stopped, 10);
        assert_eq!(result.messages_drained, 50);
        assert!(!result.timed_out);
    }

    #[test]
    fn drain_broadcasts_leave() {
        let drain = NodeDrain::new(DrainConfig::default());
        let mut gossip = GossipQueue::new();
        let mut registry = Registry::default();

        drain.announce(1, test_addr(), &mut gossip, &mut registry);

        let updates = gossip.drain(10);
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], GossipUpdate::Leave { node_id: 1, .. }));
    }

    #[test]
    fn drain_unregisters_names() {
        let drain = NodeDrain::new(DrainConfig::default());
        let mut gossip = GossipQueue::new();
        let mut registry = Registry::default();

        registry.register("service_a", ProcessId::new(1, 1), 1, 100);
        registry.register("service_b", ProcessId::new(1, 2), 1, 101);
        registry.register("service_c", ProcessId::new(2, 1), 2, 102);

        assert_eq!(registry.registered().len(), 3);

        let removed = drain.announce(1, test_addr(), &mut gossip, &mut registry);

        assert_eq!(removed, 2);
        assert_eq!(registry.registered().len(), 1);
        assert!(registry.lookup("service_c").is_some());
        assert!(registry.lookup("service_a").is_none());
        assert!(registry.lookup("service_b").is_none());
    }
}
