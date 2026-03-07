use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::process::{Message, SendError};

// ---------------------------------------------------------------------------
// Bounded capacity tracking (shared between Tx and Rx)
// ---------------------------------------------------------------------------

struct BoundedState {
    permits: Cell<usize>,
    waker: Cell<Option<Waker>>,
}

impl BoundedState {
    fn new(capacity: usize) -> Self {
        Self {
            permits: Cell::new(capacity),
            waker: Cell::new(None),
        }
    }

    fn try_acquire(&self) -> bool {
        let p = self.permits.get();
        if p > 0 {
            self.permits.set(p - 1);
            true
        } else {
            false
        }
    }

    fn release(&self) {
        self.permits.set(self.permits.get() + 1);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

// ---------------------------------------------------------------------------
// TxInner / RxInner
// ---------------------------------------------------------------------------

/// Inner sender type that handles both bounded and unbounded channels.
enum TxInner {
    Unbounded(local_sync::mpsc::unbounded::Tx<Message>),
    Bounded {
        tx: local_sync::mpsc::unbounded::Tx<Message>,
        state: Rc<BoundedState>,
    },
}

/// Inner receiver type that handles both bounded and unbounded channels.
enum RxInner {
    Unbounded(local_sync::mpsc::unbounded::Rx<Message>),
    Bounded {
        rx: local_sync::mpsc::unbounded::Rx<Message>,
        state: Rc<BoundedState>,
    },
}

impl Clone for TxInner {
    fn clone(&self) -> Self {
        match self {
            TxInner::Unbounded(tx) => TxInner::Unbounded(tx.clone()),
            TxInner::Bounded { tx, state } => TxInner::Bounded {
                tx: tx.clone(),
                state: Rc::clone(state),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// MailboxTx / MailboxRx
// ---------------------------------------------------------------------------

/// Sender half of a mailbox channel.
///
/// Wraps either a bounded or unbounded local_sync mpsc sender.
/// Implements `Clone` so multiple producers can send to the same mailbox.
#[derive(Clone)]
pub struct MailboxTx {
    inner: TxInner,
}

/// Receiver half of a mailbox channel.
///
/// Provides `recv()` for blocking receive and `recv_timeout()` for
/// time-limited receive operations.
pub struct MailboxRx {
    inner: RxInner,
}

/// Factory for creating mailbox channel pairs.
pub struct Mailbox;

impl Mailbox {
    /// Create an unbounded mailbox channel pair.
    ///
    /// The sender will never block or fail due to capacity constraints.
    /// Messages are only lost if the receiver is dropped.
    pub fn unbounded() -> (MailboxTx, MailboxRx) {
        let (tx, rx) = local_sync::mpsc::unbounded::channel();
        (
            MailboxTx {
                inner: TxInner::Unbounded(tx),
            },
            MailboxRx {
                inner: RxInner::Unbounded(rx),
            },
        )
    }

    /// Create a bounded mailbox channel pair with the given capacity.
    ///
    /// When the mailbox is full, `try_send` will return `SendError::MailboxFull`
    /// and `send` will also return an error for bounded channels at capacity.
    pub fn bounded(capacity: usize) -> (MailboxTx, MailboxRx) {
        let (tx, rx) = local_sync::mpsc::unbounded::channel();
        let state = Rc::new(BoundedState::new(capacity));
        (
            MailboxTx {
                inner: TxInner::Bounded {
                    tx,
                    state: Rc::clone(&state),
                },
            },
            MailboxRx {
                inner: RxInner::Bounded { rx, state },
            },
        )
    }
}

impl MailboxTx {
    /// Send a message to the mailbox.
    ///
    /// For unbounded channels, this only fails if the receiver has been dropped
    /// (returns `SendError::ProcessDead`).
    ///
    /// For bounded channels, this uses `try_send` semantics: if the channel is
    /// full, returns `SendError::MailboxFull`; if the receiver is dropped,
    /// returns `SendError::ProcessDead`.
    pub fn send(&self, msg: Message) -> Result<(), SendError> {
        self.try_send(msg)
    }

    /// Send a message, waiting for space if the mailbox is bounded and full.
    ///
    /// Unlike `send()`, which returns `MailboxFull` immediately when a bounded
    /// mailbox is at capacity, this method awaits until space is available.
    pub async fn send_async(&self, msg: Message) -> Result<(), SendError> {
        match &self.inner {
            TxInner::Unbounded(tx) => Self::send_unbounded(tx, msg),
            TxInner::Bounded { tx, state } => {
                let from = msg.from();

                // Wait for a permit to become available
                WaitForPermit { state }.await;

                tx.send(msg).map_err(|_| SendError::ProcessDead(from))
            }
        }
    }

    /// Try to send a message without blocking.
    ///
    /// For unbounded channels, behaves the same as `send`.
    /// For bounded channels, returns `SendError::MailboxFull` if the channel
    /// is at capacity.
    pub fn try_send(&self, msg: Message) -> Result<(), SendError> {
        match &self.inner {
            TxInner::Unbounded(tx) => Self::send_unbounded(tx, msg),
            TxInner::Bounded { tx, state } => {
                let from = msg.from();
                if state.try_acquire() {
                    tx.send(msg).map_err(|_| SendError::ProcessDead(from))
                } else {
                    // Check if receiver is actually closed
                    if tx.is_closed() {
                        Err(SendError::ProcessDead(from))
                    } else {
                        Err(SendError::MailboxFull(from))
                    }
                }
            }
        }
    }

    /// Send a message on an unbounded channel.
    ///
    /// Returns `SendError::ProcessDead` if the receiver has been dropped.
    fn send_unbounded(
        tx: &local_sync::mpsc::unbounded::Tx<Message>,
        msg: Message,
    ) -> Result<(), SendError> {
        let from = msg.from();
        tx.send(msg).map_err(|_| SendError::ProcessDead(from))
    }
}

/// Future that waits for a bounded permit to become available.
struct WaitForPermit<'a> {
    state: &'a Rc<BoundedState>,
}

impl Future for WaitForPermit<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.state.try_acquire() {
            Poll::Ready(())
        } else {
            self.state.waker.set(Some(cx.waker().clone()));
            Poll::Pending
        }
    }
}

impl MailboxRx {
    /// Try to receive a message without blocking.
    ///
    /// Returns `Some(message)` if one is immediately available,
    /// or `None` if the channel is empty or closed.
    pub fn try_recv(&mut self) -> Option<Message> {
        match &mut self.inner {
            RxInner::Unbounded(rx) => rx.try_recv().ok(),
            RxInner::Bounded { rx, state } => {
                let msg = rx.try_recv().ok()?;
                state.release();
                Some(msg)
            }
        }
    }

    /// Receive a message from the mailbox.
    ///
    /// Returns `None` if all senders have been dropped (channel is closed).
    pub async fn recv(&mut self) -> Option<Message> {
        match &mut self.inner {
            RxInner::Unbounded(rx) => rx.recv().await,
            RxInner::Bounded { rx, state } => {
                let msg = rx.recv().await?;
                state.release();
                Some(msg)
            }
        }
    }

    /// Receive a message with a timeout.
    ///
    /// Returns `Some(message)` if a message arrives within the given duration,
    /// or `None` if the timeout expires or the channel is closed.
    pub async fn recv_timeout(&mut self, duration: Duration) -> Option<Message> {
        monoio::time::timeout(duration, self.recv())
            .await
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[monoio::test(enable_timer = true)]
    async fn unbounded_send_receive() {
        let (tx, mut rx) = Mailbox::unbounded();
        let msg = Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil);
        tx.send(msg).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.from(), ProcessId::new(1, 0, 1));
    }

    #[monoio::test(enable_timer = true)]
    async fn bounded_capacity_respected() {
        let (tx, _rx) = Mailbox::bounded(1);
        let msg1 = Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil);
        let msg2 = Message::new(ProcessId::new(1, 0, 2), rmpv::Value::Nil);
        tx.send(msg1).unwrap();
        assert!(tx.try_send(msg2).is_err());
    }

    #[monoio::test(enable_timer = true)]
    async fn recv_timeout_expires() {
        let (_tx, mut rx) = Mailbox::unbounded();
        let result = rx.recv_timeout(std::time::Duration::from_millis(10)).await;
        assert!(result.is_none());
    }

    #[monoio::test(enable_timer = true)]
    async fn recv_timeout_receives_in_time() {
        let (tx, mut rx) = Mailbox::unbounded();
        let msg = Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil);
        tx.send(msg).unwrap();
        let result = rx.recv_timeout(std::time::Duration::from_millis(100)).await;
        assert!(result.is_some());
    }

