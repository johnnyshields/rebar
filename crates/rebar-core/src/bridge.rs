use crate::process::{Message, ProcessId};
use crossbeam_channel::{Receiver, Sender};

pub struct CrossThreadMessage {
    pub target_pid: ProcessId,
    pub message: Message,
}

pub struct ThreadBridge {
    senders: std::sync::Arc<Vec<Sender<CrossThreadMessage>>>,
    local_rx: Receiver<CrossThreadMessage>,
    thread_id: u16,
}

impl ThreadBridge {
    pub fn new(
        senders: std::sync::Arc<Vec<Sender<CrossThreadMessage>>>,
        local_rx: Receiver<CrossThreadMessage>,
        thread_id: u16,
    ) -> Self {
        Self {
            senders,
            local_rx,
            thread_id,
        }
    }

    /// Send a message to a specific thread.
    pub fn send_to_thread(
        &self,
        thread_id: u16,
        msg: CrossThreadMessage,
    ) -> Result<(), crossbeam_channel::SendError<CrossThreadMessage>> {
        self.senders[thread_id as usize].send(msg)
    }

    /// Try to receive a cross-thread message (non-blocking).
    pub fn try_recv(&self) -> Option<CrossThreadMessage> {
        self.local_rx.try_recv().ok()
    }

    pub fn thread_id(&self) -> u16 {
        self.thread_id
    }
}
