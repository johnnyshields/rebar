use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use dashmap::DashMap;
use dashmap::mapref::one::Ref;

use crate::process::mailbox::MailboxTx;
use crate::process::{Message, ProcessId, RegistryError, SendError};

/// Handle to a process, wrapping the mailbox sender.
///
/// Each process in the table has a handle that allows sending messages
/// to its mailbox.
pub struct ProcessHandle {
    tx: MailboxTx,
}

impl ProcessHandle {
    /// Create a new process handle wrapping the given mailbox sender.
    pub fn new(tx: MailboxTx) -> Self {
        Self { tx }
    }

    /// Send a message to this process's mailbox.
    pub fn send(&self, msg: Message) -> Result<(), SendError> {
        self.tx.send(msg)
    }

    /// Send a message, waiting for space if the mailbox is bounded and full.
    pub async fn send_async(&self, msg: Message) -> Result<(), SendError> {
        self.tx.send_async(msg).await
    }
}

/// Table of all processes on this node.
///
/// Uses `DashMap` for concurrent access and `AtomicU64` for lock-free
/// PID allocation. All methods are safe to call from multiple threads
/// concurrently.
pub struct ProcessTable {
    node_id: u64,
    next_id: AtomicU64,
    processes: DashMap<ProcessId, ProcessHandle>,
    names: RwLock<HashMap<String, ProcessId>>,
}

impl ProcessTable {
    /// Create a new process table for the given node ID.
    pub fn new(node_id: u64) -> Self {
        Self {
            node_id,
            next_id: AtomicU64::new(1),
            processes: DashMap::new(),
            names: RwLock::new(HashMap::new()),
        }
    }

    /// Allocate a new unique process ID on this node.
    ///
    /// Uses atomic fetch-and-add for lock-free, concurrent-safe allocation.
    /// PIDs start at 1 and increment monotonically.
    pub fn allocate_pid(&self) -> ProcessId {
        let local_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        ProcessId::new(self.node_id, local_id)
    }

    /// Insert a process handle into the table under the given PID.
    pub fn insert(&self, pid: ProcessId, handle: ProcessHandle) {
        self.processes.insert(pid, handle);
    }

