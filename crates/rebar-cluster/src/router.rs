use std::rc::Rc;

use local_sync::mpsc::unbounded::Tx;

use crate::protocol::{Frame, MsgType};
use rebar_core::process::table::ProcessTable;
use rebar_core::process::{Message, ProcessId, SendError};
use rebar_core::router::MessageRouter;

/// Commands sent to the remote transport layer for cross-node delivery.
#[derive(Debug)]
pub enum RouterCommand {
    Send { node_id: u64, frame: Frame },
}

/// A distributed message router that delivers messages locally when the
/// target is on this node, or encodes them as frames and forwards to
/// the remote transport layer for cross-node delivery.
pub struct DistributedRouter {
    node_id: u64,
    table: Rc<ProcessTable>,
    remote_tx: Tx<RouterCommand>,
}

impl DistributedRouter {
    pub fn new(
        node_id: u64,
        table: Rc<ProcessTable>,
        remote_tx: Tx<RouterCommand>,
    ) -> Self {
        Self {
            node_id,
            table,
            remote_tx,
        }
    }
}

impl MessageRouter for DistributedRouter {
    fn route(&self, from: ProcessId, to: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        if to.node_id() == self.node_id {
            // Local delivery
            let msg = Message::new(from, payload);
            self.table.send(to, msg)
        } else {
            // Remote delivery: encode as frame and send to transport
            let frame = encode_send_frame(from, to, payload);
            self.remote_tx
                .send(RouterCommand::Send {
                    node_id: to.node_id(),
                    frame,
                })
                .map_err(|_| SendError::NodeUnreachable(to.node_id()))
        }
    }
}

/// Encode a send message into a wire protocol frame.
pub fn encode_send_frame(from: ProcessId, to: ProcessId, payload: rmpv::Value) -> Frame {
    Frame {
        version: 1,
        msg_type: MsgType::Send,
        request_id: 0,
        header: rmpv::Value::Map(vec![
            (
                rmpv::Value::String("from_node".into()),
                rmpv::Value::Integer(from.node_id().into()),
            ),
            (
                rmpv::Value::String("from_local".into()),
                rmpv::Value::Integer(from.local_id().into()),
            ),
            (
                rmpv::Value::String("to_node".into()),
                rmpv::Value::Integer(to.node_id().into()),
            ),
            (
                rmpv::Value::String("to_local".into()),
                rmpv::Value::Integer(to.local_id().into()),
            ),
        ]),
        payload,
    }
}

/// Extract a u64 value from a msgpack Value, returning a SendError if it is not a u64.
fn require_u64(value: &rmpv::Value, field: &'static str) -> Result<u64, SendError> {
    value.as_u64().ok_or_else(|| SendError::MalformedFrame(field))
}

