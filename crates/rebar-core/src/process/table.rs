use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::process::mailbox::MailboxTx;
use crate::process::{Message, ProcessId, RegistryError, SendError};

/// Global (cross-thread) named process registry.
///
/// Shared across all threads via `Arc`. Uses `RwLock` for thread-safe access.
/// Maps name -> (thread_id, ProcessId).
pub type GlobalRegistry = Arc<RwLock<HashMap<String, (u16, ProcessId)>>>;

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

/// Table of all processes on this thread.
///
/// Uses `HashMap` with `RefCell` for single-threaded interior mutability
/// (thread-per-core model). All methods take `&self` so the table can be
/// shared via `Rc<ProcessTable>`.
pub struct ProcessTable {
    node_id: u64,
    thread_id: u16,
    next_id: Cell<u64>,
    processes: RefCell<HashMap<ProcessId, ProcessHandle>>,
    names: RefCell<HashMap<String, ProcessId>>,
    global_registry: Option<GlobalRegistry>,
}

impl ProcessTable {
    /// Create a new process table for the given node ID and thread ID.
    pub fn new(node_id: u64, thread_id: u16) -> Self {
        Self {
            node_id,
            thread_id,
            next_id: Cell::new(1),
            processes: RefCell::new(HashMap::new()),
            names: RefCell::new(HashMap::new()),
            global_registry: None,
        }
    }

    /// Attach a global registry for cross-thread name lookups.
    pub fn with_global_registry(mut self, registry: GlobalRegistry) -> Self {
        self.global_registry = Some(registry);
        self
    }

    /// Allocate a new unique process ID on this node/thread.
    ///
    /// PIDs start at 1 and increment monotonically.
    pub fn allocate_pid(&self) -> ProcessId {
        let local_id = self.next_id.get();
        self.next_id.set(local_id + 1);
        ProcessId::new(self.node_id, self.thread_id, local_id)
    }

    /// Insert a process handle into the table under the given PID.
    pub fn insert(&self, pid: ProcessId, handle: ProcessHandle) {
        self.processes.borrow_mut().insert(pid, handle);
    }

    /// Look up a process by its PID and send it a message.
    ///
    /// Returns `None` if the PID is not in the table.
    /// Note: Returns bool for existence check since we can't return a ref
    /// through RefCell without a guard.
    pub fn get_exists(&self, pid: &ProcessId) -> bool {
        self.processes.borrow().contains_key(pid)
    }

    /// Remove a process from the table.
    ///
    /// Returns the removed handle, or `None` if the PID was not found.
    pub fn remove(&self, pid: &ProcessId) -> Option<ProcessHandle> {
        self.names.borrow_mut().retain(|_, v| v != pid);
        self.processes.borrow_mut().remove(pid)
    }

    /// Send a message to a process by its PID.
    ///
    /// Returns `SendError::ProcessDead` if the PID is not in the table.
    pub fn send(&self, pid: ProcessId, msg: Message) -> Result<(), SendError> {
        let processes = self.processes.borrow();
        match processes.get(&pid) {
            Some(handle) => handle.send(msg),
            None => Err(SendError::ProcessDead(pid)),
        }
    }

    /// Check whether a process is alive (exists in the table).
    pub fn is_alive(&self, pid: &ProcessId) -> bool {
        self.processes.borrow().contains_key(pid)
    }

    /// Kill a process by removing it from the table.
    ///
    /// Returns `true` if the process was found and removed.
    pub fn kill(&self, pid: &ProcessId) -> bool {
        self.names.borrow_mut().retain(|_, v| v != pid);
        self.processes.borrow_mut().remove(pid).is_some()
    }

    /// Return a list of all live process IDs.
    pub fn list_pids(&self) -> Vec<ProcessId> {
        self.processes.borrow().keys().copied().collect()
    }

    /// Register a name for a process.
    pub fn register_name(&self, name: String, pid: ProcessId) -> Result<(), RegistryError> {
        if !self.processes.borrow().contains_key(&pid) {
            return Err(RegistryError::ProcessNotFound(pid));
        }
        let mut names = self.names.borrow_mut();
        if names.contains_key(&name) {
            return Err(RegistryError::NameAlreadyRegistered(name));
        }
        names.insert(name, pid);
        Ok(())
    }

