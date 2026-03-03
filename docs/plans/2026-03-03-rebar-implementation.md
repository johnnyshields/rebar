# Rebar Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a BEAM-inspired distributed actor runtime in Rust with OTP-style supervision, SWIM gossip clustering, CRDT-based global registry, pluggable transports (QUIC + TCP), and polyglot C-ABI FFI.

**Architecture:** Cargo workspace with a core `rebar` crate (the runtime library) and a `rebar-ffi` crate (C-ABI shared library). Core is structured as modules: `process`, `supervisor`, `transport`, `swim`, `registry`, `protocol`. Each layer builds on the previous — processes first, then supervision, then networking, then distribution.

**Tech Stack:** Rust (2024 edition), tokio (async runtime), dashmap (concurrent process table), rmp-serde (MessagePack), quinn (QUIC), rustls (TLS), uuid (registry tags), cargo-nextest (test runner)

**Design Doc:** `docs/plans/2026-03-03-rebar-design.md`

---

### Task 1: Project Scaffold

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/rebar/Cargo.toml`
- Create: `crates/rebar/src/lib.rs`
- Create: `crates/rebar-ffi/Cargo.toml`
- Create: `crates/rebar-ffi/src/lib.rs`

**Step 1: Create workspace Cargo.toml**

```toml
[workspace]
resolver = "2"
members = ["crates/rebar", "crates/rebar-ffi"]

[workspace.package]
edition = "2024"
license = "MIT"
repository = "https://github.com/alexandernicholson/rebar"
```

**Step 2: Create rebar core crate**

`crates/rebar/Cargo.toml`:
```toml
[package]
name = "rebar"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "A BEAM-inspired distributed actor runtime"

[dependencies]
tokio = { version = "1", features = ["full"] }
dashmap = "6"
rmp-serde = "1"
rmpv = "1"
serde = { version = "1", features = ["derive"] }
uuid = { version = "1", features = ["v4"] }
async-trait = "0.1"
thiserror = "2"
tracing = "0.1"
bytes = "1"

[dev-dependencies]
tokio-test = "0.4"
```

`crates/rebar/src/lib.rs`:
```rust
pub mod process;
```

Create `crates/rebar/src/process/mod.rs`:
```rust
mod types;
pub use types::*;
```

Create `crates/rebar/src/process/types.rs` as an empty file.

**Step 3: Create rebar-ffi crate (stub)**

`crates/rebar-ffi/Cargo.toml`:
```toml
[package]
name = "rebar-ffi"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "C-ABI FFI bindings for the Rebar actor runtime"

[lib]
crate-type = ["cdylib"]

[dependencies]
rebar = { path = "../rebar" }
```

`crates/rebar-ffi/src/lib.rs`:
```rust
// FFI bindings will be implemented after core runtime is complete.
```

**Step 4: Verify workspace builds**

Run: `cargo build`
Expected: Compiles with no errors

**Step 5: Commit**

```bash
git add Cargo.toml crates/
git commit -m "scaffold: cargo workspace with rebar and rebar-ffi crates"
```

---

### Task 2: ProcessId and Message Types

**Files:**
- Create: `crates/rebar/src/process/types.rs`
- Test: `crates/rebar/src/process/types.rs` (inline tests)

**Step 1: Write failing tests for ProcessId**

In `crates/rebar/src/process/types.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_id_equality() {
        let a = ProcessId::new(1, 42);
        let b = ProcessId::new(1, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn process_id_inequality_different_node() {
        let a = ProcessId::new(1, 42);
        let b = ProcessId::new(2, 42);
        assert_ne!(a, b);
    }

    #[test]
    fn process_id_inequality_different_local() {
        let a = ProcessId::new(1, 42);
        let b = ProcessId::new(1, 43);
        assert_ne!(a, b);
    }

    #[test]
    fn process_id_is_copy() {
        let a = ProcessId::new(1, 42);
        let b = a; // copy
        assert_eq!(a, b); // a is still valid
    }

    #[test]
    fn process_id_display() {
        let pid = ProcessId::new(1, 42);
        assert_eq!(format!("{}", pid), "<1.42>");
    }

    #[test]
    fn process_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ProcessId::new(1, 1));
        set.insert(ProcessId::new(1, 1));
        assert_eq!(set.len(), 1);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `ProcessId` not defined

**Step 3: Implement ProcessId**

In `crates/rebar/src/process/types.rs` (above the tests module):
```rust
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessId {
    node_id: u64,
    local_id: u64,
}

impl ProcessId {
    pub fn new(node_id: u64, local_id: u64) -> Self {
        Self { node_id, local_id }
    }

    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    pub fn local_id(&self) -> u64 {
        self.local_id
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}.{}>", self.node_id, self.local_id)
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 6 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add ProcessId type with copy, hash, display"
```

---

### Task 3: Message Type

**Files:**
- Modify: `crates/rebar/src/process/types.rs`

**Step 1: Write failing tests for Message**

Add to the tests module in `crates/rebar/src/process/types.rs`:
```rust
    #[test]
    fn message_creation() {
        let from = ProcessId::new(1, 1);
        let payload = rmpv::Value::String("hello".into());
        let msg = Message::new(from, payload.clone());
        assert_eq!(msg.from(), from);
        assert_eq!(*msg.payload(), payload);
        assert!(msg.timestamp() > 0);
    }

    #[test]
    fn message_with_map_payload() {
        let from = ProcessId::new(1, 1);
        let payload = rmpv::Value::Map(vec![
            (rmpv::Value::String("key".into()), rmpv::Value::Integer(42.into())),
        ]);
        let msg = Message::new(from, payload.clone());
        assert_eq!(*msg.payload(), payload);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `Message` not defined

**Step 3: Implement Message**

Add to `crates/rebar/src/process/types.rs` (after ProcessId, before tests):
```rust
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Message {
    from: ProcessId,
    payload: rmpv::Value,
    timestamp: u64,
}

impl Message {
    pub fn new(from: ProcessId, payload: rmpv::Value) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        Self { from, payload, timestamp }
    }

    pub fn from(&self) -> ProcessId {
        self.from
    }

    pub fn payload(&self) -> &rmpv::Value {
        &self.payload
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 8 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add Message type with msgpack payload"
```

---

### Task 4: Exit Reasons and Send Errors

**Files:**
- Modify: `crates/rebar/src/process/types.rs`

**Step 1: Write failing tests**

Add to tests module:
```rust
    #[test]
    fn exit_reason_normal() {
        let reason = ExitReason::Normal;
        assert!(reason.is_normal());
    }

    #[test]
    fn exit_reason_abnormal() {
        let reason = ExitReason::Abnormal("panicked".into());
        assert!(!reason.is_normal());
    }

    #[test]
    fn exit_reason_kill() {
        let reason = ExitReason::Kill;
        assert!(!reason.is_normal());
    }

    #[test]
    fn send_error_display() {
        let err = SendError::ProcessDead(ProcessId::new(1, 5));
        let msg = format!("{}", err);
        assert!(msg.contains("1") && msg.contains("5"));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `ExitReason` and `SendError` not defined

**Step 3: Implement ExitReason and SendError**

Add to `crates/rebar/src/process/types.rs`:
```rust
#[derive(Debug, Clone)]
pub enum ExitReason {
    Normal,
    Abnormal(String),
    Kill,
    LinkedExit(ProcessId, Box<ExitReason>),
}

impl ExitReason {
    pub fn is_normal(&self) -> bool {
        matches!(self, ExitReason::Normal)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("process dead: {0}")]
    ProcessDead(ProcessId),
    #[error("mailbox full for: {0}")]
    MailboxFull(ProcessId),
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 12 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add ExitReason and SendError types"
```

---

### Task 5: Process Table and Mailbox

**Files:**
- Create: `crates/rebar/src/process/table.rs`
- Create: `crates/rebar/src/process/mailbox.rs`
- Modify: `crates/rebar/src/process/mod.rs`

**Step 1: Write failing tests for Mailbox**

Create `crates/rebar/src/process/mailbox.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[tokio::test]
    async fn mailbox_send_receive() {
        let (tx, mut rx) = Mailbox::unbounded();
        let msg = Message::new(ProcessId::new(1, 1), rmpv::Value::Nil);
        tx.send(msg).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.from(), ProcessId::new(1, 1));
    }

    #[tokio::test]
    async fn mailbox_bounded() {
        let (tx, _rx) = Mailbox::bounded(1);
        let msg1 = Message::new(ProcessId::new(1, 1), rmpv::Value::Nil);
        let msg2 = Message::new(ProcessId::new(1, 2), rmpv::Value::Nil);
        tx.send(msg1).unwrap();
        // Second send should fail on a capacity-1 channel that hasn't been drained
        assert!(tx.try_send(msg2).is_err());
    }

    #[tokio::test]
    async fn mailbox_recv_timeout_expires() {
        let (_tx, mut rx) = Mailbox::unbounded();
        let result = rx.recv_timeout(std::time::Duration::from_millis(10)).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn mailbox_recv_timeout_receives() {
        let (tx, mut rx) = Mailbox::unbounded();
        let msg = Message::new(ProcessId::new(1, 1), rmpv::Value::Nil);
        tx.send(msg).unwrap();
        let result = rx.recv_timeout(std::time::Duration::from_millis(100)).await;
        assert!(result.is_some());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `Mailbox` not defined

**Step 3: Implement Mailbox**

Add above the tests module in `crates/rebar/src/process/mailbox.rs`:
```rust
use tokio::sync::mpsc;
use crate::process::{Message, SendError, ProcessId};

pub struct MailboxSender {
    tx: mpsc::UnboundedSender<Message>,
}

pub struct MailboxBoundedSender {
    tx: mpsc::Sender<Message>,
}

pub enum MailboxTx {
    Unbounded(mpsc::UnboundedSender<Message>),
    Bounded(mpsc::Sender<Message>),
}

pub struct MailboxRx {
    rx_unbounded: Option<mpsc::UnboundedReceiver<Message>>,
    rx_bounded: Option<mpsc::Receiver<Message>>,
}

pub struct Mailbox;

impl Mailbox {
    pub fn unbounded() -> (MailboxTx, MailboxRx) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            MailboxTx::Unbounded(tx),
            MailboxRx { rx_unbounded: Some(rx), rx_bounded: None },
        )
    }

    pub fn bounded(capacity: usize) -> (MailboxTx, MailboxRx) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            MailboxTx::Bounded(tx),
            MailboxRx { rx_unbounded: None, rx_bounded: Some(rx) },
        )
    }
}

