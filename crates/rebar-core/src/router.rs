use std::rc::Rc;

use crate::process::table::ProcessTable;
use crate::process::{Message, ProcessId, SendError};

/// Trait for routing messages between processes.
/// Implementations decide whether to deliver locally or over the network.
pub trait MessageRouter {
    fn route(
        &self,
        from: ProcessId,
        to: ProcessId,
        payload: rmpv::Value,
    ) -> Result<(), SendError>;
}

/// Default router that delivers messages to the local ProcessTable.
pub struct LocalRouter {
    table: Rc<ProcessTable>,
}

impl LocalRouter {
    pub fn new(table: Rc<ProcessTable>) -> Self {
        Self { table }
    }
}

impl MessageRouter for LocalRouter {
    fn route(
        &self,
        from: ProcessId,
        to: ProcessId,
        payload: rmpv::Value,
    ) -> Result<(), SendError> {
        let msg = Message::new(from, payload);
        self.table.send(to, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::mailbox::Mailbox;
    use crate::process::table::ProcessHandle;

    #[test]
    fn local_router_delivers_locally() {
        let table = ProcessTable::new(1, 0);
        let pid = table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));

        let table = Rc::new(table);
        let router = LocalRouter::new(table);
        let from = ProcessId::new(1, 0, 0);
        router
            .route(from, pid, rmpv::Value::String("hello".into()))
            .unwrap();

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "hello");
    }

    #[test]
    fn local_router_rejects_unknown_pid() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let router = LocalRouter::new(table);
        let from = ProcessId::new(1, 0, 0);
        let dead_pid = ProcessId::new(1, 0, 999);

        let result = router.route(from, dead_pid, rmpv::Value::Nil);
        assert!(matches!(result, Err(SendError::ProcessDead(_))));
    }
}
