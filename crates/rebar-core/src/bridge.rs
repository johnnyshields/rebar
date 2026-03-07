use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use crate::process::table::ProcessTable;
use crate::process::{Message, ProcessId, SendError};
use crate::router::MessageRouter;

/// Cross-thread message envelope.
pub struct CrossThreadMessage {
    pub target_pid: ProcessId,
    pub message: Message,
}

/// Low-level bridge for cross-thread message delivery.
///
/// Each thread owns a `ThreadBridge` with access to all threads' crossbeam
/// senders and its own local receiver. When a message targets a different
/// thread, it is sent via crossbeam and the target thread is woken via eventfd
/// (Linux) or a self-pipe (non-Linux).
pub struct ThreadBridge {
    senders: Arc<Vec<Sender<CrossThreadMessage>>>,
    local_rx: Receiver<CrossThreadMessage>,
    thread_id: u16,
    eventfds: Arc<Vec<RawFd>>,
}

impl ThreadBridge {
    pub fn new(
        senders: Arc<Vec<Sender<CrossThreadMessage>>>,
        local_rx: Receiver<CrossThreadMessage>,
        thread_id: u16,
        eventfds: Arc<Vec<RawFd>>,
    ) -> Self {
        Self {
            senders,
            local_rx,
            thread_id,
            eventfds,
        }
    }

    /// Send a message to a specific thread and wake it via eventfd/pipe.
    pub fn send_to_thread(
        &self,
        thread_id: u16,
        msg: CrossThreadMessage,
    ) -> Result<(), SendError> {
        self.senders[thread_id as usize]
            .send(msg)
            .map_err(|_| SendError::ProcessDead(ProcessId::new(0, thread_id, 0)))?;

        // Wake the target thread
        wake_fd(self.eventfds[thread_id as usize]);
        Ok(())
    }

    /// Try to receive a cross-thread message (non-blocking).
    pub fn try_recv(&self) -> Option<CrossThreadMessage> {
        self.local_rx.try_recv().ok()
    }

    /// Return this bridge's thread ID.
    pub fn thread_id(&self) -> u16 {
        self.thread_id
    }

    /// Return the local receiver (for drain task setup).
    pub fn local_rx(&self) -> &Receiver<CrossThreadMessage> {
        &self.local_rx
    }

    /// Return the eventfd/pipe fd for this thread (for async read).
    pub fn local_eventfd(&self) -> RawFd {
        self.eventfds[self.thread_id as usize]
    }

    /// Drain all pending cross-thread messages into the local ProcessTable.
    pub fn drain_into(&self, table: &ProcessTable) {
        while let Ok(msg) = self.local_rx.try_recv() {
            let _ = table.send(msg.target_pid, msg.message);
        }
    }
}

/// Router that delivers messages locally or across threads via ThreadBridge.
pub struct ThreadBridgeRouter {
    local_thread_id: u16,
    table: Rc<ProcessTable>,
    bridge: Rc<ThreadBridge>,
}

impl ThreadBridgeRouter {
    pub fn new(
        local_thread_id: u16,
        table: Rc<ProcessTable>,
        bridge: Rc<ThreadBridge>,
    ) -> Self {
        Self {
            local_thread_id,
            table,
            bridge,
        }
    }
}