    #[monoio::test(enable_timer = true)]
    async fn multiple_messages_fifo() {
        let (tx, mut rx) = Mailbox::unbounded();
        for i in 0..5u64 {
            tx.send(Message::new(
                ProcessId::new(1, 0, i),
                rmpv::Value::Integer(i.into()),
            ))
            .unwrap();
        }
        for i in 0..5u64 {
            let msg = rx.recv().await.unwrap();
            assert_eq!(msg.from().local_id(), i);
        }
    }

    #[monoio::test(enable_timer = true)]
    async fn dropped_sender_closes_receiver() {
        let (tx, mut rx) = Mailbox::unbounded();
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    #[monoio::test(enable_timer = true)]
    async fn bounded_send_after_drain() {
        let (tx, mut rx) = Mailbox::bounded(1);
        let msg1 = Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil);
        tx.send(msg1).unwrap();
        rx.recv().await.unwrap();
        let msg2 = Message::new(ProcessId::new(1, 0, 2), rmpv::Value::Nil);
        assert!(tx.send(msg2).is_ok());
    }

    #[monoio::test(enable_timer = true)]
    async fn send_to_closed_receiver_returns_error() {
        let (tx, rx) = Mailbox::unbounded();
        drop(rx);
        let msg = Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil);
        assert!(tx.send(msg).is_err());
    }

    #[monoio::test(enable_timer = true)]
    async fn recv_timeout_zero_duration() {
        let (_tx, mut rx) = Mailbox::unbounded();
        let result = rx.recv_timeout(std::time::Duration::ZERO).await;
        assert!(result.is_none());
    }

    #[monoio::test(enable_timer = true)]
    async fn large_message_throughput() {
        let (tx, mut rx) = Mailbox::unbounded();
        let count = 10_000;
        for i in 0..count {
            tx.send(Message::new(
                ProcessId::new(1, 0, 1),
                rmpv::Value::Integer(i.into()),
            ))
            .unwrap();
        }
        for _ in 0..count {
            rx.recv().await.unwrap();
        }
    }

    #[monoio::test(enable_timer = true)]
    async fn bounded_try_send_full_returns_mailbox_full() {
        let (tx, _rx) = Mailbox::bounded(1);
        tx.send(Message::new(ProcessId::new(1, 0, 1), rmpv::Value::Nil))
            .unwrap();
        let result = tx.try_send(Message::new(ProcessId::new(1, 0, 2), rmpv::Value::Nil));
        match result {
            Err(SendError::MailboxFull(_)) => {}
            other => panic!("expected MailboxFull, got {:?}", other),
        }
    }

    #[monoio::test(enable_timer = true)]
    async fn send_async_waits_for_space() {
        let (tx, mut rx) = Mailbox::bounded(1);
        let pid = ProcessId::new(0, 0, 1);

        // Fill the mailbox
        let msg1 = Message::new(pid, rmpv::Value::from(1));
        tx.send(msg1).unwrap();

        // Regular send should fail (mailbox full)
        let msg2 = Message::new(pid, rmpv::Value::from(2));
        assert!(matches!(tx.send(msg2), Err(SendError::MailboxFull(_))));

        // send_async should wait, then succeed when space is made
        let tx_clone = tx.clone();
        let send_handle = monoio::spawn(async move {
            let msg3 = Message::new(pid, rmpv::Value::from(3));
            tx_clone.send_async(msg3).await.unwrap();
        });

        // Yield to let the sender start waiting
        monoio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Consume a message to make space
        let received = rx.recv().await.unwrap();
        assert_eq!(received.payload().as_u64(), Some(1));

        // The send_async should now complete
        send_handle.await;

        // Verify the async-sent message is in the mailbox
        let received = rx.recv().await.unwrap();
        assert_eq!(received.payload().as_u64(), Some(3));
    }

    #[monoio::test(enable_timer = true)]
    async fn send_async_to_closed_bounded_returns_process_dead() {
        let (tx, rx) = Mailbox::bounded(10);
        drop(rx);
        let pid = ProcessId::new(0, 0, 1);
        let msg = Message::new(pid, rmpv::Value::from(1));
        let result = tx.send_async(msg).await;
        assert!(matches!(result, Err(SendError::ProcessDead(_))));
    }

    #[monoio::test(enable_timer = true)]
    async fn send_async_to_closed_unbounded_returns_process_dead() {
        let (tx, rx) = Mailbox::unbounded();
        drop(rx);
        let pid = ProcessId::new(0, 0, 1);
        let msg = Message::new(pid, rmpv::Value::from(1));
        let result = tx.send_async(msg).await;
        assert!(matches!(result, Err(SendError::ProcessDead(_))));
    }

    #[monoio::test(enable_timer = true)]
    async fn send_async_unbounded_works() {
        let (tx, mut rx) = Mailbox::unbounded();
        let pid = ProcessId::new(0, 0, 1);

        let msg = Message::new(pid, rmpv::Value::from(42));
        tx.send_async(msg).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.payload().as_u64(), Some(42));
    }
}