impl MailboxTx {
    pub fn send(&self, msg: Message) -> Result<(), SendError> {
        match self {
            MailboxTx::Unbounded(tx) => tx.send(msg).map_err(|_| SendError::ProcessDead(ProcessId::new(0, 0))),
            MailboxTx::Bounded(tx) => tx.try_send(msg).map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => SendError::MailboxFull(ProcessId::new(0, 0)),
                mpsc::error::TrySendError::Closed(_) => SendError::ProcessDead(ProcessId::new(0, 0)),
            }),
        }
    }

    pub fn try_send(&self, msg: Message) -> Result<(), SendError> {
        self.send(msg)
    }
}

impl MailboxRx {
    pub async fn recv(&mut self) -> Option<Message> {
        if let Some(rx) = &mut self.rx_unbounded {
            rx.recv().await
        } else if let Some(rx) = &mut self.rx_bounded {
            rx.recv().await
        } else {
            None
        }
    }

    pub async fn recv_timeout(&mut self, duration: std::time::Duration) -> Option<Message> {
        tokio::time::timeout(duration, self.recv()).await.ok().flatten()
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 16 tests PASS

**Step 5: Write failing tests for ProcessTable**

Create `crates/rebar/src/process/table.rs`:
```rust
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
    fn insert_and_lookup() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        assert!(table.get(&pid).is_some());
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
    fn send_to_process() {
        let table = ProcessTable::new(1);
        let pid = table.allocate_pid();
        let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
        table.insert(pid, ProcessHandle::new(tx));
        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::Nil);
        assert!(table.send(pid, msg).is_ok());
    }

    #[test]
    fn send_to_dead_process() {
        let table = ProcessTable::new(1);
        let pid = ProcessId::new(1, 999);
        let msg = crate::process::Message::new(ProcessId::new(1, 0), rmpv::Value::Nil);
        assert!(table.send(pid, msg).is_err());
    }
}
```

**Step 6: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `ProcessTable` not defined

**Step 7: Implement ProcessTable**

Add above tests in `crates/rebar/src/process/table.rs`:
```rust
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::process::{ProcessId, Message, SendError};
use crate::process::mailbox::MailboxTx;

pub struct ProcessHandle {
    tx: MailboxTx,
}

impl ProcessHandle {
    pub fn new(tx: MailboxTx) -> Self {
        Self { tx }
    }

    pub fn send(&self, msg: Message) -> Result<(), SendError> {
        self.tx.send(msg)
    }
}

pub struct ProcessTable {
    node_id: u64,
    next_id: AtomicU64,
    processes: DashMap<ProcessId, ProcessHandle>,
}

impl ProcessTable {
    pub fn new(node_id: u64) -> Self {
        Self {
            node_id,
            next_id: AtomicU64::new(1),
            processes: DashMap::new(),
        }
    }

    pub fn allocate_pid(&self) -> ProcessId {
        let local_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        ProcessId::new(self.node_id, local_id)
    }

    pub fn insert(&self, pid: ProcessId, handle: ProcessHandle) {
        self.processes.insert(pid, handle);
    }

    pub fn get(&self, pid: &ProcessId) -> Option<dashmap::mapref::one::Ref<ProcessId, ProcessHandle>> {
        self.processes.get(pid)
    }

    pub fn remove(&self, pid: &ProcessId) -> Option<(ProcessId, ProcessHandle)> {
        self.processes.remove(pid)
    }

    pub fn send(&self, pid: ProcessId, msg: Message) -> Result<(), SendError> {
        match self.processes.get(&pid) {
            Some(handle) => handle.send(msg),
            None => Err(SendError::ProcessDead(pid)),
        }
    }
}
```

**Step 8: Update mod.rs**

Update `crates/rebar/src/process/mod.rs`:
```rust
mod types;
pub mod mailbox;
pub mod table;

pub use types::*;
```

**Step 9: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 21 tests PASS

**Step 10: Commit**

```bash
git add -A
git commit -m "feat: add Mailbox and ProcessTable"
```

---

### Task 6: Runtime and Process Spawning

**Files:**
- Create: `crates/rebar/src/runtime.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for Runtime**

Create `crates/rebar/src/runtime.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_process_returns_pid() {
        let rt = Runtime::new(1);
        let pid = rt.spawn(|_ctx| async {}).await;
        assert_eq!(pid.node_id(), 1);
        assert_eq!(pid.local_id(), 1);
    }

    #[tokio::test]
    async fn spawn_multiple_processes() {
        let rt = Runtime::new(1);
        let pid1 = rt.spawn(|_ctx| async {}).await;
        let pid2 = rt.spawn(|_ctx| async {}).await;
        assert_ne!(pid1, pid2);
    }

    #[tokio::test]
    async fn send_message_between_processes() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let receiver = rt.spawn(move |mut ctx| async move {
            let msg = ctx.recv().await.unwrap();
            let value = msg.payload().as_str().unwrap().to_string();
            done_tx.send(value).unwrap();
        }).await;

        rt.spawn(move |ctx| async move {
            ctx.send(receiver, rmpv::Value::String("hello".into())).await.unwrap();
        }).await;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            done_rx,
        ).await.unwrap().unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn self_pid_is_correct() {
        let rt = Runtime::new(1);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let pid = rt.spawn(move |ctx| async move {
            done_tx.send(ctx.self_pid()).unwrap();
        }).await;

        let reported_pid = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            done_rx,
        ).await.unwrap().unwrap();
        assert_eq!(pid, reported_pid);
    }

    #[tokio::test]
    async fn send_to_dead_process_returns_error() {
        let rt = Runtime::new(1);
        let pid = ProcessId::new(1, 999);
        let result = rt.send(pid, rmpv::Value::Nil).await;
        assert!(result.is_err());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `Runtime` not defined

**Step 3: Implement Runtime**

Add above the tests module in `crates/rebar/src/runtime.rs`:
```rust
use std::future::Future;
use std::sync::Arc;
use crate::process::{ProcessId, Message, SendError};
use crate::process::mailbox::{Mailbox, MailboxRx};
use crate::process::table::{ProcessTable, ProcessHandle};

pub struct ProcessContext {
    pid: ProcessId,
    rx: MailboxRx,
    table: Arc<ProcessTable>,
}

impl ProcessContext {
    pub fn self_pid(&self) -> ProcessId {
        self.pid
    }

    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    pub async fn recv_timeout(&mut self, duration: std::time::Duration) -> Option<Message> {
        self.rx.recv_timeout(duration).await
    }

    pub async fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        let msg = Message::new(self.pid, payload);
        self.table.send(dest, msg)
    }
}

pub struct Runtime {
    table: Arc<ProcessTable>,
}

impl Runtime {
    pub fn new(node_id: u64) -> Self {
        Self {
            table: Arc::new(ProcessTable::new(node_id)),
        }
    }

    pub async fn spawn<F, Fut>(&self, handler: F) -> ProcessId
    where
        F: FnOnce(ProcessContext) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pid = self.table.allocate_pid();
        let (tx, rx) = Mailbox::unbounded();

        let ctx = ProcessContext {
            pid,
            rx,
            table: Arc::clone(&self.table),
        };

        self.table.insert(pid, ProcessHandle::new(tx));

        tokio::spawn(async move {
            handler(ctx).await;
        });

        pid
    }

    pub async fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        let msg = Message::new(ProcessId::new(0, 0), payload);
        self.table.send(dest, msg)
    }
}
```

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod runtime;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 26 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add Runtime with process spawning and local messaging"
```

---

### Task 7: Links and Monitors

**Files:**
- Create: `crates/rebar/src/process/monitor.rs`
- Modify: `crates/rebar/src/process/table.rs`
- Modify: `crates/rebar/src/runtime.rs`
- Modify: `crates/rebar/src/process/mod.rs`

**Step 1: Write failing tests for monitors**

Create `crates/rebar/src/process/monitor.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[test]
    fn monitor_ref_unique() {
        let r1 = MonitorRef::new();
        let r2 = MonitorRef::new();
        assert_ne!(r1, r2);
    }

    #[test]
    fn monitor_set_add_and_iter() {
        let mut set = MonitorSet::new();
        let pid = ProcessId::new(1, 1);
        let mref = set.add_monitor(pid);
        let monitors: Vec<_> = set.monitors_for(pid).collect();
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0], mref);
    }

    #[test]
    fn monitor_set_remove() {
        let mut set = MonitorSet::new();
        let pid = ProcessId::new(1, 1);
        let mref = set.add_monitor(pid);
        set.remove_monitor(mref);
        assert_eq!(set.monitors_for(pid).count(), 0);
    }

    #[test]
    fn link_set_add_and_contains() {
        let mut set = LinkSet::new();
        let pid = ProcessId::new(1, 1);
        set.add_link(pid);
        assert!(set.is_linked(pid));
    }

    #[test]
    fn link_set_remove() {
        let mut set = LinkSet::new();
        let pid = ProcessId::new(1, 1);
        set.add_link(pid);
        set.remove_link(pid);
        assert!(!set.is_linked(pid));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `MonitorRef`, `MonitorSet`, `LinkSet` not defined

**Step 3: Implement monitor types**

Add above tests in `crates/rebar/src/process/monitor.rs`:
```rust
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::process::ProcessId;

static MONITOR_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorRef(u64);

impl MonitorRef {
    pub fn new() -> Self {
        Self(MONITOR_COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct MonitorSet {
    by_ref: HashMap<MonitorRef, ProcessId>,
    by_pid: HashMap<ProcessId, Vec<MonitorRef>>,
}

impl MonitorSet {
    pub fn new() -> Self {
        Self {
            by_ref: HashMap::new(),
            by_pid: HashMap::new(),
        }
    }

    pub fn add_monitor(&mut self, target: ProcessId) -> MonitorRef {
        let mref = MonitorRef::new();
        self.by_ref.insert(mref, target);
        self.by_pid.entry(target).or_default().push(mref);
        mref
    }

    pub fn remove_monitor(&mut self, mref: MonitorRef) {
        if let Some(target) = self.by_ref.remove(&mref) {
            if let Some(refs) = self.by_pid.get_mut(&target) {
                refs.retain(|r| *r != mref);
                if refs.is_empty() {
                    self.by_pid.remove(&target);
                }
            }
        }
    }

    pub fn monitors_for(&self, target: ProcessId) -> impl Iterator<Item = MonitorRef> + '_ {
        self.by_pid
            .get(&target)
            .into_iter()
            .flat_map(|refs| refs.iter().copied())
    }
}

pub struct LinkSet {
    links: HashSet<ProcessId>,
}

impl LinkSet {
    pub fn new() -> Self {
        Self { links: HashSet::new() }
    }

    pub fn add_link(&mut self, pid: ProcessId) {
        self.links.insert(pid);
    }

    pub fn remove_link(&mut self, pid: ProcessId) {
        self.links.remove(&pid);
    }

    pub fn is_linked(&self, pid: ProcessId) -> bool {
        self.links.contains(&pid)
    }

    pub fn linked_pids(&self) -> impl Iterator<Item = ProcessId> + '_ {
        self.links.iter().copied()
    }
}
```

Update `crates/rebar/src/process/mod.rs`:
```rust
mod types;
pub mod mailbox;
pub mod table;
pub mod monitor;

pub use types::*;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 31 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add MonitorRef, MonitorSet, LinkSet types"
```

---

### Task 8: Wire Protocol Frame Encoding/Decoding

**Files:**
- Create: `crates/rebar/src/protocol/mod.rs`
- Create: `crates/rebar/src/protocol/frame.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for frame codec**

Create `crates/rebar/src/protocol/frame.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_send_message() {
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::Map(vec![
                (rmpv::Value::String("dest".into()), rmpv::Value::Integer(42.into())),
                (rmpv::Value::String("from".into()), rmpv::Value::Integer(1.into())),
            ]),
            payload: rmpv::Value::String("hello".into()),
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.msg_type, MsgType::Send);
        assert_eq!(decoded.request_id, 0);
        assert_eq!(decoded.payload, rmpv::Value::String("hello".into()));
    }

    #[test]
    fn encode_decode_heartbeat() {
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Heartbeat,
            request_id: 0,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::Nil,
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded.msg_type, MsgType::Heartbeat);
    }

    #[test]
    fn encode_decode_with_request_id() {
        let frame = Frame {
            version: 1,
            msg_type: MsgType::NameLookup,
            request_id: 12345,
            header: rmpv::Value::String("worker".into()),
            payload: rmpv::Value::Nil,
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded.request_id, 12345);
    }

    #[test]
    fn all_msg_types_roundtrip() {
        let types = [
            MsgType::Send, MsgType::Monitor, MsgType::Demonitor,
            MsgType::Link, MsgType::Unlink, MsgType::Exit,
            MsgType::ProcessDown, MsgType::NameLookup,
            MsgType::NameRegister, MsgType::NameUnregister,
            MsgType::Heartbeat, MsgType::HeartbeatAck, MsgType::NodeInfo,
        ];
        for msg_type in types {
            let frame = Frame {
                version: 1,
                msg_type,
                request_id: 0,
                header: rmpv::Value::Nil,
                payload: rmpv::Value::Nil,
            };
            let bytes = frame.encode();
            let decoded = Frame::decode(&bytes).unwrap();
            assert_eq!(decoded.msg_type, msg_type, "failed for {:?}", msg_type);
        }
    }

    #[test]
    fn decode_invalid_bytes_returns_error() {
        let result = Frame::decode(&[0xFF, 0xFF]);
        assert!(result.is_err());
    }

    #[test]
    fn decode_truncated_returns_error() {
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 0,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::Nil,
        };
        let bytes = frame.encode();
        // truncate
        let result = Frame::decode(&bytes[..5]);
        assert!(result.is_err());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `Frame`, `MsgType` not defined