impl MessageRouter for ThreadBridgeRouter {
    fn route(
        &self,
        from: ProcessId,
        to: ProcessId,
        payload: rmpv::Value,
    ) -> Result<(), SendError> {
        let msg = Message::new(from, payload);
        if to.thread_id() == self.local_thread_id {
            // Local delivery
            self.table.send(to, msg)
        } else {
            // Cross-thread delivery
            self.bridge.send_to_thread(
                to.thread_id(),
                CrossThreadMessage {
                    target_pid: to,
                    message: msg,
                },
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Platform-specific wake mechanism
// ---------------------------------------------------------------------------

/// Create a wake file descriptor.
///
/// On Linux, uses eventfd. On other platforms, uses a self-pipe and returns
/// the write end (the read end should be stored separately for drain tasks).
#[cfg(target_os = "linux")]
pub fn create_wake_fd() -> RawFd {
    unsafe { libc::eventfd(0, libc::EFD_NONBLOCK) }
}

#[cfg(not(target_os = "linux"))]
pub fn create_wake_fd() -> RawFd {
    let mut fds = [0i32; 2];
    unsafe {
        libc::pipe(fds.as_mut_ptr());
        // Set non-blocking on both ends
        libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
    }
    // Return write end; the read end is fds[0].
    // For simplicity in the bridge, we use a single fd per thread.
    // In practice, the multi-thread bootstrap creates matched pairs.
    fds[1]
}

/// Wake a thread by writing to its eventfd/pipe.
fn wake_fd(fd: RawFd) {
    #[cfg(target_os = "linux")]
    {
        let val: u64 = 1;
        unsafe {
            libc::write(fd, &val as *const u64 as *const libc::c_void, 8);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let val: u8 = 1;
        unsafe {
            libc::write(fd, &val as *const u8 as *const libc::c_void, 1);
        }
    }
}

/// Drain (clear) a wake fd after reading.
pub fn drain_wake_fd(fd: RawFd) {
    #[cfg(target_os = "linux")]
    {
        let mut val: u64 = 0;
        unsafe {
            libc::read(fd, &mut val as *mut u64 as *mut libc::c_void, 8);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut buf = [0u8; 64];
        unsafe {
            libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
        }
    }
}

/// Create N wake file descriptors for N threads.
pub fn create_wake_fds(n: usize) -> Vec<RawFd> {
    (0..n).map(|_| create_wake_fd()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::mailbox::Mailbox;
    use crate::process::table::ProcessHandle;

    #[test]
    fn thread_bridge_router_local_delivery() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let pid = table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));

        let (sender, receiver) = crossbeam_channel::unbounded();
        let senders = Arc::new(vec![sender]);
        let eventfds = Arc::new(vec![create_wake_fd()]);
        let bridge = Rc::new(ThreadBridge::new(senders, receiver, 0, eventfds));
        let router = ThreadBridgeRouter::new(0, Rc::clone(&table), bridge);

        let from = ProcessId::new(1, 0, 0);
        router
            .route(from, pid, rmpv::Value::String("local".into()))
            .unwrap();

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "local");
    }

    #[test]
    fn thread_bridge_router_cross_thread_delivery() {
        // Thread 0's table
        let table0 = Rc::new(ProcessTable::new(1, 0));

        // Thread 1's table
        let table1 = ProcessTable::new(1, 1);
        let target_pid = table1.allocate_pid(); // pid on thread 1
        let (tx1, mut rx1) = Mailbox::unbounded();
        table1.insert(target_pid, ProcessHandle::new(tx1));

        // Create bridge channels for 2 threads
        let (sender0, receiver0) = crossbeam_channel::unbounded();
        let (sender1, receiver1) = crossbeam_channel::unbounded();
        let senders = Arc::new(vec![sender0, sender1]);
        let eventfds = Arc::new(create_wake_fds(2));

        // Thread 0's bridge
        let bridge0 = Rc::new(ThreadBridge::new(
            Arc::clone(&senders),
            receiver0,
            0,
            Arc::clone(&eventfds),
        ));
        let router0 = ThreadBridgeRouter::new(0, Rc::clone(&table0), bridge0);

        // Route from thread 0 to thread 1's pid
        let from = ProcessId::new(1, 0, 1);
        router0
            .route(from, target_pid, rmpv::Value::String("cross".into()))
            .unwrap();

        // Simulate thread 1 draining its receiver
        let cross_msg = receiver1.try_recv().unwrap();
        assert_eq!(cross_msg.target_pid, target_pid);
        table1.send(cross_msg.target_pid, cross_msg.message).unwrap();

        let msg = rx1.try_recv().unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "cross");
    }

    #[test]
    fn drain_into_delivers_messages() {
        let table = Rc::new(ProcessTable::new(1, 0));
        let pid = table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));

        let (sender, receiver) = crossbeam_channel::unbounded();
        let senders = Arc::new(vec![sender.clone()]);
        let eventfds = Arc::new(vec![create_wake_fd()]);
        let bridge = ThreadBridge::new(Arc::clone(&senders), receiver, 0, eventfds);

        // Simulate incoming cross-thread message
        let msg = Message::new(ProcessId::new(1, 1, 1), rmpv::Value::String("drained".into()));
        sender
            .send(CrossThreadMessage {
                target_pid: pid,
                message: msg,
            })
            .unwrap();

        bridge.drain_into(&table);

        let received = rx.try_recv().unwrap();
        assert_eq!(received.payload().as_str().unwrap(), "drained");
    }

    #[test]
    fn wake_fd_roundtrip() {
        let fd = create_wake_fd();
        wake_fd(fd);
        drain_wake_fd(fd);
        // Should not block — just verify no panic
    }
}
