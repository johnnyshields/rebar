use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::gen_server::types::{CallEnvelope, CallError, GenServer, GenServerContext};
use crate::process::mailbox::Mailbox;
use crate::process::table::ProcessHandle;
use crate::process::{ExitReason, Message, ProcessId, SendError};
use crate::runtime::Runtime;

/// Handle to a running GenServer, used by clients to send calls and casts.
pub struct GenServerRef<S: GenServer> {
    pid: ProcessId,
    call_tx: mpsc::Sender<CallEnvelope<S>>,
    cast_tx: mpsc::UnboundedSender<S::Cast>,
}

// Manual Clone because derive requires S: Clone which we don't need
impl<S: GenServer> Clone for GenServerRef<S> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            call_tx: self.call_tx.clone(),
            cast_tx: self.cast_tx.clone(),
        }
    }
}

// Safety: GenServerRef only contains channel senders which are Send+Sync
unsafe impl<S: GenServer> Sync for GenServerRef<S> {}

impl<S: GenServer> GenServerRef<S> {
    /// The PID of the GenServer process.
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Send a synchronous call and wait for the reply.
    pub async fn call(&self, msg: S::Call, timeout: Duration) -> Result<S::Reply, CallError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let envelope = CallEnvelope {
            msg,
            from: ProcessId::new(0, 0), // caller PID not tracked for now
            reply_tx,
        };
        self.call_tx
            .send(envelope)
            .await
            .map_err(|_| CallError::ServerDead)?;

        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(CallError::ServerDead),
            Err(_) => Err(CallError::Timeout),
        }
    }

    /// Send an asynchronous cast (fire-and-forget).
    pub fn cast(&self, msg: S::Cast) -> Result<(), SendError> {
        self.cast_tx
            .send(msg)
            .map_err(|_| SendError::ProcessDead(self.pid))
    }
}

/// Spawn a GenServer as a process in the runtime.
///
/// Returns a typed GenServerRef for interacting with the server.
pub async fn spawn_gen_server<S: GenServer>(
    runtime: Arc<Runtime>,
    server: S,
) -> GenServerRef<S> {
    let pid = runtime.table().allocate_pid();

    // Typed channels for call/cast
    let (call_tx, mut call_rx) = mpsc::channel::<CallEnvelope<S>>(64);
    let (cast_tx, mut cast_rx) = mpsc::unbounded_channel::<S::Cast>();

    // Standard mailbox for handle_info
    let (mailbox_tx, mut mailbox_rx) = Mailbox::unbounded();
    runtime
        .table()
        .insert(pid, ProcessHandle::new(mailbox_tx));

    let table = Arc::clone(runtime.table());

    // Create a LocalRouter for the GenServer context to send messages
    let local_router: Arc<dyn crate::router::MessageRouter> =
        Arc::new(crate::router::LocalRouter::new(Arc::clone(runtime.table())));

    let ctx = GenServerContext::with_router(pid, local_router);

    // Spawn the event loop
    tokio::spawn(async move {
        let inner = tokio::spawn(async move {
            // Initialize
            let mut state = match server.init(&ctx).await {
                Ok(s) => s,
                Err(_e) => {
                    return;
                }
            };

            // Event loop: select between call, cast, and info channels
            loop {
                tokio::select! {
                    biased;

                    // Prioritize calls (they have timeouts)
                    call = call_rx.recv() => {
                        match call {
                            Some(envelope) => {
                                let reply = server
                                    .handle_call(envelope.msg, envelope.from, &mut state, &ctx)
                                    .await;
                                let _ = envelope.reply_tx.send(reply);
                            }
                            None => {
                                // All call senders dropped
                                server.terminate(ExitReason::Normal, &mut state).await;
                                break;
                            }
                        }
                    }

                    cast = cast_rx.recv() => {
                        match cast {
                            Some(msg) => {
                                server.handle_cast(msg, &mut state, &ctx).await;
                            }
                            None => {
                                server.terminate(ExitReason::Normal, &mut state).await;
                                break;
                            }
                        }
                    }

                    info = mailbox_rx.recv() => {
                        match info {
                            Some(msg) => {
                                server.handle_info(msg, &mut state, &ctx).await;
                            }
                            None => {
                                server.terminate(ExitReason::Normal, &mut state).await;
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Panic isolation: whether inner completes or panics, clean up
        let _ = inner.await;
        table.remove(&pid);
    });

    GenServerRef {
        pid,
        call_tx,
        cast_tx,
    }
}
