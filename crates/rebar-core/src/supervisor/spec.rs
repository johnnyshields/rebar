use crate::process::ExitReason;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub enum RestartStrategy {
    OneForOne,
    OneForAll,
    RestForOne,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildType {
    Worker,
    Supervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoShutdown {
    Never,
    AnySignificant,
    AllSignificant,
}

#[derive(Debug, Clone, Copy)]
pub enum RestartType {
    Permanent,
    Transient,
    Temporary,
}

impl RestartType {
    pub fn should_restart(&self, reason: &ExitReason) -> bool {
        match self {
            RestartType::Permanent => true,
            RestartType::Transient => !reason.is_normal(),
            RestartType::Temporary => false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ShutdownStrategy {
    Timeout(Duration),
    BrutalKill,
    Infinity,
}

pub struct SupervisorSpec {
    pub strategy: RestartStrategy,
    pub max_restarts: u32,
    pub max_seconds: u32,
    pub children: Vec<ChildSpec>,
    pub auto_shutdown: AutoShutdown,
}

impl SupervisorSpec {
    pub fn new(strategy: RestartStrategy) -> Self {
        Self {
            strategy,
            max_restarts: 3,
            max_seconds: 5,
            children: Vec::new(),
            auto_shutdown: AutoShutdown::Never,
        }
    }

    pub fn max_restarts(mut self, n: u32) -> Self {
        self.max_restarts = n;
        self
    }

    pub fn max_seconds(mut self, n: u32) -> Self {
        self.max_seconds = n;
        self
    }

    pub fn child(mut self, spec: ChildSpec) -> Self {
        self.children.push(spec);
        self
    }

    pub fn auto_shutdown(mut self, mode: AutoShutdown) -> Self {
        self.auto_shutdown = mode;
        self
    }
}

pub struct ChildSpec {
    pub id: String,
    pub restart: RestartType,
    pub shutdown: ShutdownStrategy,
    pub child_type: ChildType,
    pub significant: bool,
}

impl ChildSpec {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            restart: RestartType::Permanent,
            shutdown: ShutdownStrategy::Timeout(Duration::from_secs(5)),
            child_type: ChildType::Worker,
            significant: false,
        }
    }

    pub fn restart(mut self, restart: RestartType) -> Self {
        self.restart = restart;
        self
    }

    pub fn shutdown(mut self, strategy: ShutdownStrategy) -> Self {
        self.shutdown = strategy;
        self
    }

    pub fn child_type(mut self, ct: ChildType) -> Self {
        self.child_type = ct;
        if ct == ChildType::Supervisor {
            self.shutdown = ShutdownStrategy::Infinity;
        }
        self
    }

    pub fn significant(mut self, val: bool) -> Self {
        self.significant = val;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ExitReason;

    #[test]
    fn permanent_restarts_on_normal() {
        assert!(RestartType::Permanent.should_restart(&ExitReason::Normal));
    }

    #[test]
    fn permanent_restarts_on_abnormal() {
        assert!(RestartType::Permanent.should_restart(&ExitReason::Abnormal("err".into())));
    }

    #[test]
    fn permanent_restarts_on_kill() {
        assert!(RestartType::Permanent.should_restart(&ExitReason::Kill));
    }

    #[test]
    fn transient_does_not_restart_on_normal() {
        assert!(!RestartType::Transient.should_restart(&ExitReason::Normal));
    }

    #[test]
    fn transient_restarts_on_abnormal() {
        assert!(RestartType::Transient.should_restart(&ExitReason::Abnormal("err".into())));
    }

    #[test]
    fn temporary_never_restarts() {
        assert!(!RestartType::Temporary.should_restart(&ExitReason::Normal));
        assert!(!RestartType::Temporary.should_restart(&ExitReason::Abnormal("err".into())));
        assert!(!RestartType::Temporary.should_restart(&ExitReason::Kill));
    }

    #[test]
    fn supervisor_spec_builder() {
        let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
            .max_restarts(3)
            .max_seconds(5);
        assert_eq!(spec.max_restarts, 3);
        assert_eq!(spec.max_seconds, 5);
    }

    #[test]
    fn supervisor_spec_with_children() {
        let spec = SupervisorSpec::new(RestartStrategy::OneForAll)
            .child(ChildSpec::new("worker_1"))
            .child(ChildSpec::new("worker_2"));
        assert_eq!(spec.children.len(), 2);
    }

    #[test]
    fn child_spec_defaults() {
        let child = ChildSpec::new("test");
        assert_eq!(child.id, "test");
        assert!(matches!(child.restart, RestartType::Permanent));
        assert!(matches!(child.shutdown, ShutdownStrategy::Timeout(_)));
        assert_eq!(child.child_type, ChildType::Worker);
        assert!(!child.significant);
    }

    #[test]
    fn child_spec_builder() {
        let child = ChildSpec::new("worker")
            .restart(RestartType::Transient)
            .shutdown(ShutdownStrategy::BrutalKill);
        assert!(matches!(child.restart, RestartType::Transient));
        assert!(matches!(child.shutdown, ShutdownStrategy::BrutalKill));
    }

    #[test]
    fn child_type_supervisor_sets_infinity_shutdown() {
        let child = ChildSpec::new("sup").child_type(ChildType::Supervisor);
        assert_eq!(child.child_type, ChildType::Supervisor);
        assert!(matches!(child.shutdown, ShutdownStrategy::Infinity));
    }

    #[test]
    fn significant_can_be_set() {
        let child = ChildSpec::new("test").significant(true);
        assert!(child.significant);
    }

    #[test]
    fn auto_shutdown_defaults_to_never() {
        let spec = SupervisorSpec::new(RestartStrategy::OneForOne);
        assert_eq!(spec.auto_shutdown, AutoShutdown::Never);
    }

    #[test]
    fn auto_shutdown_builder() {
        let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
            .auto_shutdown(AutoShutdown::AnySignificant);
        assert_eq!(spec.auto_shutdown, AutoShutdown::AnySignificant);
    }
}