    /// Look up a process by its PID.
    ///
    /// Returns a reference guard that holds a read lock on the entry.
    /// Returns `None` if the PID is not in the table.
    pub fn get(&self, pid: &ProcessId) -> Option<Ref<'_, ProcessId, ProcessHandle>> {
        self.processes.get(pid)
    }

    /// Remove a process from the table.
    ///
    /// Returns the removed PID and handle, or `None` if the PID was not found.
    pub fn remove(&self, pid: &ProcessId) -> Option<(ProcessId, ProcessHandle)> {
        self.processes.remove(pid)
    }

    /// Send a message to a process by its PID.
    ///
    /// Returns `SendError::ProcessDead` if the PID is not in the table.
    pub fn send(&self, pid: ProcessId, msg: Message) -> Result<(), SendError> {
        match self.processes.get(&pid) {
            Some(handle) => handle.send(msg),
            None => Err(SendError::ProcessDead(pid)),
        }
    }

    /// Check whether a process is alive (exists in the table).
    pub fn is_alive(&self, pid: &ProcessId) -> bool {
        self.processes.contains_key(pid)
    }

    /// Kill a process by removing it from the table.
    ///
    /// Returns `true` if the process was found and removed.
    pub fn kill(&self, pid: &ProcessId) -> bool {
        // Also clean up any name registrations for this PID
        if let Ok(mut names) = self.names.write() {
            names.retain(|_, v| v != pid);
        }
        self.processes.remove(pid).is_some()
    }

    /// Return a list of all live process IDs.
    pub fn list_pids(&self) -> Vec<ProcessId> {
        self.processes.iter().map(|entry| *entry.key()).collect()
    }

    /// Register a name for a process.
    pub fn register_name(&self, name: String, pid: ProcessId) -> Result<(), RegistryError> {
        if !self.processes.contains_key(&pid) {
            return Err(RegistryError::ProcessNotFound(pid));
        }
        let mut names = self.names.write().unwrap();
        if names.contains_key(&name) {
            return Err(RegistryError::NameAlreadyRegistered(name));
        }
        names.insert(name, pid);
        Ok(())
    }

    /// Unregister a name, returning the PID it was associated with.
    pub fn unregister_name(&self, name: &str) -> Result<ProcessId, RegistryError> {
        let mut names = self.names.write().unwrap();
        names
            .remove(name)
            .ok_or_else(|| RegistryError::NameNotFound(name.to_string()))
    }

    /// Look up a PID by its registered name.
    pub fn whereis(&self, name: &str) -> Option<ProcessId> {
        let names = self.names.read().unwrap();
        names.get(name).copied()
    }

    /// Send a message to a process by its PID, waiting for space if bounded.
    ///
    /// Returns `SendError::ProcessDead` if the PID is not in the table.
    pub async fn send_async(&self, pid: ProcessId, msg: Message) -> Result<(), SendError> {
        let handle = self.processes.get(&pid)
            .ok_or(SendError::ProcessDead(pid))?;
        handle.send_async(msg).await
    }

    /// Return the number of processes currently in the table.
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Return whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[test]
    fn allocate_pid_increments() {
        let table = ProcessTable::new(1);
        let pid1 = table.allocate_pid();
        let pid2 = table.allocate_pid();
        assert_eq!(pid1.node_id(), 1);
        assert_eq!(pid1.local_id(), 1);
        assert_eq!(pid2.local_id(), 2);
    }

    #[test]
    fn allocate_pid_node_id_preserved() {
        let table = ProcessTable::new(42);
        let pid = table.allocate_pid();
        assert_eq!(pid.node_id(), 42);
    }

    #[test]
    fn insert_and_lookup() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert!(table.get(&pid).is_some());
    }

    #[test]
    fn lookup_missing_returns_none() {
        let table = ProcessTable::new(1);
        assert!(table.get(&ProcessId::new(1, 999)).is_none());
    }

    #[test]
    fn remove_process() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        table.remove(&pid);
        assert!(table.get(&pid).is_none());
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let table = ProcessTable::new(1);
        assert!(table.remove(&ProcessId::new(1, 999)).is_none());
    }

    #[test]
    fn send_to_process() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::Nil);
        assert!(table.send(pid, msg).is_ok());
    }

    #[test]
    fn send_to_dead_process_returns_error() {
        let table = ProcessTable::new(1);
        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::Nil);
        assert!(table.send(ProcessId::new(1, 999), msg).is_err());
    }

    #[test]
    fn process_count() {
        let table = ProcessTable::new(1);
        assert_eq!(table.len(), 0);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn concurrent_allocate_pids_unique() {
        use std::collections::HashSet;
        use std::sync::Arc;
        let table = Arc::new(ProcessTable::new(1));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let t = Arc::clone(&table);
            handles.push(std::thread::spawn(move || {
                (0..100).map(|_| t.allocate_pid()).collect::<Vec<_>>()
            }));
        }
        let mut all_pids = HashSet::new();
        for h in handles {
            for pid in h.join().unwrap() {
                assert!(all_pids.insert(pid), "duplicate PID: {}", pid);
            }
        }
        assert_eq!(all_pids.len(), 1000);
    }

    #[test]
    fn concurrent_insert_and_send() {
        use std::sync::Arc;
        let table = Arc::new(ProcessTable::new(1));
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        let mut handles = Vec::new();
        for i in 0..10u64 {
            let t = Arc::clone(&table);
            handles.push(std::thread::spawn(move || {
                let msg = crate::process::Message::new(
                    ProcessId::new(1, 0),
                    rmpv::Value::Integer(i.into()),
                );
                t.send(pid, msg)
            }));
        }
        for h in handles {
            assert!(h.join().unwrap().is_ok());
        }
    }

    #[tokio::test]
    async fn send_async_through_table() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, mut rx) = crate::process::mailbox::Mailbox::bounded(1);
        table.insert(pid, ProcessHandle::new(tx));

        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::from(99));
        table.send_async(pid, msg).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.payload().as_u64(), Some(99));
    }

    #[tokio::test]
    async fn send_async_to_missing_pid_returns_error() {
        let table = ProcessTable::new(1);
        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::Nil);
        let result = table.send_async(ProcessId::new(1, 999), msg).await;
        assert!(matches!(result, Err(SendError::ProcessDead(_))));
    }

    #[test]
    fn is_empty() {
        let table = ProcessTable::new(1);
        assert!(table.is_empty());
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert!(!table.is_empty());
    }
}