**Step 3: Implement frame codec**

Add above tests in `crates/rebar/src/protocol/frame.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Send = 0x01,
    Monitor = 0x02,
    Demonitor = 0x03,
    Link = 0x04,
    Unlink = 0x05,
    Exit = 0x06,
    ProcessDown = 0x07,
    NameLookup = 0x08,
    NameRegister = 0x09,
    NameUnregister = 0x0A,
    Heartbeat = 0x0B,
    HeartbeatAck = 0x0C,
    NodeInfo = 0x0D,
}

impl MsgType {
    fn from_u8(v: u8) -> Result<Self, FrameError> {
        match v {
            0x01 => Ok(Self::Send),
            0x02 => Ok(Self::Monitor),
            0x03 => Ok(Self::Demonitor),
            0x04 => Ok(Self::Link),
            0x05 => Ok(Self::Unlink),
            0x06 => Ok(Self::Exit),
            0x07 => Ok(Self::ProcessDown),
            0x08 => Ok(Self::NameLookup),
            0x09 => Ok(Self::NameRegister),
            0x0A => Ok(Self::NameUnregister),
            0x0B => Ok(Self::Heartbeat),
            0x0C => Ok(Self::HeartbeatAck),
            0x0D => Ok(Self::NodeInfo),
            _ => Err(FrameError::InvalidMsgType(v)),
        }
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("invalid message type: {0:#x}")]
    InvalidMsgType(u8),
    #[error("frame too short: need {need} bytes, got {got}")]
    TooShort { need: usize, got: usize },
    #[error("msgpack decode error: {0}")]
    MsgpackDecode(#[from] rmpv::decode::Error),
    #[error("msgpack encode error: {0}")]
    MsgpackEncode(String),
}

const HEADER_SIZE: usize = 1 + 1 + 8 + 4 + 4; // version + msg_type + request_id + header_len + payload_len

#[derive(Debug, Clone)]
pub struct Frame {
    pub version: u8,
    pub msg_type: MsgType,
    pub request_id: u64,
    pub header: rmpv::Value,
    pub payload: rmpv::Value,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut header_buf = Vec::new();
        rmpv::encode::write_value(&mut header_buf, &self.header).unwrap();
        let mut payload_buf = Vec::new();
        rmpv::encode::write_value(&mut payload_buf, &self.payload).unwrap();

        let mut buf = Vec::with_capacity(HEADER_SIZE + header_buf.len() + payload_buf.len());
        buf.push(self.version);
        buf.push(self.msg_type as u8);
        buf.extend_from_slice(&self.request_id.to_be_bytes());
        buf.extend_from_slice(&(header_buf.len() as u32).to_be_bytes());
        buf.extend_from_slice(&(payload_buf.len() as u32).to_be_bytes());
        buf.extend_from_slice(&header_buf);
        buf.extend_from_slice(&payload_buf);
        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self, FrameError> {
        if data.len() < HEADER_SIZE {
            return Err(FrameError::TooShort { need: HEADER_SIZE, got: data.len() });
        }

        let version = data[0];
        let msg_type = MsgType::from_u8(data[1])?;
        let request_id = u64::from_be_bytes(data[2..10].try_into().unwrap());
        let header_len = u32::from_be_bytes(data[10..14].try_into().unwrap()) as usize;
        let payload_len = u32::from_be_bytes(data[14..18].try_into().unwrap()) as usize;

        let total_needed = HEADER_SIZE + header_len + payload_len;
        if data.len() < total_needed {
            return Err(FrameError::TooShort { need: total_needed, got: data.len() });
        }

        let header_data = &data[HEADER_SIZE..HEADER_SIZE + header_len];
        let payload_data = &data[HEADER_SIZE + header_len..HEADER_SIZE + header_len + payload_len];

        let header = rmpv::decode::read_value(&mut &header_data[..])?;
        let payload = rmpv::decode::read_value(&mut &payload_data[..])?;

        Ok(Self { version, msg_type, request_id, header, payload })
    }
}
```

