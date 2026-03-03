pub use rebar_core::router::{LocalRouter, MessageRouter};
pub use rebar_core::*;

use std::sync::Arc;
use tokio::sync::mpsc;

use rebar_cluster::connection::manager::ConnectionManager;
use rebar_cluster::protocol::Frame;
use rebar_cluster::router::{DistributedRouter, RouterCommand, deliver_inbound_frame};
use rebar_core::process::SendError;
use rebar_core::process::table::ProcessTable;
use rebar_core::runtime::Runtime;

/// A fully wired distributed runtime bridging rebar-core and rebar-cluster.
pub struct DistributedRuntime {
    runtime: Runtime,
    table: Arc<ProcessTable>,
    connection_manager: ConnectionManager,
    remote_rx: mpsc::Receiver<RouterCommand>,
}

impl DistributedRuntime {
    pub fn new(node_id: u64, connection_manager: ConnectionManager) -> Self {
        let table = Arc::new(ProcessTable::new(node_id));
        let (remote_tx, remote_rx) = mpsc::channel(1024);
        let router = Arc::new(DistributedRouter::new(
            node_id,
            Arc::clone(&table),
            remote_tx,
        ));
        let runtime = Runtime::with_router(node_id, Arc::clone(&table), router);

        Self {
            runtime,
            table,
            connection_manager,
            remote_rx,
        }
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn table(&self) -> &Arc<ProcessTable> {
        &self.table
    }

    pub fn connection_manager_mut(&mut self) -> &mut ConnectionManager {
        &mut self.connection_manager
    }

    /// Process one pending outbound remote message.
    /// Returns true if a message was processed.
    pub async fn process_outbound(&mut self) -> bool {
        match self.remote_rx.try_recv() {
            Ok(RouterCommand::Send { node_id, frame }) => {
                let _ = self.connection_manager.route(node_id, &frame).await;
                true
            }
            Err(_) => false,
        }
    }

    /// Deliver an inbound frame to a local process.
    pub fn deliver_inbound(&self, frame: &Frame) -> Result<(), SendError> {
        deliver_inbound_frame(&self.table, frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebar_cluster::connection::manager::TransportConnector;
    use rebar_cluster::protocol::MsgType;
    use rebar_cluster::transport::{TransportConnection, TransportError};
    use rebar_core::process::mailbox::Mailbox;
    use rebar_core::process::table::ProcessHandle;
    use std::sync::Mutex;

    struct MockConnector;

    #[async_trait::async_trait]
    impl TransportConnector for MockConnector {
        async fn connect(
            &self,
            _addr: std::net::SocketAddr,
        ) -> Result<Box<dyn TransportConnection>, TransportError> {
            Ok(Box::new(MockConn {
                sent: Arc::new(Mutex::new(Vec::new())),
            }))
        }
    }

    struct MockConn {
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[async_trait::async_trait]
    impl TransportConnection for MockConn {
        async fn send(&mut self, frame: &Frame) -> Result<(), TransportError> {
            self.sent.lock().unwrap().push(frame.encode());
            Ok(())
        }
        async fn recv(&mut self) -> Result<Frame, TransportError> {
            Err(TransportError::ConnectionClosed)
        }
        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn distributed_runtime_local_send() {
        let mgr = ConnectionManager::new(Box::new(MockConnector));
        let drt = DistributedRuntime::new(1, mgr);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let receiver = drt
            .runtime()
            .spawn(move |mut ctx| async move {
                let msg = ctx.recv().await.unwrap();
                done_tx
                    .send(msg.payload().as_str().unwrap().to_string())
                    .unwrap();
            })
            .await;

        drt.runtime()
            .spawn(move |ctx| async move {
                ctx.send(receiver, rmpv::Value::String("local".into()))
                    .await
                    .unwrap();
            })
            .await;

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), done_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, "local");
    }

    #[tokio::test]
    async fn distributed_runtime_inbound_delivery() {
        let mgr = ConnectionManager::new(Box::new(MockConnector));
        let drt = DistributedRuntime::new(2, mgr);

        let pid = drt.table().allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        drt.table().insert(pid, ProcessHandle::new(tx));

        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::Map(vec![
                (
                    rmpv::Value::String("from_node".into()),
                    rmpv::Value::Integer(1u64.into()),
                ),
                (
                    rmpv::Value::String("from_local".into()),
                    rmpv::Value::Integer(5u64.into()),
                ),
                (
                    rmpv::Value::String("to_node".into()),
                    rmpv::Value::Integer(pid.node_id().into()),
                ),
                (
                    rmpv::Value::String("to_local".into()),
                    rmpv::Value::Integer(pid.local_id().into()),
                ),
            ]),
            payload: rmpv::Value::String("from-remote-node".into()),
        };

        drt.deliver_inbound(&frame).unwrap();

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "from-remote-node");
    }
}