    /// Unregister a name, returning the PID it was associated with.
    pub fn unregister_name(&self, name: &str) -> Result<ProcessId, RegistryError> {
        self.names
            .borrow_mut()
            .remove(name)
            .ok_or_else(|| RegistryError::NameNotFound(name.to_string()))
    }

    /// Look up a PID by its registered name.
    ///
    /// Checks the local registry first, then falls back to the global registry
    /// if one is attached.
    pub fn whereis(&self, name: &str) -> Option<ProcessId> {
        if let Some(pid) = self.names.borrow().get(name).copied() {
            return Some(pid);
        }
        if let Some(ref global) = self.global_registry
            && let Ok(map) = global.read()
        {
            return map.get(name).map(|(_tid, pid)| *pid);
        }
        None
    }

    /// Register a name in the global (cross-thread) registry.
    ///
    /// Returns an error if no global registry is attached or the name is
    /// already taken.
    pub fn register_global(&self, name: String, pid: ProcessId) -> Result<(), RegistryError> {
        let global = self
            .global_registry
            .as_ref()
            .ok_or_else(|| RegistryError::NameNotFound("no global registry".to_string()))?;
        let mut map = global.write().map_err(|_| {
            RegistryError::NameNotFound("global registry lock poisoned".to_string())
        })?;
        if map.contains_key(&name) {
            return Err(RegistryError::NameAlreadyRegistered(name));
        }
        map.insert(name, (self.thread_id, pid));
        Ok(())
    }

    /// Unregister a name from the global (cross-thread) registry.
    pub fn unregister_global(&self, name: &str) -> Result<ProcessId, RegistryError> {
        let global = self
            .global_registry
            .as_ref()
            .ok_or_else(|| RegistryError::NameNotFound("no global registry".to_string()))?;
        let mut map = global.write().map_err(|_| {
            RegistryError::NameNotFound("global registry lock poisoned".to_string())
        })?;
        map.remove(name)
            .map(|(_tid, pid)| pid)
            .ok_or_else(|| RegistryError::NameNotFound(name.to_string()))
    }

    /// Return the number of processes currently in the table.
    pub fn len(&self) -> usize {
        self.processes.borrow().len()
    }

    /// Return whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.processes.borrow().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[test]
    fn allocate_pid_increments() {
        let table = ProcessTable::new(1, 0);
        let pid1 = table.allocate_pid();
        let pid2 = table.allocate_pid();
        assert_eq!(pid1.node_id(), 1);
        assert_eq!(pid1.local_id(), 1);
        assert_eq!(pid2.local_id(), 2);
    }

    #[test]
    fn allocate_pid_node_id_preserved() {
        let table = ProcessTable::new(42, 0);
        let pid = table.allocate_pid();
        assert_eq!(pid.node_id(), 42);
    }

    #[test]
    fn insert_and_lookup() {
        let table = ProcessTable::new(1, 0);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert!(table.is_alive(&pid));
    }

    #[test]
    fn lookup_missing_returns_none() {
        let table = ProcessTable::new(1, 0);
        assert!(!table.is_alive(&ProcessId::new(1, 0, 999)));
    }

    #[test]
    fn remove_process() {
        let table = ProcessTable::new(1, 0);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        table.remove(&pid);
        assert!(!table.is_alive(&pid));
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let table = ProcessTable::new(1, 0);
        assert!(table.remove(&ProcessId::new(1, 0, 999)).is_none());
    }

    #[test]
    fn send_to_process() {
        let table = ProcessTable::new(1, 0);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        let msg = crate::process::Message::new(ProcessId::new(1, 0, 0), rmpv::Value::Nil);
        assert!(table.send(pid, msg).is_ok());
    }

    #[test]
    fn send_to_dead_process_returns_error() {
        let table = ProcessTable::new(1, 0);
        let msg = crate::process::Message::new(ProcessId::new(1, 0, 0), rmpv::Value::Nil);
        assert!(table.send(ProcessId::new(1, 0, 999), msg).is_err());
    }

    #[test]
    fn process_count() {
        let table = ProcessTable::new(1, 0);
        assert_eq!(table.len(), 0);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn is_empty() {
        let table = ProcessTable::new(1, 0);
        assert!(table.is_empty());
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert!(!table.is_empty());
    }

    #[test]
    fn allocate_pid_uses_thread_id() {
        let table = ProcessTable::new(1, 5);
        let pid = table.allocate_pid();
        assert_eq!(pid.thread_id(), 5);
    }
}