Create `crates/rebar/src/protocol/mod.rs`:
```rust
pub mod frame;

pub use frame::*;
```

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod protocol;
pub mod runtime;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 37 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add wire protocol frame encoding/decoding"
```

---

### Task 9: Transport Trait and TCP Implementation

**Files:**
- Create: `crates/rebar/src/transport/mod.rs`
- Create: `crates/rebar/src/transport/traits.rs`
- Create: `crates/rebar/src/transport/tcp.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for TCP transport**

Create `crates/rebar/src/transport/tcp.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, MsgType};

    #[tokio::test]
    async fn tcp_connect_and_send_frame() {
        let transport = TcpTransport::new();
        let listener = transport.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr();

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            conn.recv().await.unwrap()
        });

        let mut client = transport.connect(addr).await.unwrap();
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Heartbeat,
            request_id: 0,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::Nil,
        };
        client.send(&frame).await.unwrap();
        client.close().await.unwrap();

        let received = server.await.unwrap();
        assert_eq!(received.msg_type, MsgType::Heartbeat);
    }

    #[tokio::test]
    async fn tcp_bidirectional_messaging() {
        let transport = TcpTransport::new();
        let listener = transport.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr();

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let frame = conn.recv().await.unwrap();
            // echo back
            conn.send(&frame).await.unwrap();
        });

        let mut client = transport.connect(addr).await.unwrap();
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 42,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::String("ping".into()),
        };
        client.send(&frame).await.unwrap();
        let response = client.recv().await.unwrap();
        assert_eq!(response.request_id, 42);
        assert_eq!(response.payload, rmpv::Value::String("ping".into()));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_multiple_frames() {
        let transport = TcpTransport::new();
        let listener = transport.listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr();

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let mut frames = Vec::new();
            for _ in 0..3 {
                frames.push(conn.recv().await.unwrap());
            }
            frames
        });

        let mut client = transport.connect(addr).await.unwrap();
        for i in 0..3u64 {
            let frame = Frame {
                version: 1,
                msg_type: MsgType::Send,
                request_id: i,
                header: rmpv::Value::Nil,
                payload: rmpv::Value::Integer(i.into()),
            };
            client.send(&frame).await.unwrap();
        }

        let frames = server.await.unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].request_id, 0);
        assert_eq!(frames[2].request_id, 2);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `TcpTransport` not defined

**Step 3: Implement transport trait**

Create `crates/rebar/src/transport/traits.rs`:
```rust
use std::net::SocketAddr;
use async_trait::async_trait;
use crate::protocol::Frame;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("frame error: {0}")]
    Frame(#[from] crate::protocol::FrameError),
}