/// Deliver an inbound frame from the network to a local process.
///
/// Extracts addressing information from the frame header and delivers the
/// payload to the target process's mailbox via the process table.
pub fn deliver_inbound_frame(table: &ProcessTable, frame: &Frame) -> Result<(), SendError> {
    let header = frame.header.as_map()
        .ok_or_else(|| SendError::MalformedFrame("frame header must be a Map"))?;

    let mut from_node: u64 = 0;
    let mut from_local: u64 = 0;
    let mut to_node: u64 = 0;
    let mut to_local: u64 = 0;
    let mut has_to_node = false;
    let mut has_to_local = false;

    for (key, value) in header {
        let key_str = key.as_str().unwrap_or("");
        match key_str {
            "from_node" => from_node = require_u64(value, "from_node must be u64")?,
            "from_local" => from_local = require_u64(value, "from_local must be u64")?,
            "to_node" => {
                to_node = require_u64(value, "to_node must be u64")?;
                has_to_node = true;
            }
            "to_local" => {
                to_local = require_u64(value, "to_local must be u64")?;
                has_to_local = true;
            }
            _ => {}
        }
    }

    if !has_to_node || !has_to_local {
        return Err(SendError::MalformedFrame("missing required addressing fields: to_node and to_local"));
    }

    let from = ProcessId::new(from_node, 0, from_local);
    let to = ProcessId::new(to_node, 0, to_local);
    let msg = Message::new(from, frame.payload.clone());
    table.send(to, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebar_core::process::mailbox::Mailbox;
    use rebar_core::process::table::ProcessHandle;

    #[test]
    fn distributed_router_routes_local() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let pid = table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));

        let (remote_tx, _remote_rx) = local_sync::mpsc::unbounded::channel();
        let router = DistributedRouter::new(1, Rc::clone(&table), remote_tx);

        let from = ProcessId::new(1, 0, 0);
        router
            .route(from, pid, rmpv::Value::String("local-msg".into()))
            .unwrap();

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "local-msg");
    }

    #[monoio::test(enable_timer = true)]
    async fn distributed_router_routes_remote() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let (remote_tx, mut remote_rx) = local_sync::mpsc::unbounded::channel();
        let router = DistributedRouter::new(1, Rc::clone(&table), remote_tx);

        let from = ProcessId::new(1, 0, 5);
        let remote_pid = ProcessId::new(2, 0, 10);

        router
            .route(from, remote_pid, rmpv::Value::String("remote-msg".into()))
            .unwrap();

        let cmd = remote_rx.recv().await.unwrap();
        match cmd {
            RouterCommand::Send { node_id, frame } => {
                assert_eq!(node_id, 2);
                assert_eq!(frame.msg_type, MsgType::Send);
                assert_eq!(frame.payload, rmpv::Value::String("remote-msg".into()));
            }
        }
    }

    #[test]
    fn distributed_router_node_unreachable() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let (remote_tx, remote_rx) = local_sync::mpsc::unbounded::channel();
        let router = DistributedRouter::new(1, Rc::clone(&table), remote_tx);

        // Drop receiver so send fails
        drop(remote_rx);

        let from = ProcessId::new(1, 0, 0);
        let remote_pid = ProcessId::new(2, 0, 1);

        let result = router.route(from, remote_pid, rmpv::Value::Nil);
        assert_eq!(result, Err(SendError::NodeUnreachable(2)));
    }

    #[test]
    fn inbound_frame_delivers_to_local_process() {
        let table = Rc::new(ProcessTable::new(2, 0));
        let pid = table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));

        let frame = encode_send_frame(
            ProcessId::new(1, 0, 5),
            pid,
            rmpv::Value::String("from-remote".into()),
        );

        deliver_inbound_frame(&table, &frame).unwrap();

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "from-remote");
        assert_eq!(msg.from().node_id(), 1);
        assert_eq!(msg.from().local_id(), 5);
    }

    #[test]
    fn encode_send_frame_has_correct_fields() {
        let from = ProcessId::new(1, 0, 5);
        let to = ProcessId::new(2, 0, 10);
        let frame = encode_send_frame(from, to, rmpv::Value::Integer(42.into()));

        assert_eq!(frame.version, 1);
        assert_eq!(frame.msg_type, MsgType::Send);
        assert_eq!(frame.payload, rmpv::Value::Integer(42.into()));
        let header = frame.header.as_map().unwrap();
        assert_eq!(header.len(), 4);
    }

    #[test]
    fn malformed_header_type() {
        let table = ProcessTable::new(1, 0);
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::String("not-a-map".into()),
            payload: rmpv::Value::Nil,
        };
        let result = deliver_inbound_frame(&table, &frame);
        assert_eq!(result, Err(SendError::MalformedFrame("frame header must be a Map")));
    }

    #[test]
    fn malformed_field_type() {
        let table = ProcessTable::new(1, 0);
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::Map(vec![(
                rmpv::Value::String("from_node".into()),
                rmpv::Value::String("not-a-u64".into()),
            )]),
            payload: rmpv::Value::Nil,
        };
        let result = deliver_inbound_frame(&table, &frame);
        assert_eq!(result, Err(SendError::MalformedFrame("from_node must be u64")));
    }

    #[test]
    fn missing_fields_returns_malformed_frame() {
        let table = ProcessTable::new(1, 0);
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::Map(vec![]),
            payload: rmpv::Value::Nil,
        };
        let result = deliver_inbound_frame(&table, &frame);
        assert_eq!(
            result,
            Err(SendError::MalformedFrame("missing required addressing fields: to_node and to_local"))
        );
    }
}
