use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