#[async_trait]
pub trait TransportConnection: Send + Sync {
    async fn send(&mut self, frame: &Frame) -> Result<(), TransportError>;
    async fn recv(&mut self) -> Result<Frame, TransportError>;
    async fn close(&mut self) -> Result<(), TransportError>;
}

#[async_trait]
pub trait TransportListener: Send + Sync {
    type Connection: TransportConnection;
    fn local_addr(&self) -> SocketAddr;
    async fn accept(&self) -> Result<Self::Connection, TransportError>;
}
```

**Step 4: Implement TCP transport**

Add above tests in `crates/rebar/src/transport/tcp.rs`:
```rust
use std::net::SocketAddr;
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use crate::protocol::Frame;
use crate::transport::traits::{TransportConnection, TransportError, TransportListener};

pub struct TcpTransport;

impl TcpTransport {
    pub fn new() -> Self {
        Self
    }

    pub async fn listen(&self, addr: SocketAddr) -> Result<TcpTransportListener, TransportError> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        Ok(TcpTransportListener { listener, local_addr })
    }

    pub async fn connect(&self, addr: SocketAddr) -> Result<TcpConnection, TransportError> {
        let stream = TcpStream::connect(addr).await?;
        Ok(TcpConnection { stream })
    }
}

pub struct TcpTransportListener {
    listener: TcpListener,
    local_addr: SocketAddr,
}

#[async_trait]
impl TransportListener for TcpTransportListener {
    type Connection = TcpConnection;

    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    async fn accept(&self) -> Result<TcpConnection, TransportError> {
        let (stream, _) = self.listener.accept().await?;
        Ok(TcpConnection { stream })
    }
}

pub struct TcpConnection {
    stream: TcpStream,
}

#[async_trait]
impl TransportConnection for TcpConnection {
    async fn send(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let data = frame.encode();
        let len = data.len() as u32;
        self.stream.write_all(&len.to_be_bytes()).await?;
        self.stream.write_all(&data).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Frame, TransportError> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                TransportError::ConnectionClosed
            } else {
                TransportError::Io(e)
            }
        })?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut data = vec![0u8; len];
        self.stream.read_exact(&mut data).await?;
        Ok(Frame::decode(&data)?)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.stream.shutdown().await?;
        Ok(())
    }
}
```

Create `crates/rebar/src/transport/mod.rs`:
```rust
pub mod traits;
pub mod tcp;

pub use traits::*;
pub use tcp::TcpTransport;
```

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod protocol;
pub mod runtime;
pub mod transport;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 5: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 40 tests PASS

**Step 6: Commit**

```bash
git add -A
git commit -m "feat: add Transport trait and TCP implementation"
```

---

### Task 10: Supervisor Implementation

**Files:**
- Create: `crates/rebar/src/supervisor/mod.rs`
- Create: `crates/rebar/src/supervisor/spec.rs`
- Create: `crates/rebar/src/supervisor/engine.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for supervisor spec types**

Create `crates/rebar/src/supervisor/spec.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_strategy_variants() {
        let _ = RestartStrategy::OneForOne;
        let _ = RestartStrategy::OneForAll;
        let _ = RestartStrategy::RestForOne;
    }

    #[test]
    fn restart_type_variants() {
        assert!(RestartType::Permanent.should_restart(&ExitReason::Normal));
        assert!(RestartType::Permanent.should_restart(&ExitReason::Abnormal("err".into())));
        assert!(!RestartType::Transient.should_restart(&ExitReason::Normal));
        assert!(RestartType::Transient.should_restart(&ExitReason::Abnormal("err".into())));
        assert!(!RestartType::Temporary.should_restart(&ExitReason::Normal));
        assert!(!RestartType::Temporary.should_restart(&ExitReason::Abnormal("err".into())));
    }

    #[test]
    fn shutdown_strategy_default() {
        let strategy = ShutdownStrategy::Timeout(std::time::Duration::from_secs(5));
        match strategy {
            ShutdownStrategy::Timeout(d) => assert_eq!(d.as_secs(), 5),
            _ => panic!("expected Timeout"),
        }
    }

    #[test]
    fn supervisor_spec_builder() {
        let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
            .max_restarts(3)
            .max_seconds(5);
        assert_eq!(spec.max_restarts, 3);
        assert_eq!(spec.max_seconds, 5);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — supervisor types not defined

**Step 3: Implement supervisor spec types**

Add above tests in `crates/rebar/src/supervisor/spec.rs`:
```rust
use std::time::Duration;
use crate::process::ExitReason;

#[derive(Debug, Clone, Copy)]
pub enum RestartStrategy {
    OneForOne,
    OneForAll,
    RestForOne,
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
}

pub struct SupervisorSpec {
    pub strategy: RestartStrategy,
    pub max_restarts: u32,
    pub max_seconds: u32,
    pub children: Vec<ChildSpec>,
}

impl SupervisorSpec {
    pub fn new(strategy: RestartStrategy) -> Self {
        Self {
            strategy,
            max_restarts: 3,
            max_seconds: 5,
            children: Vec::new(),
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
}

pub struct ChildSpec {
    pub id: String,
    pub restart: RestartType,
    pub shutdown: ShutdownStrategy,
}

impl ChildSpec {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            restart: RestartType::Permanent,
            shutdown: ShutdownStrategy::Timeout(Duration::from_secs(5)),
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
}
```

Create `crates/rebar/src/supervisor/mod.rs`:
```rust
pub mod spec;

pub use spec::*;
```

Create `crates/rebar/src/supervisor/engine.rs` as an empty file (placeholder for Task 11).

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod protocol;
pub mod runtime;
pub mod supervisor;
pub mod transport;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 44 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add supervisor spec types (strategies, child specs)"
```

---

### Task 11: SWIM Protocol Core

**Files:**
- Create: `crates/rebar/src/swim/mod.rs`
- Create: `crates/rebar/src/swim/member.rs`
- Create: `crates/rebar/src/swim/protocol.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for SWIM membership**

Create `crates/rebar/src/swim/member.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_member_is_alive() {
        let member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        assert_eq!(member.state, NodeState::Alive);
        assert_eq!(member.incarnation, 0);
    }

    #[test]
    fn suspect_member() {
        let mut member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        member.suspect(1);
        assert_eq!(member.state, NodeState::Suspect);
    }

    #[test]
    fn refute_suspicion_with_higher_incarnation() {
        let mut member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        member.suspect(0);
        assert_eq!(member.state, NodeState::Suspect);
        member.alive(1);
        assert_eq!(member.state, NodeState::Alive);
        assert_eq!(member.incarnation, 1);
    }

    #[test]
    fn ignore_stale_alive() {
        let mut member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        member.suspect(5);
        member.alive(3); // stale
        assert_eq!(member.state, NodeState::Suspect);
    }

    #[test]
    fn declare_dead() {
        let mut member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        member.dead();
        assert_eq!(member.state, NodeState::Dead);
    }

    #[test]
    fn membership_list_add_and_get() {
        let mut list = MembershipList::new();
        let member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        list.add(member);
        assert!(list.get(1).is_some());
        assert_eq!(list.alive_count(), 1);
    }

    #[test]
    fn membership_list_remove_dead() {
        let mut list = MembershipList::new();
        let mut member = Member::new(1, "127.0.0.1:4000".parse().unwrap());
        list.add(member);
        list.mark_dead(1);
        list.remove_dead();
        assert_eq!(list.alive_count(), 0);
    }

    #[test]
    fn membership_list_random_alive_member() {
        let mut list = MembershipList::new();
        list.add(Member::new(1, "127.0.0.1:4001".parse().unwrap()));
        list.add(Member::new(2, "127.0.0.1:4002".parse().unwrap()));
        list.add(Member::new(3, "127.0.0.1:4003".parse().unwrap()));
        let pick = list.random_alive_member(0); // exclude self (node 0)
        assert!(pick.is_some());
        let picked = pick.unwrap();
        assert!(picked.node_id == 1 || picked.node_id == 2 || picked.node_id == 3);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — SWIM types not defined

**Step 3: Implement SWIM membership**

Add above tests in `crates/rebar/src/swim/member.rs`:
```rust
use std::net::SocketAddr;
use std::collections::HashMap;
use rand::seq::IteratorRandom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Alive,
    Suspect,
    Dead,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub node_id: u64,
    pub addr: SocketAddr,
    pub state: NodeState,
    pub incarnation: u64,
}

impl Member {
    pub fn new(node_id: u64, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            state: NodeState::Alive,
            incarnation: 0,
        }
    }

    pub fn suspect(&mut self, incarnation: u64) {
        if incarnation >= self.incarnation && self.state != NodeState::Dead {
            self.state = NodeState::Suspect;
            self.incarnation = incarnation;
        }
    }

    pub fn alive(&mut self, incarnation: u64) {
        if incarnation > self.incarnation {
            self.state = NodeState::Alive;
            self.incarnation = incarnation;
        }
    }

    pub fn dead(&mut self) {
        self.state = NodeState::Dead;
    }
}

pub struct MembershipList {
    members: HashMap<u64, Member>,
}

impl MembershipList {
    pub fn new() -> Self {
        Self { members: HashMap::new() }
    }

    pub fn add(&mut self, member: Member) {
        self.members.insert(member.node_id, member);
    }

    pub fn get(&self, node_id: u64) -> Option<&Member> {
        self.members.get(&node_id)
    }

    pub fn get_mut(&mut self, node_id: u64) -> Option<&mut Member> {
        self.members.get_mut(&node_id)
    }

    pub fn mark_dead(&mut self, node_id: u64) {
        if let Some(member) = self.members.get_mut(&node_id) {
            member.dead();
        }
    }

    pub fn remove_dead(&mut self) {
        self.members.retain(|_, m| m.state != NodeState::Dead);
    }

    pub fn alive_count(&self) -> usize {
        self.members.values().filter(|m| m.state == NodeState::Alive).count()
    }

    pub fn random_alive_member(&self, exclude: u64) -> Option<Member> {
        let mut rng = rand::rng();
        self.members
            .values()
            .filter(|m| m.state == NodeState::Alive && m.node_id != exclude)
            .choose(&mut rng)
            .cloned()
    }

    pub fn all_members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }
}
```

Add `rand` to dependencies in `crates/rebar/Cargo.toml`:
```toml
rand = "0.9"
```

Create `crates/rebar/src/swim/mod.rs`:
```rust
pub mod member;

pub use member::*;
```

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod protocol;
pub mod runtime;
pub mod supervisor;
pub mod swim;
pub mod transport;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 52 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add SWIM membership list with state transitions"
```

---

### Task 12: SWIM Dissemination (Gossip Updates)

**Files:**
- Create: `crates/rebar/src/swim/gossip.rs`
- Modify: `crates/rebar/src/swim/mod.rs`

**Step 1: Write failing tests for gossip updates**

Create `crates/rebar/src/swim/gossip.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_update_to_queue() {
        let mut queue = GossipQueue::new(5);
        queue.push(GossipUpdate::Alive { node_id: 1, incarnation: 0, addr: "127.0.0.1:4000".parse().unwrap() });
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn drain_returns_bounded_updates() {
        let mut queue = GossipQueue::new(2);
        queue.push(GossipUpdate::Alive { node_id: 1, incarnation: 0, addr: "127.0.0.1:4001".parse().unwrap() });
        queue.push(GossipUpdate::Alive { node_id: 2, incarnation: 0, addr: "127.0.0.1:4002".parse().unwrap() });
        queue.push(GossipUpdate::Suspect { node_id: 3, incarnation: 1 });
        let batch = queue.drain(2);
        assert_eq!(batch.len(), 2);
        assert_eq!(queue.len(), 1); // one remaining
    }

    #[test]
    fn gossip_update_serialization() {
        let update = GossipUpdate::Alive { node_id: 42, incarnation: 5, addr: "10.0.0.1:4000".parse().unwrap() };
        let bytes = update.encode();
        let decoded = GossipUpdate::decode(&bytes).unwrap();
        match decoded {
            GossipUpdate::Alive { node_id, incarnation, .. } => {
                assert_eq!(node_id, 42);
                assert_eq!(incarnation, 5);
            }
            _ => panic!("expected Alive"),
        }
    }

    #[test]
    fn gossip_update_dead_serialization() {
        let update = GossipUpdate::Dead { node_id: 7 };
        let bytes = update.encode();
        let decoded = GossipUpdate::decode(&bytes).unwrap();
        match decoded {
            GossipUpdate::Dead { node_id } => assert_eq!(node_id, 7),
            _ => panic!("expected Dead"),
        }
    }

    #[test]
    fn gossip_update_leave_serialization() {
        let update = GossipUpdate::Leave { node_id: 3 };
        let bytes = update.encode();
        let decoded = GossipUpdate::decode(&bytes).unwrap();
        match decoded {
            GossipUpdate::Leave { node_id } => assert_eq!(node_id, 3),
            _ => panic!("expected Leave"),
        }
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `GossipQueue`, `GossipUpdate` not defined

**Step 3: Implement gossip types**

Add above tests in `crates/rebar/src/swim/gossip.rs`:
```rust
use std::collections::VecDeque;
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub enum GossipUpdate {
    Alive { node_id: u64, incarnation: u64, addr: SocketAddr },
    Suspect { node_id: u64, incarnation: u64 },
    Dead { node_id: u64 },
    Leave { node_id: u64 },
}

impl GossipUpdate {
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).unwrap()
    }

    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

impl serde::Serialize for GossipUpdate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(None)?;
        match self {
            GossipUpdate::Alive { node_id, incarnation, addr } => {
                map.serialize_entry("type", "alive")?;
                map.serialize_entry("node_id", node_id)?;
                map.serialize_entry("incarnation", incarnation)?;
                map.serialize_entry("addr", &addr.to_string())?;
            }
            GossipUpdate::Suspect { node_id, incarnation } => {
                map.serialize_entry("type", "suspect")?;
                map.serialize_entry("node_id", node_id)?;
                map.serialize_entry("incarnation", incarnation)?;
            }
            GossipUpdate::Dead { node_id } => {
                map.serialize_entry("type", "dead")?;
                map.serialize_entry("node_id", node_id)?;
            }
            GossipUpdate::Leave { node_id } => {
                map.serialize_entry("type", "leave")?;
                map.serialize_entry("node_id", node_id)?;
            }
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for GossipUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use std::collections::HashMap;
        let map: HashMap<String, rmpv::Value> = serde::Deserialize::deserialize(d)?;
        let typ = map.get("type").and_then(|v| v.as_str()).ok_or_else(|| serde::de::Error::missing_field("type"))?;
        match typ {
            "alive" => {
                let node_id = map.get("node_id").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("node_id"))?;
                let incarnation = map.get("incarnation").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("incarnation"))?;
                let addr_str = map.get("addr").and_then(|v| v.as_str()).ok_or_else(|| serde::de::Error::missing_field("addr"))?;
                let addr: SocketAddr = addr_str.parse().map_err(serde::de::Error::custom)?;
                Ok(GossipUpdate::Alive { node_id, incarnation, addr })
            }
            "suspect" => {
                let node_id = map.get("node_id").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("node_id"))?;
                let incarnation = map.get("incarnation").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("incarnation"))?;
                Ok(GossipUpdate::Suspect { node_id, incarnation })
            }
            "dead" => {
                let node_id = map.get("node_id").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("node_id"))?;
                Ok(GossipUpdate::Dead { node_id })
            }
            "leave" => {
                let node_id = map.get("node_id").and_then(|v| v.as_u64()).ok_or_else(|| serde::de::Error::missing_field("node_id"))?;
                Ok(GossipUpdate::Leave { node_id })
            }
            other => Err(serde::de::Error::unknown_variant(other, &["alive", "suspect", "dead", "leave"])),
        }
    }
}

pub struct GossipQueue {
    queue: VecDeque<GossipUpdate>,
    max_per_message: usize,
}

impl GossipQueue {
    pub fn new(max_per_message: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_per_message,
        }
    }

    pub fn push(&mut self, update: GossipUpdate) {
        self.queue.push_back(update);
    }

    pub fn drain(&mut self, count: usize) -> Vec<GossipUpdate> {
        let n = count.min(self.queue.len());
        self.queue.drain(..n).collect()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
```

Update `crates/rebar/src/swim/mod.rs`:
```rust
pub mod member;
pub mod gossip;

pub use member::*;
pub use gossip::*;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 57 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add SWIM gossip update queue and serialization"
```

---

### Task 13: Global Registry (CRDT OR-Set)

**Files:**
- Create: `crates/rebar/src/registry/mod.rs`
- Create: `crates/rebar/src/registry/orset.rs`
- Modify: `crates/rebar/src/lib.rs`

**Step 1: Write failing tests for OR-Set registry**

Create `crates/rebar/src/registry/orset.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessId;

    #[test]
    fn register_and_lookup() {
        let mut reg = Registry::new();
        let pid = ProcessId::new(1, 1);
        reg.register("worker", pid);
        assert_eq!(reg.whereis("worker"), Some(pid));
    }

    #[test]
    fn unregister() {
        let mut reg = Registry::new();
        let pid = ProcessId::new(1, 1);
        reg.register("worker", pid);
        reg.unregister("worker", pid);
        assert_eq!(reg.whereis("worker"), None);
    }

    #[test]
    fn last_writer_wins_conflict() {
        let mut reg = Registry::new();
        let pid1 = ProcessId::new(1, 1);
        let pid2 = ProcessId::new(2, 2);
        reg.register("leader", pid1);
        reg.register("leader", pid2);
        // last registration wins
        assert_eq!(reg.whereis("leader"), Some(pid2));
    }

    #[test]
    fn remove_by_pid_cleans_all_names() {
        let mut reg = Registry::new();
        let pid = ProcessId::new(1, 1);
        reg.register("name1", pid);
        reg.register("name2", pid);
        reg.remove_by_pid(pid);
        assert_eq!(reg.whereis("name1"), None);
        assert_eq!(reg.whereis("name2"), None);
    }

    #[test]
    fn remove_by_node_cleans_all() {
        let mut reg = Registry::new();
        reg.register("a", ProcessId::new(1, 1));
        reg.register("b", ProcessId::new(1, 2));
        reg.register("c", ProcessId::new(2, 1));
        reg.remove_by_node(1);
        assert_eq!(reg.whereis("a"), None);
        assert_eq!(reg.whereis("b"), None);
        assert_eq!(reg.whereis("c"), Some(ProcessId::new(2, 1)));
    }

    #[test]
    fn registered_returns_all() {
        let mut reg = Registry::new();
        reg.register("a", ProcessId::new(1, 1));
        reg.register("b", ProcessId::new(1, 2));
        let all = reg.registered();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn merge_delta_add() {
        let mut reg = Registry::new();
        let delta = RegistryDelta::Add {
            name: "service".into(),
            pid: ProcessId::new(2, 1),
            tag: uuid::Uuid::new_v4(),
            timestamp: 100,
        };
        reg.merge_delta(delta);
        assert_eq!(reg.whereis("service"), Some(ProcessId::new(2, 1)));
    }

    #[test]
    fn merge_delta_remove() {
        let mut reg = Registry::new();
        let pid = ProcessId::new(1, 1);
        let tag = reg.register("svc", pid);
        let delta = RegistryDelta::Remove { tag };
        reg.merge_delta(delta);
        assert_eq!(reg.whereis("svc"), None);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar`
Expected: FAIL — `Registry`, `RegistryDelta` not defined

**Step 3: Implement OR-Set registry**

Add above tests in `crates/rebar/src/registry/orset.rs`:
```rust
use std::collections::HashMap;
use std::collections::HashSet;
use uuid::Uuid;
use crate::process::ProcessId;

#[derive(Debug, Clone)]
pub struct RegistryEntry {
    pub name: String,
    pub pid: ProcessId,
    pub tag: Uuid,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum RegistryDelta {
    Add { name: String, pid: ProcessId, tag: Uuid, timestamp: u64 },
    Remove { tag: Uuid },
}

pub struct Registry {
    entries: HashMap<String, Vec<RegistryEntry>>,
    tombstones: HashSet<Uuid>,
    logical_clock: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            tombstones: HashSet::new(),
            logical_clock: 0,
        }
    }

    pub fn register(&mut self, name: &str, pid: ProcessId) -> Uuid {
        self.logical_clock += 1;
        let tag = Uuid::new_v4();
        let entry = RegistryEntry {
            name: name.to_string(),
            pid,
            tag,
            timestamp: self.logical_clock,
        };
        self.entries.entry(name.to_string()).or_default().push(entry);
        tag
    }

    pub fn unregister(&mut self, name: &str, pid: ProcessId) {
        if let Some(entries) = self.entries.get_mut(name) {
            let tags: Vec<Uuid> = entries.iter()
                .filter(|e| e.pid == pid)
                .map(|e| e.tag)
                .collect();
            for tag in &tags {
                self.tombstones.insert(*tag);
            }
            entries.retain(|e| !tags.contains(&e.tag));
            if entries.is_empty() {
                self.entries.remove(name);
            }
        }
    }

    pub fn whereis(&self, name: &str) -> Option<ProcessId> {
        self.entries.get(name).and_then(|entries| {
            entries.iter()
                .filter(|e| !self.tombstones.contains(&e.tag))
                .max_by_key(|e| e.timestamp)
                .map(|e| e.pid)
        })
    }

    pub fn remove_by_pid(&mut self, pid: ProcessId) {
        let names: Vec<String> = self.entries.keys().cloned().collect();
        for name in names {
            if let Some(entries) = self.entries.get_mut(&name) {
                let tags: Vec<Uuid> = entries.iter()
                    .filter(|e| e.pid == pid)
                    .map(|e| e.tag)
                    .collect();
                for tag in &tags {
                    self.tombstones.insert(*tag);
                }
                entries.retain(|e| !tags.contains(&e.tag));
                if entries.is_empty() {
                    self.entries.remove(&name);
                }
            }
        }
    }

    pub fn remove_by_node(&mut self, node_id: u64) {
        let names: Vec<String> = self.entries.keys().cloned().collect();
        for name in names {
            if let Some(entries) = self.entries.get_mut(&name) {
                let tags: Vec<Uuid> = entries.iter()
                    .filter(|e| e.pid.node_id() == node_id)
                    .map(|e| e.tag)
                    .collect();
                for tag in &tags {
                    self.tombstones.insert(*tag);
                }
                entries.retain(|e| !tags.contains(&e.tag));
                if entries.is_empty() {
                    self.entries.remove(&name);
                }
            }
        }
    }

    pub fn registered(&self) -> Vec<(String, ProcessId)> {
        let mut result = Vec::new();
        for (name, entries) in &self.entries {
            if let Some(entry) = entries.iter()
                .filter(|e| !self.tombstones.contains(&e.tag))
                .max_by_key(|e| e.timestamp)
            {
                result.push((name.clone(), entry.pid));
            }
        }
        result
    }

    pub fn merge_delta(&mut self, delta: RegistryDelta) {
        match delta {
            RegistryDelta::Add { name, pid, tag, timestamp } => {
                if !self.tombstones.contains(&tag) {
                    let entry = RegistryEntry { name: name.clone(), pid, tag, timestamp };
                    self.entries.entry(name).or_default().push(entry);
                    self.logical_clock = self.logical_clock.max(timestamp);
                }
            }
            RegistryDelta::Remove { tag } => {
                self.tombstones.insert(tag);
                for entries in self.entries.values_mut() {
                    entries.retain(|e| e.tag != tag);
                }
                self.entries.retain(|_, v| !v.is_empty());
            }
        }
    }
}
```

Create `crates/rebar/src/registry/mod.rs`:
```rust
pub mod orset;

pub use orset::*;
```

Update `crates/rebar/src/lib.rs`:
```rust
pub mod process;
pub mod protocol;
pub mod registry;
pub mod runtime;
pub mod supervisor;
pub mod swim;
pub mod transport;

pub use process::ProcessId;
pub use runtime::Runtime;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar`
Expected: All 66 tests PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat: add CRDT OR-Set global registry"
```

---

### Task 14: C-ABI FFI Bindings

**Files:**
- Modify: `crates/rebar-ffi/src/lib.rs`
- Modify: `crates/rebar-ffi/Cargo.toml`

**Step 1: Write failing tests for FFI types**

In `crates/rebar-ffi/src/lib.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_msg_create_and_read() {
        let data = b"hello";
        let msg = unsafe { rebar_msg_new(data.as_ptr(), data.len()) };
        assert!(!msg.is_null());
        let ptr = unsafe { rebar_msg_data(msg) };
        let len = unsafe { rebar_msg_len(msg) };
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert_eq!(slice, b"hello");
        unsafe { rebar_msg_free(msg) };
    }

    #[test]
    fn ffi_runtime_create_destroy() {
        let rt = unsafe { rebar_runtime_new(1) };
        assert!(!rt.is_null());
        unsafe { rebar_runtime_destroy(rt) };
    }

    #[test]
    fn ffi_pid_components() {
        let pid = RebarPid { node_id: 1, local_id: 42 };
        assert_eq!(pid.node_id, 1);
        assert_eq!(pid.local_id, 42);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p rebar-ffi`
Expected: FAIL — FFI functions not defined

**Step 3: Implement core FFI bindings**

In `crates/rebar-ffi/src/lib.rs` (above tests):
```rust
use std::ffi::c_void;
use std::ptr;

#[repr(C)]
pub struct RebarPid {
    pub node_id: u64,
    pub local_id: u64,
}

pub struct RebarMsg {
    data: Vec<u8>,
}

pub struct RebarRuntime {
    inner: rebar::Runtime,
    tokio_rt: tokio::runtime::Runtime,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_msg_new(data: *const u8, len: usize) -> *mut RebarMsg {
    let slice = unsafe { std::slice::from_raw_parts(data, len) };
    let msg = Box::new(RebarMsg { data: slice.to_vec() });
    Box::into_raw(msg)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_msg_data(msg: *const RebarMsg) -> *const u8 {
    let msg = unsafe { &*msg };
    msg.data.as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_msg_len(msg: *const RebarMsg) -> usize {
    let msg = unsafe { &*msg };
    msg.data.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_msg_free(msg: *mut RebarMsg) {
    if !msg.is_null() {
        unsafe { drop(Box::from_raw(msg)) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_runtime_new(node_id: u64) -> *mut RebarRuntime {
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let inner = rebar::Runtime::new(node_id);
    let rt = Box::new(RebarRuntime { inner, tokio_rt });
    Box::into_raw(rt)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rebar_runtime_destroy(rt: *mut RebarRuntime) {
    if !rt.is_null() {
        unsafe { drop(Box::from_raw(rt)) };
    }
}
```

Update `crates/rebar-ffi/Cargo.toml`:
```toml
[package]
name = "rebar-ffi"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "C-ABI FFI bindings for the Rebar actor runtime"

[lib]
crate-type = ["cdylib", "lib"]

[dependencies]
rebar = { path = "../rebar" }
tokio = { version = "1", features = ["full"] }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p rebar-ffi`
Expected: All 3 FFI tests PASS

**Step 5: Verify cdylib builds**

Run: `cargo build -p rebar-ffi`
Expected: Produces `target/debug/librebar_ffi.so`

**Step 6: Commit**

```bash
git add -A
git commit -m "feat: add C-ABI FFI bindings for runtime, messages, PIDs"
```

---

### Task 15: Integration Test — End-to-End Local Messaging

**Files:**
- Create: `crates/rebar/tests/integration.rs`

**Step 1: Write integration test**

```rust
use rebar::{Runtime, ProcessId};

#[tokio::test]
async fn ping_pong_between_processes() {
    let rt = Runtime::new(1);
    let (tx, rx) = tokio::sync::oneshot::channel();

    let pong = rt.spawn(|mut ctx| async move {
        let msg = ctx.recv().await.unwrap();
        let from = msg.from();
        ctx.send(from, rmpv::Value::String("pong".into())).await.unwrap();
    }).await;

    rt.spawn(move |mut ctx| async move {
        ctx.send(pong, rmpv::Value::String("ping".into())).await.unwrap();
        let reply = ctx.recv().await.unwrap();
        tx.send(reply.payload().as_str().unwrap().to_string()).unwrap();
    }).await;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rx,
    ).await.unwrap().unwrap();
    assert_eq!(result, "pong");
}

#[tokio::test]
async fn fan_out_to_multiple_processes() {
    let rt = Runtime::new(1);
    let (tx, rx) = tokio::sync::mpsc::channel(10);

    let mut workers = Vec::new();
    for _ in 0..5 {
        let tx = tx.clone();
        let pid = rt.spawn(move |mut ctx| async move {
            let msg = ctx.recv().await.unwrap();
            let val = msg.payload().as_u64().unwrap();
            tx.send(val * 2).await.unwrap();
        }).await;
        workers.push(pid);
    }
    drop(tx);

    rt.spawn(move |ctx| async move {
        for (i, pid) in workers.iter().enumerate() {
            ctx.send(*pid, rmpv::Value::Integer((i as u64).into())).await.unwrap();
        }
    }).await;

    let mut results = Vec::new();
    let mut rx = rx;
    while let Ok(Some(val)) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rx.recv(),
    ).await {
        results.push(val);
    }
    results.sort();
    assert_eq!(results, vec![0, 2, 4, 6, 8]);
}
```

**Step 2: Run integration tests**

Run: `cargo test -p rebar --test integration`
Expected: All 2 integration tests PASS

**Step 3: Commit**

```bash
git add -A
git commit -m "test: add end-to-end integration tests for local messaging"
```

---

## Summary

| Task | Component | Tests |
|------|-----------|-------|
| 1 | Project scaffold | build check |
| 2 | ProcessId | 6 |
| 3 | Message | 2 |
| 4 | ExitReason, SendError | 4 |
| 5 | Mailbox, ProcessTable | 9 |
| 6 | Runtime, process spawning | 5 |
| 7 | Links, monitors | 5 |
| 8 | Wire protocol frames | 6 |
| 9 | Transport trait + TCP | 3 |
| 10 | Supervisor spec types | 4 |
| 11 | SWIM membership | 8 |
| 12 | SWIM gossip dissemination | 5 |
| 13 | Global registry (CRDT) | 9 |
| 14 | C-ABI FFI bindings | 3 |
| 15 | Integration tests | 2 |
| **Total** | | **~71** |

Each task follows TDD: write failing test, run to confirm failure, implement, run to confirm pass, commit.
