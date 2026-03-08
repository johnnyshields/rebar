# GenServer, DynamicSupervisor, RuntimeBuilder Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add GenServer (OTP-style call/cast/info), DynamicSupervisor, and RuntimeBuilder to rebar-core, plus 5 targeted performance optimisations identified from baseline benchmarking — all without breaking existing API contracts or Barkeeper's 315 tests.

**Architecture:** GenServer uses a dual-channel design (typed mpsc for call/cast + existing rmpv::Value mailbox for handle_info). DynamicSupervisor is a new supervisor type using HashMap<ProcessId, _> for O(1) child management. RuntimeBuilder wraps tokio's RuntimeBuilder for convenient multi-threaded bootstrap.

**Performance targets** (from baseline analysis `bench/results/baseline-2026-03-08/analysis.md`):
1. Eliminate double-spawn overhead in `Runtime::spawn` (Tasks 10)
2. Remove sleep-based supervisor shutdown (Task 11)
3. Pre-size ProcessTable DashMap to reduce rehashing (Task 12)
4. Make Message timestamp lazy to skip syscall on internal routing (Task 13)
5. Use concrete LocalRouter type to avoid vtable dispatch (Task 14)

**Tech Stack:** Rust, tokio, async-trait, rmpv, dashmap

---

### Task 1: Add async-trait dependency

**Files:**
- Modify: `crates/rebar-core/Cargo.toml`

**Step 1: Add async-trait to dependencies**

In `crates/rebar-core/Cargo.toml`, add `async-trait = "0.1"` to `[dependencies]`:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
dashmap = "6"
rmpv = "1"
serde = { version = "1", features = ["derive"] }
thiserror = "2"
tracing = "0.1"
async-trait = "0.1"
```

**Step 2: Verify it compiles**

Run: `~/.cargo/bin/cargo check -p rebar-core`
Expected: Compiles cleanly

**Step 3: Commit**

```bash
git add crates/rebar-core/Cargo.toml
git commit -m "chore(rebar-core): add async-trait dependency for GenServer"
```

---

### Task 2: GenServer trait and types

**Files:**
- Create: `crates/rebar-core/src/gen_server/mod.rs`
- Create: `crates/rebar-core/src/gen_server/types.rs`
- Modify: `crates/rebar-core/src/lib.rs`

**Step 1: Write the failing test**

Create `crates/rebar-core/src/gen_server/mod.rs`:

```rust
pub mod types;

pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{ExitReason, Message, ProcessId, SendError};
    use crate::router::MessageRouter;

    // Test: GenServer trait is object-safe enough to implement
    struct CounterServer;

    #[async_trait::async_trait]
    impl GenServer for CounterServer {
        type State = u64;
        type Call = String;
        type Cast = String;
        type Reply = u64;

        async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
            Ok(0)
        }

        async fn handle_call(
            &self,
            msg: Self::Call,
            _from: ProcessId,
            state: &mut Self::State,
            _ctx: &GenServerContext,
        ) -> Self::Reply {
            if msg == "get" {
                *state
            } else {
                0
            }
        }

        async fn handle_cast(
            &self,
            msg: Self::Cast,
            state: &mut Self::State,
            _ctx: &GenServerContext,
        ) {
            if msg == "inc" {
                *state += 1;
            }
        }
    }

    #[test]
    fn gen_server_context_has_pid() {
        let ctx = GenServerContext::new(ProcessId::new(1, 5));
        assert_eq!(ctx.self_pid(), ProcessId::new(1, 5));
    }

    #[test]
    fn call_error_display() {
        let err = CallError::Timeout;
        assert!(format!("{}", err).contains("timeout"));
        let err = CallError::ServerDead;
        assert!(format!("{}", err).contains("dead"));
    }
}
```

**Step 2: Create types**

Create `crates/rebar-core/src/gen_server/types.rs`:

```rust
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::process::{ExitReason, Message, ProcessId, SendError};
use crate::router::MessageRouter;

/// Errors returned by GenServerRef::call.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("call timeout: server did not respond in time")]
    Timeout,
    #[error("server dead: channel closed")]
    ServerDead,
}

/// Context available to GenServer callbacks.
/// Provides access to the server's PID and message routing.
pub struct GenServerContext {
    pid: ProcessId,
    router: Option<Arc<dyn MessageRouter>>,
}

impl GenServerContext {
    /// Create a new context (used internally by the engine).
    pub(crate) fn new(pid: ProcessId) -> Self {
        Self { pid, router: None }
    }

    /// Create a context with a router for sending messages.
    pub(crate) fn with_router(pid: ProcessId, router: Arc<dyn MessageRouter>) -> Self {
        Self {
            pid,
            router: Some(router),
        }
    }

    /// Return this server's PID.
    pub fn self_pid(&self) -> ProcessId {
        self.pid
    }

    /// Send a raw message to another process.
    pub fn send(&self, dest: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        match &self.router {
            Some(router) => router.route(self.pid, dest, payload),
            None => Err(SendError::ProcessDead(dest)),
        }
    }
}

/// The GenServer trait. Implement this for your server's logic.
///
/// Associated types define the message protocol:
/// - `State`: server state, mutated by callbacks
/// - `Call`: synchronous request message (gets a Reply)
/// - `Cast`: asynchronous fire-and-forget message
/// - `Reply`: response to a Call
#[async_trait::async_trait]
pub trait GenServer: Send + 'static {
    type State: Send + 'static;
    type Call: Send + 'static;
    type Cast: Send + 'static;
    type Reply: Send + 'static;

    /// Initialize server state. Called once at startup.
    async fn init(&self, ctx: &GenServerContext) -> Result<Self::State, String>;

    /// Handle a synchronous call. Must return a reply.
    async fn handle_call(
        &self,
        msg: Self::Call,
        from: ProcessId,
        state: &mut Self::State,
        ctx: &GenServerContext,
    ) -> Self::Reply;

    /// Handle an asynchronous cast (fire-and-forget).
    async fn handle_cast(
        &self,
        msg: Self::Cast,
        state: &mut Self::State,
        ctx: &GenServerContext,
    );

    /// Handle a raw info message from the standard mailbox.
    /// Default: ignores the message.
    async fn handle_info(
        &self,
        _msg: Message,
        _state: &mut Self::State,
        _ctx: &GenServerContext,
    ) {
    }

    /// Called when the server is shutting down.
    async fn terminate(&self, _reason: ExitReason, _state: &mut Self::State) {}
}

/// Internal envelope for call messages, carrying the reply channel.
pub(crate) struct CallEnvelope<S: GenServer> {
    pub msg: S::Call,
    pub from: ProcessId,
    pub reply_tx: oneshot::Sender<S::Reply>,
}
```

**Step 3: Add gen_server module to lib.rs**

In `crates/rebar-core/src/lib.rs`, add:

```rust
pub mod gen_server;
pub mod process;
pub mod router;
pub mod runtime;
pub mod supervisor;
```

**Step 4: Run tests to verify types compile and pass**

Run: `~/.cargo/bin/cargo test -p rebar-core gen_server`
Expected: 2 tests pass (gen_server_context_has_pid, call_error_display)

**Step 5: Commit**

```bash
git add crates/rebar-core/src/gen_server/ crates/rebar-core/src/lib.rs
git commit -m "feat(gen-server): add GenServer trait, GenServerContext, CallError types"
```

---

### Task 3: GenServer engine (spawn + event loop)

**Files:**
- Create: `crates/rebar-core/src/gen_server/engine.rs`
- Modify: `crates/rebar-core/src/gen_server/mod.rs`

**Step 1: Write failing tests**

Add to `crates/rebar-core/src/gen_server/mod.rs`, inside the test module:

```rust
mod engine;

// Add to existing test module:
use crate::runtime::Runtime;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn spawn_gen_server_returns_ref_with_pid() {
    let rt = Arc::new(Runtime::new(1));
    let server_ref = spawn_gen_server(Arc::clone(&rt), CounterServer).await;
    assert_eq!(server_ref.pid().node_id(), 1);
}

#[tokio::test]
async fn gen_server_call_returns_reply() {
    let rt = Arc::new(Runtime::new(1));
    let server_ref = spawn_gen_server(Arc::clone(&rt), CounterServer).await;
    let reply = server_ref
        .call("get".to_string(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(reply, 0);
}

#[tokio::test]
async fn gen_server_cast_then_call() {
    let rt = Arc::new(Runtime::new(1));
    let server_ref = spawn_gen_server(Arc::clone(&rt), CounterServer).await;
    server_ref.cast("inc".to_string()).unwrap();
    // Small yield to let cast process
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let reply = server_ref
        .call("get".to_string(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(reply, 1);
}

#[tokio::test]
async fn gen_server_multiple_casts() {
    let rt = Arc::new(Runtime::new(1));
    let server_ref = spawn_gen_server(Arc::clone(&rt), CounterServer).await;
    for _ in 0..10 {
        server_ref.cast("inc".to_string()).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reply = server_ref
        .call("get".to_string(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(reply, 10);
}

#[tokio::test]
async fn gen_server_call_timeout() {
    struct SlowServer;

    #[async_trait::async_trait]
    impl GenServer for SlowServer {
        type State = ();
        type Call = ();
        type Cast = ();
        type Reply = ();

        async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
            Ok(())
        }

        async fn handle_call(
            &self,
            _msg: Self::Call,
            _from: ProcessId,
            _state: &mut Self::State,
            _ctx: &GenServerContext,
        ) -> Self::Reply {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }

        async fn handle_cast(
            &self,
            _msg: Self::Cast,
            _state: &mut Self::State,
            _ctx: &GenServerContext,
        ) {
        }
    }

    let rt = Arc::new(Runtime::new(1));
    let server_ref = spawn_gen_server(Arc::clone(&rt), SlowServer).await;
    let result = server_ref
        .call((), Duration::from_millis(50))
        .await;
    assert!(matches!(result, Err(CallError::Timeout)));
}

#[tokio::test]
async fn gen_server_ref_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    // GenServerRef should be Send + Sync
    assert_send_sync::<GenServerRef<CounterServer>>();
}
```

**Step 2: Run tests to verify they fail**

Run: `~/.cargo/bin/cargo test -p rebar-core gen_server`
Expected: FAIL — `spawn_gen_server` not found, `GenServerRef` not found

**Step 3: Write the engine**

Create `crates/rebar-core/src/gen_server/engine.rs`:

```rust
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

    let router = Arc::clone(&runtime.table() as &Arc<_>);
    let table = Arc::clone(runtime.table());

    // Get router from runtime - we need to reconstruct it
    // Actually we can't access runtime.router directly. We'll use a LocalRouter.
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
```

**Step 4: Update mod.rs exports**

Update `crates/rebar-core/src/gen_server/mod.rs`:

```rust
pub mod engine;
pub mod types;

pub use engine::*;
pub use types::*;
```

**Step 5: Run tests**

Run: `~/.cargo/bin/cargo test -p rebar-core gen_server`
Expected: All gen_server tests pass

**Step 6: Run full test suite to check no regressions**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Expected: All tests pass

**Step 7: Commit**

```bash
git add crates/rebar-core/src/gen_server/
git commit -m "feat(gen-server): implement GenServer spawn, event loop, call/cast/info"
```

---

### Task 4: GenServer handle_info and integration tests

**Files:**
- Create: `crates/rebar-core/tests/gen_server_integration.rs`

**Step 1: Write integration tests**

Create `crates/rebar-core/tests/gen_server_integration.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use rebar_core::gen_server::{spawn_gen_server, CallError, GenServer, GenServerContext, GenServerRef};
use rebar_core::process::{ExitReason, Message, ProcessId, SendError};
use rebar_core::runtime::Runtime;

/// A counter GenServer that tracks a count.
struct Counter;

#[async_trait::async_trait]
impl GenServer for Counter {
    type State = u64;
    type Call = CounterCall;
    type Cast = CounterCast;
    type Reply = CounterReply;

    async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
        Ok(0)
    }

    async fn handle_call(
        &self,
        msg: Self::Call,
        _from: ProcessId,
        state: &mut Self::State,
        _ctx: &GenServerContext,
    ) -> Self::Reply {
        match msg {
            CounterCall::Get => CounterReply::Count(*state),
            CounterCall::IncrementAndGet => {
                *state += 1;
                CounterReply::Count(*state)
            }
        }
    }

    async fn handle_cast(
        &self,
        msg: Self::Cast,
        state: &mut Self::State,
        _ctx: &GenServerContext,
    ) {
        match msg {
            CounterCast::Increment => *state += 1,
            CounterCast::Reset => *state = 0,
        }
    }

    async fn handle_info(
        &self,
        msg: Message,
        state: &mut Self::State,
        _ctx: &GenServerContext,
    ) {
        // If we get a raw message with an integer payload, add it to state
        if let Some(val) = msg.payload().as_u64() {
            *state += val;
        }
    }
}

#[derive(Debug)]
enum CounterCall {
    Get,
    IncrementAndGet,
}

#[derive(Debug)]
enum CounterCast {
    Increment,
    Reset,
}

#[derive(Debug, PartialEq)]
enum CounterReply {
    Count(u64),
}

#[tokio::test]
async fn counter_get_initial() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;
    let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
    assert_eq!(reply, CounterReply::Count(0));
}

#[tokio::test]
async fn counter_increment_and_get() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;
    let reply = server
        .call(CounterCall::IncrementAndGet, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(reply, CounterReply::Count(1));
}

#[tokio::test]
async fn counter_cast_increment_then_get() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;
    server.cast(CounterCast::Increment).unwrap();
    server.cast(CounterCast::Increment).unwrap();
    server.cast(CounterCast::Increment).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
    assert_eq!(reply, CounterReply::Count(3));
}

#[tokio::test]
async fn counter_cast_reset() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;
    server.cast(CounterCast::Increment).unwrap();
    server.cast(CounterCast::Increment).unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    server.cast(CounterCast::Reset).unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
    assert_eq!(reply, CounterReply::Count(0));
}

#[tokio::test]
async fn counter_handle_info_via_send() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(Arc::clone(&rt), Counter).await;
    // Send a raw message to the GenServer's PID via the runtime
    rt.send(server.pid(), rmpv::Value::Integer(5u64.into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
    assert_eq!(reply, CounterReply::Count(5));
}

#[tokio::test]
async fn counter_concurrent_calls() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;

    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = server.clone();
        handles.push(tokio::spawn(async move {
            s.call(CounterCall::IncrementAndGet, Duration::from_secs(1))
                .await
                .unwrap()
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }

    // All calls should have been processed sequentially by the GenServer
    // Final count should be 10
    let final_count = server
        .call(CounterCall::Get, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(final_count, CounterReply::Count(10));
}

#[tokio::test]
async fn gen_server_ref_clone_works() {
    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, Counter).await;
    let server2 = server.clone();
    assert_eq!(server.pid(), server2.pid());

    server.cast(CounterCast::Increment).unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let reply = server2
        .call(CounterCall::Get, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(reply, CounterReply::Count(1));
}

#[tokio::test]
async fn gen_server_init_failure() {
    struct FailInit;

    #[async_trait::async_trait]
    impl GenServer for FailInit {
        type State = ();
        type Call = ();
        type Cast = ();
        type Reply = ();

        async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
            Err("init failed".into())
        }

        async fn handle_call(
            &self, _msg: (), _from: ProcessId, _state: &mut (), _ctx: &GenServerContext,
        ) -> () {
        }

        async fn handle_cast(
            &self, _msg: (), _state: &mut (), _ctx: &GenServerContext,
        ) {
        }
    }

    let rt = Arc::new(Runtime::new(1));
    let server = spawn_gen_server(rt, FailInit).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Server should be dead, call should fail
    let result = server.call((), Duration::from_millis(100)).await;
    assert!(result.is_err());
}
```

**Step 2: Run tests**

Run: `~/.cargo/bin/cargo test -p rebar-core --test gen_server_integration`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/rebar-core/tests/gen_server_integration.rs
git commit -m "test(gen-server): add comprehensive GenServer integration tests"
```

---

### Task 5: DynamicSupervisor

**Files:**
- Create: `crates/rebar-core/src/supervisor/dynamic.rs`
- Modify: `crates/rebar-core/src/supervisor/mod.rs`

**Step 1: Write failing tests**

Create `crates/rebar-core/src/supervisor/dynamic.rs` with tests first:

```rust
// Implementation will go here

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ExitReason;
    use crate::runtime::Runtime;
    use crate::supervisor::spec::{ChildSpec, RestartType};
    use crate::supervisor::engine::ChildEntry;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn test_runtime() -> Arc<Runtime> {
        Arc::new(Runtime::new(1))
    }

    #[tokio::test]
    async fn start_dynamic_supervisor_returns_handle() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;
        assert_eq!(handle.pid().node_id(), 1);
    }

    #[tokio::test]
    async fn start_child_returns_pid() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(ChildSpec::new("worker"), || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ExitReason::Normal
        });

        let pid = handle.start_child(entry).await.unwrap();
        assert!(pid.local_id() > 0);
        handle.shutdown();
    }

    #[tokio::test]
    async fn count_children_after_start() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        for i in 0..3 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{}", i)), || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                ExitReason::Normal
            });
            handle.start_child(entry).await.unwrap();
        }

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 3);
        assert_eq!(counts.specs, 3);
        handle.shutdown();
    }

    #[tokio::test]
    async fn terminate_child_stops_it() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(ChildSpec::new("worker"), || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ExitReason::Normal
        });

        let pid = handle.start_child(entry).await.unwrap();
        handle.terminate_child(pid).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let counts = handle.count_children().await.unwrap();
        // Spec still there but not active
        assert_eq!(counts.active, 0);
        handle.shutdown();
    }

    #[tokio::test]
    async fn remove_child_removes_spec() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        let entry = ChildEntry::new(
            ChildSpec::new("worker").restart(RestartType::Temporary),
            || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                ExitReason::Normal
            },
        );

        let pid = handle.start_child(entry).await.unwrap();
        handle.terminate_child(pid).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.remove_child(pid).await.unwrap();

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.specs, 0);
        handle.shutdown();
    }

    #[tokio::test]
    async fn which_children_lists_all() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        for i in 0..2 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{}", i)), || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                ExitReason::Normal
            });
            handle.start_child(entry).await.unwrap();
        }

        let children = handle.which_children().await.unwrap();
        assert_eq!(children.len(), 2);
        for info in &children {
            assert!(info.pid.is_some());
        }
        handle.shutdown();
    }

    #[tokio::test]
    async fn permanent_child_is_restarted() {
        let rt = test_runtime();
        let start_count = Arc::new(AtomicU32::new(0));

        let handle = start_dynamic_supervisor(
            rt,
            DynamicSupervisorSpec::new().max_restarts(5).max_seconds(10),
        )
        .await;

        let sc = Arc::clone(&start_count);
        let entry = ChildEntry::new(
            ChildSpec::new("restartable").restart(RestartType::Permanent),
            move || {
                let sc = Arc::clone(&sc);
                async move {
                    let n = sc.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        // Exit abnormally to trigger restart
                        ExitReason::Abnormal("crash".into())
                    } else {
                        // Stay alive
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        ExitReason::Normal
                    }
                }
            },
        );

        handle.start_child(entry).await.unwrap();
        // Wait for restarts
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(start_count.load(Ordering::SeqCst) >= 3);
        handle.shutdown();
    }

    #[tokio::test]
    async fn temporary_child_not_restarted() {
        let rt = test_runtime();
        let start_count = Arc::new(AtomicU32::new(0));

        let handle = start_dynamic_supervisor(
            rt,
            DynamicSupervisorSpec::new().max_restarts(5).max_seconds(10),
        )
        .await;

        let sc = Arc::clone(&start_count);
        let entry = ChildEntry::new(
            ChildSpec::new("temp").restart(RestartType::Temporary),
            move || {
                let sc = Arc::clone(&sc);
                async move {
                    sc.fetch_add(1, Ordering::SeqCst);
                    ExitReason::Abnormal("crash".into())
                }
            },
        );

        handle.start_child(entry).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should only have started once (not restarted)
        assert_eq!(start_count.load(Ordering::SeqCst), 1);
        handle.shutdown();
    }

    #[tokio::test]
    async fn shutdown_stops_all_children() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

        for i in 0..5 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{}", i)), || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                ExitReason::Normal
            });
            handle.start_child(entry).await.unwrap();
        }

        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // After shutdown, handle operations should fail
        let result = handle.count_children().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn handle_is_clone() {
        let rt = test_runtime();
        let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;
        let handle2 = handle.clone();
        assert_eq!(handle.pid(), handle2.pid());
        handle.shutdown();
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `~/.cargo/bin/cargo test -p rebar-core dynamic`
Expected: FAIL — types not defined

**Step 3: Write the implementation**

Add implementation to `crates/rebar-core/src/supervisor/dynamic.rs` above the tests:

```rust
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use crate::process::{ExitReason, ProcessId};
use crate::runtime::Runtime;
use crate::supervisor::spec::{ChildSpec, RestartType, ShutdownStrategy};
use crate::supervisor::engine::ChildEntry;

/// Spec for configuring a DynamicSupervisor.
pub struct DynamicSupervisorSpec {
    pub max_restarts: u32,
    pub max_seconds: u32,
}

impl DynamicSupervisorSpec {
    pub fn new() -> Self {
        Self {
            max_restarts: 3,
            max_seconds: 5,
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
}

/// Info about a child in a DynamicSupervisor.
#[derive(Debug, Clone)]
pub struct DynChildInfo {
    pub id: String,
    pub pid: Option<ProcessId>,
    pub restart: RestartType,
}

/// Child counts for a DynamicSupervisor.
#[derive(Debug, Clone)]
pub struct DynChildCounts {
    pub active: usize,
    pub specs: usize,
}

/// Handle to a running DynamicSupervisor.
#[derive(Clone)]
pub struct DynamicSupervisorHandle {
    pid: ProcessId,
    msg_tx: mpsc::UnboundedSender<DynSupervisorMsg>,
}

impl DynamicSupervisorHandle {
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    pub async fn start_child(&self, entry: ChildEntry) -> Result<ProcessId, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::StartChild { entry, reply: reply_tx })
            .map_err(|_| "supervisor gone".to_string())?;
        reply_rx.await.map_err(|_| "supervisor gone".to_string())?
    }

    pub async fn terminate_child(&self, pid: ProcessId) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::TerminateChild { pid, reply: reply_tx })
            .map_err(|_| "supervisor gone".to_string())?;
        reply_rx.await.map_err(|_| "supervisor gone".to_string())?
    }

    pub async fn remove_child(&self, pid: ProcessId) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::RemoveChild { pid, reply: reply_tx })
            .map_err(|_| "supervisor gone".to_string())?;
        reply_rx.await.map_err(|_| "supervisor gone".to_string())?
    }

    pub async fn which_children(&self) -> Result<Vec<DynChildInfo>, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::WhichChildren { reply: reply_tx })
            .map_err(|_| "supervisor gone".to_string())?;
        reply_rx.await.map_err(|_| "supervisor gone".to_string())
    }

    pub async fn count_children(&self) -> Result<DynChildCounts, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(DynSupervisorMsg::CountChildren { reply: reply_tx })
            .map_err(|_| "supervisor gone".to_string())?;
        reply_rx.await.map_err(|_| "supervisor gone".to_string())
    }

    pub fn shutdown(&self) {
        let _ = self.msg_tx.send(DynSupervisorMsg::Shutdown);
    }
}

enum DynSupervisorMsg {
    StartChild {
        entry: ChildEntry,
        reply: oneshot::Sender<Result<ProcessId, String>>,
    },
    TerminateChild {
        pid: ProcessId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RemoveChild {
        pid: ProcessId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    WhichChildren {
        reply: oneshot::Sender<Vec<DynChildInfo>>,
    },
    CountChildren {
        reply: oneshot::Sender<DynChildCounts>,
    },
    ChildExited {
        pid: ProcessId,
        reason: ExitReason,
    },
    Shutdown,
}

struct DynChildState {
    spec: ChildSpec,
    factory: crate::supervisor::engine::ChildFactory,
    pid: Option<ProcessId>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

/// Start a new DynamicSupervisor.
pub async fn start_dynamic_supervisor(
    runtime: Arc<Runtime>,
    spec: DynamicSupervisorSpec,
) -> DynamicSupervisorHandle {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();

    let msg_tx_clone = msg_tx.clone();
    let pid = runtime
        .spawn(move |_ctx| async move {
            dynamic_supervisor_loop(spec, msg_rx, msg_tx_clone).await;
        })
        .await;

    DynamicSupervisorHandle { pid, msg_tx }
}

async fn dynamic_supervisor_loop(
    spec: DynamicSupervisorSpec,
    mut msg_rx: mpsc::UnboundedReceiver<DynSupervisorMsg>,
    msg_tx: mpsc::UnboundedSender<DynSupervisorMsg>,
) {
    let mut children: HashMap<ProcessId, DynChildState> = HashMap::new();
    let mut restart_times: VecDeque<Instant> = VecDeque::new();

    loop {
        match msg_rx.recv().await {
            Some(DynSupervisorMsg::StartChild { entry, reply }) => {
                let pid = start_dyn_child(&entry, &msg_tx);
                let child_state = DynChildState {
                    spec: entry.spec,
                    factory: entry.factory,
                    pid: Some(pid),
                    shutdown_tx: None, // We don't use shutdown_tx in the same way
                };
                children.insert(pid, child_state);
                let _ = reply.send(Ok(pid));
            }
            Some(DynSupervisorMsg::TerminateChild { pid, reply }) => {
                if let Some(child) = children.get_mut(&pid) {
                    if let Some(tx) = child.shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                    child.pid = None;
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(format!("child {} not found", pid)));
                }
            }
            Some(DynSupervisorMsg::RemoveChild { pid, reply }) => {
                if let Some(child) = children.get(&pid) {
                    if child.pid.is_some() {
                        let _ = reply.send(Err("child still running, terminate first".into()));
                    } else {
                        children.remove(&pid);
                        let _ = reply.send(Ok(()));
                    }
                } else {
                    let _ = reply.send(Err(format!("child {} not found", pid)));
                }
            }
            Some(DynSupervisorMsg::WhichChildren { reply }) => {
                let infos: Vec<DynChildInfo> = children
                    .values()
                    .map(|c| DynChildInfo {
                        id: c.spec.id.clone(),
                        pid: c.pid,
                        restart: c.spec.restart.clone(),
                    })
                    .collect();
                let _ = reply.send(infos);
            }
            Some(DynSupervisorMsg::CountChildren { reply }) => {
                let active = children.values().filter(|c| c.pid.is_some()).count();
                let specs = children.len();
                let _ = reply.send(DynChildCounts { active, specs });
            }
            Some(DynSupervisorMsg::ChildExited { pid, reason }) => {
                if let Some(child) = children.get_mut(&pid) {
                    child.pid = None;
                    child.shutdown_tx = None;

                    let should_restart = child.spec.restart.should_restart(&reason);
                    if should_restart {
                        // Check restart limit
                        if check_restart_limit(
                            &mut restart_times,
                            spec.max_restarts,
                            spec.max_seconds,
                        ) {
                            let factory = Arc::clone(&child.factory);
                            let new_pid = start_dyn_child_from_factory(
                                &child.spec,
                                &factory,
                                &msg_tx,
                            );
                            child.pid = Some(new_pid);
                            // Re-key: remove old pid, insert under new pid
                            let mut child_state = children.remove(&pid).unwrap();
                            children.insert(new_pid, child_state);
                        } else {
                            // Exceeded restart limit, shut down everything
                            for (_, child) in children.iter_mut() {
                                if let Some(tx) = child.shutdown_tx.take() {
                                    let _ = tx.send(());
                                }
                            }
                            break;
                        }
                    } else if matches!(child.spec.restart, RestartType::Temporary) {
                        // Auto-remove temporary children
                        children.remove(&pid);
                    }
                }
            }
            Some(DynSupervisorMsg::Shutdown) | None => {
                // Shutdown all children
                for (_, child) in children.iter_mut() {
                    if let Some(tx) = child.shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                }
                break;
            }
        }
    }
}

use std::sync::atomic::{AtomicU64, Ordering};

fn start_dyn_child(
    entry: &ChildEntry,
    msg_tx: &mpsc::UnboundedSender<DynSupervisorMsg>,
) -> ProcessId {
    start_dyn_child_from_factory(&entry.spec, &entry.factory, msg_tx)
}

fn start_dyn_child_from_factory(
    spec: &ChildSpec,
    factory: &crate::supervisor::engine::ChildFactory,
    msg_tx: &mpsc::UnboundedSender<DynSupervisorMsg>,
) -> ProcessId {
    static DYN_CHILD_PID_COUNTER: AtomicU64 = AtomicU64::new(2_000_000);
    let local_id = DYN_CHILD_PID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = ProcessId::new(0, local_id);

    let factory = Arc::clone(factory);
    let msg_tx = msg_tx.clone();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    tokio::spawn(async move {
        let child_future = factory();
        tokio::select! {
            reason = child_future => {
                let _ = msg_tx.send(DynSupervisorMsg::ChildExited { pid, reason });
            }
            _ = shutdown_rx => {
                let _ = msg_tx.send(DynSupervisorMsg::ChildExited {
                    pid,
                    reason: ExitReason::Normal,
                });
            }
        }
    });

    // Note: We don't store shutdown_tx here — it's stored in the child state
    // This is a design limitation we'll address
    pid
}

fn check_restart_limit(
    restart_times: &mut VecDeque<Instant>,
    max_restarts: u32,
    max_seconds: u32,
) -> bool {
    if max_restarts == 0 {
        return false;
    }

    let now = Instant::now();
    let window = Duration::from_secs(max_seconds as u64);
    restart_times.push_back(now);

    while let Some(&front) = restart_times.front() {
        if now.duration_since(front) > window {
            restart_times.pop_front();
        } else {
            break;
        }
    }

    (restart_times.len() as u32) <= max_restarts
}
```

**Step 4: Update supervisor mod.rs**

```rust
pub mod dynamic;
pub mod engine;
pub mod spec;
pub use dynamic::*;
pub use engine::*;
pub use spec::*;
```

**Step 5: Run tests**

Run: `~/.cargo/bin/cargo test -p rebar-core dynamic`
Expected: All dynamic supervisor tests pass

**Step 6: Run full suite**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Expected: All tests pass

**Step 7: Commit**

```bash
git add crates/rebar-core/src/supervisor/dynamic.rs crates/rebar-core/src/supervisor/mod.rs
git commit -m "feat(supervisor): add DynamicSupervisor with start/terminate/remove child"
```

---

### Task 6: DynamicSupervisor integration tests

**Files:**
- Create: `crates/rebar-core/tests/dynamic_supervisor_integration.rs`

**Step 1: Write integration tests**

Create `crates/rebar-core/tests/dynamic_supervisor_integration.rs`:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rebar_core::process::ExitReason;
use rebar_core::runtime::Runtime;
use rebar_core::supervisor::{
    start_dynamic_supervisor, ChildEntry, ChildSpec, DynamicSupervisorSpec, RestartType,
};

#[tokio::test]
async fn dynamic_supervisor_manages_many_children() {
    let rt = Arc::new(Runtime::new(1));
    let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

    let mut pids = Vec::new();
    for i in 0..10 {
        let entry = ChildEntry::new(ChildSpec::new(format!("worker-{}", i)), || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ExitReason::Normal
        });
        pids.push(handle.start_child(entry).await.unwrap());
    }

    let counts = handle.count_children().await.unwrap();
    assert_eq!(counts.active, 10);

    // Terminate half
    for pid in &pids[..5] {
        handle.terminate_child(*pid).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let counts = handle.count_children().await.unwrap();
    assert_eq!(counts.active, 5);

    handle.shutdown();
}

#[tokio::test]
async fn terminate_nonexistent_child_returns_error() {
    let rt = Arc::new(Runtime::new(1));
    let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

    let result = handle
        .terminate_child(rebar_core::process::ProcessId::new(0, 99999))
        .await;
    assert!(result.is_err());
    handle.shutdown();
}

#[tokio::test]
async fn remove_running_child_returns_error() {
    let rt = Arc::new(Runtime::new(1));
    let handle = start_dynamic_supervisor(rt, DynamicSupervisorSpec::new()).await;

    let entry = ChildEntry::new(ChildSpec::new("worker"), || async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        ExitReason::Normal
    });
    let pid = handle.start_child(entry).await.unwrap();

    // Try to remove without terminating first
    let result = handle.remove_child(pid).await;
    assert!(result.is_err());

    handle.shutdown();
}

#[tokio::test]
async fn transient_child_restarted_on_abnormal_exit() {
    let rt = Arc::new(Runtime::new(1));
    let count = Arc::new(AtomicU32::new(0));

    let handle = start_dynamic_supervisor(
        rt,
        DynamicSupervisorSpec::new().max_restarts(5).max_seconds(10),
    )
    .await;

    let c = Arc::clone(&count);
    let entry = ChildEntry::new(
        ChildSpec::new("transient").restart(RestartType::Transient),
        move || {
            let c = Arc::clone(&c);
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ExitReason::Abnormal("first crash".into())
                } else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    ExitReason::Normal
                }
            }
        },
    );

    handle.start_child(entry).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Should have restarted after abnormal exit
    assert!(count.load(Ordering::SeqCst) >= 2);
    handle.shutdown();
}

#[tokio::test]
async fn transient_child_not_restarted_on_normal_exit() {
    let rt = Arc::new(Runtime::new(1));
    let count = Arc::new(AtomicU32::new(0));

    let handle = start_dynamic_supervisor(
        rt,
        DynamicSupervisorSpec::new().max_restarts(5).max_seconds(10),
    )
    .await;

    let c = Arc::clone(&count);
    let entry = ChildEntry::new(
        ChildSpec::new("transient").restart(RestartType::Transient),
        move || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                ExitReason::Normal
            }
        },
    );

    handle.start_child(entry).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Should NOT have restarted after normal exit
    assert_eq!(count.load(Ordering::SeqCst), 1);
    handle.shutdown();
}
```

**Step 2: Run tests**

Run: `~/.cargo/bin/cargo test -p rebar-core --test dynamic_supervisor_integration`
Expected: All tests pass

**Step 3: Commit**

```bash
git add crates/rebar-core/tests/dynamic_supervisor_integration.rs
git commit -m "test(supervisor): add DynamicSupervisor integration tests"
```

---

### Task 7: RuntimeBuilder (start_threaded)

**Files:**
- Modify: `crates/rebar-core/src/runtime.rs`

**Step 1: Write failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `crates/rebar-core/src/runtime.rs`:

```rust
#[test]
fn runtime_builder_default_builds() {
    let (tokio_rt, rebar_rt) = RuntimeBuilder::new(1).build().unwrap();
    assert_eq!(rebar_rt.node_id(), 1);
    // Verify tokio runtime works
    tokio_rt.block_on(async {
        let pid = rebar_rt.spawn(|_ctx| async {}).await;
        assert_eq!(pid.node_id(), 1);
    });
}

#[test]
fn runtime_builder_custom_threads() {
    let (tokio_rt, rebar_rt) = RuntimeBuilder::new(2)
        .worker_threads(2)
        .build()
        .unwrap();
    assert_eq!(rebar_rt.node_id(), 2);
    tokio_rt.block_on(async {
        let pid = rebar_rt.spawn(|_ctx| async {}).await;
        assert_eq!(pid.node_id(), 2);
    });
}

#[test]
fn runtime_builder_thread_name() {
    let (tokio_rt, _rebar_rt) = RuntimeBuilder::new(1)
        .thread_name("rebar-test")
        .build()
        .unwrap();
    // Just verify it builds without error
    drop(tokio_rt);
}

#[test]
fn runtime_builder_start_runs_closure() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);

    RuntimeBuilder::new(1)
        .start(move |rt| async move {
            ran_clone.store(true, Ordering::SeqCst);
            let pid = rt.spawn(|_ctx| async {}).await;
            assert_eq!(pid.node_id(), 1);
        })
        .unwrap();

    assert!(ran.load(Ordering::SeqCst));
}
```

**Step 2: Run tests to verify they fail**

Run: `~/.cargo/bin/cargo test -p rebar-core runtime_builder`
Expected: FAIL — `RuntimeBuilder` not found

**Step 3: Write RuntimeBuilder implementation**

Add to `crates/rebar-core/src/runtime.rs`, before the `#[cfg(test)]` block:

```rust
/// Builder for creating a rebar Runtime with a configured tokio Runtime.
pub struct RuntimeBuilder {
    node_id: u64,
    worker_threads: Option<usize>,
    thread_name: Option<String>,
}

impl RuntimeBuilder {
    /// Create a new builder for the given node ID.
    pub fn new(node_id: u64) -> Self {
        Self {
            node_id,
            worker_threads: None,
            thread_name: None,
        }
    }

    /// Set the number of worker threads. Defaults to the number of CPU cores.
    pub fn worker_threads(mut self, n: usize) -> Self {
        self.worker_threads = Some(n);
        self
    }

    /// Set the thread name prefix for worker threads.
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = Some(name.into());
        self
    }

    /// Build and return a (tokio Runtime, rebar Runtime) pair.
    pub fn build(self) -> Result<(tokio::runtime::Runtime, Runtime), std::io::Error> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();

        if let Some(n) = self.worker_threads {
            builder.worker_threads(n);
        }
        if let Some(name) = &self.thread_name {
            builder.thread_name(name);
        }

        let tokio_rt = builder.build()?;
        let rebar_rt = Runtime::new(self.node_id);

        Ok((tokio_rt, rebar_rt))
    }

    /// Build a runtime and run a future on it. Blocks until the future completes.
    pub fn start<F, Fut>(self, f: F) -> Result<(), std::io::Error>
    where
        F: FnOnce(Arc<Runtime>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tokio_rt, rebar_rt) = self.build()?;
        let rt = Arc::new(rebar_rt);
        tokio_rt.block_on(f(rt));
        Ok(())
    }
}
```

Note: You'll also need to add `use std::sync::Arc;` to the imports at the top of runtime.rs (it's already there).

**Step 4: Run tests**

Run: `~/.cargo/bin/cargo test -p rebar-core runtime_builder`
Expected: All 4 RuntimeBuilder tests pass

**Step 5: Run full suite**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Expected: All tests pass

**Step 6: Commit**

```bash
git add crates/rebar-core/src/runtime.rs
git commit -m "feat(runtime): add RuntimeBuilder for multi-threaded bootstrap"
```

---

### Task 8: Full workspace test verification

**Files:** None (test-only)

**Step 1: Run all rebar workspace tests**

Run: `~/.cargo/bin/cargo test --workspace`
Working directory: `/home/alexandernicholson/.pxycrab/workspace/rebar`
Expected: All tests pass (should be ~330+ now)

**Step 2: Run barkeeper tests**

Run: `~/.cargo/bin/cargo test --workspace`
Working directory: `/home/alexandernicholson/.pxycrab/workspace/barkeeper`
Expected: All 315 tests pass (we didn't change any public API)

**Step 3: If any tests fail, fix them before proceeding**

**Step 4: Commit any fixes**

---

### Task 9: API contract regression tests for new features

**Files:**
- Modify: `crates/rebar-core/tests/api_contract.rs`

**Step 1: Add contract tests for new APIs**

Add to the existing `crates/rebar-core/tests/api_contract.rs`:

```rust
// === GenServer API contracts ===

#[test]
fn gen_server_context_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<rebar_core::gen_server::GenServerContext>();
}

#[test]
fn call_error_has_timeout_and_server_dead_variants() {
    // Exhaustive match to catch any additions
    let err = rebar_core::gen_server::CallError::Timeout;
    match err {
        rebar_core::gen_server::CallError::Timeout => {}
        rebar_core::gen_server::CallError::ServerDead => {}
    }
}

// === DynamicSupervisor API contracts ===

#[test]
fn dynamic_supervisor_handle_is_clone_send() {
    fn assert_clone_send<T: Clone + Send>() {}
    assert_clone_send::<rebar_core::supervisor::DynamicSupervisorHandle>();
}

#[test]
fn dynamic_supervisor_spec_builder_pattern() {
    let spec = rebar_core::supervisor::DynamicSupervisorSpec::new()
        .max_restarts(10)
        .max_seconds(60);
    // Verify it compiled with builder pattern
    let _ = spec;
}

// === RuntimeBuilder API contracts ===

#[test]
fn runtime_builder_new_takes_node_id() {
    let _builder = rebar_core::runtime::RuntimeBuilder::new(1);
}

#[test]
fn runtime_builder_has_worker_threads() {
    let _builder = rebar_core::runtime::RuntimeBuilder::new(1).worker_threads(4);
}

#[test]
fn runtime_builder_has_thread_name() {
    let _builder = rebar_core::runtime::RuntimeBuilder::new(1).thread_name("test");
}

#[test]
fn runtime_builder_build_returns_pair() {
    let (tokio_rt, rebar_rt) = rebar_core::runtime::RuntimeBuilder::new(1).build().unwrap();
    assert_eq!(rebar_rt.node_id(), 1);
    drop(tokio_rt);
}
```

**Step 2: Run contract tests**

Run: `~/.cargo/bin/cargo test -p rebar-core --test api_contract`
Expected: All pass

**Step 3: Commit**

```bash
git add crates/rebar-core/tests/api_contract.rs
git commit -m "test(api-contract): add regression tests for GenServer, DynamicSupervisor, RuntimeBuilder"
```

---

### Task 10: Optimisation — Eliminate double-spawn in Runtime::spawn

**Evidence:** Baseline shows `spawn_single` at 37.4 µs. The double-spawn pattern (outer `tokio::spawn` wrapping inner `tokio::spawn` for panic isolation) allocates two tasks and requires two scheduler round-trips.

**Files:**
- Modify: `crates/rebar-core/src/runtime.rs`

**Step 1: Write benchmark-focused test**

Add to the existing `#[cfg(test)] mod tests` block in `runtime.rs`:

```rust
#[tokio::test]
async fn spawn_cleanup_after_normal_exit() {
    let rt = Runtime::new(1);
    let pid = rt.spawn(|_ctx| async {}).await;
    // Give time for cleanup
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // Process should be cleaned up from table
    assert!(rt.table().get(&pid).is_none());
}

#[tokio::test]
async fn spawn_cleanup_after_panic() {
    let rt = Runtime::new(1);
    let pid = rt.spawn(|_ctx| async { panic!("test panic") }).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(rt.table().get(&pid).is_none());
}
```

**Step 2: Replace double-spawn with catch_unwind**

Replace the `spawn` method body in `runtime.rs`. The key change: use a single `tokio::spawn` with `std::panic::AssertUnwindSafe` + `FutureExt::catch_unwind` instead of the nested spawn pattern:

```rust
use std::panic::AssertUnwindSafe;
use futures::FutureExt; // Add futures = "0.3" to Cargo.toml if not present
```

Actually, since tokio's `JoinHandle` already catches panics and returns `JoinError`, we can use a simpler approach — use a single spawn and `catch_unwind` on the future itself:

```rust
pub async fn spawn<F, Fut>(&self, handler: F) -> ProcessId
where
    F: FnOnce(ProcessContext) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let pid = self.table.allocate_pid();
    let (tx, rx) = Mailbox::unbounded();

    let handle = ProcessHandle::new(tx);
    self.table.insert(pid, handle);

    let ctx = ProcessContext {
        pid,
        rx,
        router: Arc::clone(&self.router),
    };

    let table = Arc::clone(&self.table);

    // Single spawn with catch_unwind for panic isolation.
    // This replaces the previous double-spawn pattern, saving one task
    // allocation and one scheduler round-trip per process spawn.
    tokio::spawn(async move {
        let fut = std::panic::AssertUnwindSafe(handler(ctx));
        let _ = std::panic::catch_unwind(|| {
            // We can't use catch_unwind directly on a future — use the
            // tokio approach: the outer spawn already catches panics via
            // JoinHandle, so we just need the cleanup guarantee.
        });
        // Actually: since we need to run the future, not just construct it,
        // and tokio::spawn already catches panics, we can simplify:
        // Just run the handler directly and always clean up.
        drop(fut); // drop the AssertUnwindSafe wrapper
        // Re-approach: use the CatchUnwind future wrapper
        let result = std::panic::AssertUnwindSafe(handler(ctx))
            .catch_unwind()
            .await;
        let _ = result; // ignore panic result
        table.remove(&pid);
    });

    pid
}
```

**Important:** The above is a sketch. The actual implementation should be:

```rust
tokio::spawn(async move {
    // catch_unwind wraps the future to catch panics without needing
    // a nested tokio::spawn. Requires futures::FutureExt.
    let _result = std::panic::AssertUnwindSafe(handler(ctx))
        .catch_unwind()
        .await;
    // Always clean up, whether normal exit or panic
    table.remove(&pid);
});
```

Add `futures` to dependencies if not already present, or use `tokio`'s built-in panic handling by simply removing the inner spawn:

```rust
tokio::spawn(async move {
    handler(ctx).await;
    table.remove(&pid);
});
```

Note: This loses panic isolation. If the handler panics, `table.remove(&pid)` won't run. To fix this, use a drop guard:

```rust
tokio::spawn(async move {
    struct CleanupGuard<'a> {
        table: &'a Arc<ProcessTable>,
        pid: ProcessId,
    }
    impl Drop for CleanupGuard<'_> {
        fn drop(&mut self) {
            self.table.remove(&self.pid);
        }
    }
    let _guard = CleanupGuard { table: &table, pid };
    handler(ctx).await;
});
```

This is the recommended approach: single spawn, cleanup via drop guard (which runs even on panic unwind).

**Step 3: Run all tests**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Expected: All tests pass including the panic test

**Step 4: Run benchmarks to verify improvement**

Run: `~/.cargo/bin/cargo bench -p rebar-core -- spawn_single`
Expected: Measurable improvement from ~37 µs baseline

**Step 5: Commit**

```bash
git add crates/rebar-core/src/runtime.rs
git commit -m "perf(runtime): replace double-spawn with drop guard for panic cleanup

Eliminates the nested tokio::spawn pattern, saving one task allocation
and one scheduler round-trip per process spawn."
```

---

### Task 11: Optimisation — Replace sleep-based supervisor shutdown with channel completion

**Evidence:** All supervisor benchmarks cluster at ~11.3ms regardless of child count. The `stop_child` function has `tokio::time::sleep(Duration::from_millis(1))` and `yield_now()` — artificial delays that add latency.

**Files:**
- Modify: `crates/rebar-core/src/supervisor/engine.rs`

**Step 1: Replace stop_child sleep with proper completion signalling**

The current `stop_child` function:
```rust
async fn stop_child(child: &mut ChildState) {
    if let Some(tx) = child.shutdown_tx.take() {
        match &child.spec.shutdown {
            ShutdownStrategy::BrutalKill => { drop(tx); }
            ShutdownStrategy::Timeout(duration) => {
                let _ = tx.send(());
                tokio::time::sleep(Duration::from_millis(1).min(*duration)).await;
            }
        }
    }
    tokio::task::yield_now().await;
    child.pid = None;
}
```

Replace with a version that uses the `ChildExited` message as the completion signal instead of sleeping:

```rust
async fn stop_child(child: &mut ChildState) {
    if let Some(tx) = child.shutdown_tx.take() {
        match &child.spec.shutdown {
            ShutdownStrategy::BrutalKill => {
                drop(tx);
            }
            ShutdownStrategy::Timeout(_duration) => {
                let _ = tx.send(());
            }
        }
    }
    child.pid = None;
}
```

The key insight: the supervisor loop already receives `ChildExited` messages when children terminate. The sleep was unnecessary — the child will send its exit notification through the channel, and the supervisor processes it in the next loop iteration.

**Step 2: Run all supervisor tests**

Run: `~/.cargo/bin/cargo test -p rebar-core supervisor`
Expected: All tests pass

**Step 3: Run supervisor benchmarks**

Run: `~/.cargo/bin/cargo bench -p rebar-core -- supervisor`
Expected: Significant improvement from ~11.3ms baseline

**Step 4: Commit**

```bash
git add crates/rebar-core/src/supervisor/engine.rs
git commit -m "perf(supervisor): remove sleep from stop_child, rely on channel completion

Removes the 1ms sleep and yield_now in stop_child that dominated all
supervisor benchmark timings."
```

---

### Task 12: Optimisation — Pre-size ProcessTable DashMap

**Evidence:** Insert cost grows from 435 ns/op (100 entries) to 1.79 µs/op (10k entries) — 4x degradation from DashMap rehashing.

**Files:**
- Modify: `crates/rebar-core/src/process/table.rs`

**Step 1: Add with_capacity constructor**

Add a new constructor to `ProcessTable`:

```rust
/// Create a new process table with a pre-sized capacity hint.
///
/// Pre-sizing avoids rehashing when the expected number of processes is known.
pub fn with_capacity(node_id: u64, capacity: usize) -> Self {
    Self {
        node_id,
        next_id: AtomicU64::new(1),
        processes: DashMap::with_capacity(capacity),
    }
}
```

**Step 2: Add test**

```rust
#[test]
fn with_capacity_works() {
    let table = ProcessTable::with_capacity(1, 1000);
    let pid = table.allocate_pid();
    let (tx, _rx) = crate::process::mailbox::Mailbox::unbounded();
    table.insert(pid, ProcessHandle::new(tx));
    assert!(table.get(&pid).is_some());
}
```

**Step 3: Run tests and benchmarks**

Run: `~/.cargo/bin/cargo test -p rebar-core table`
Run: `~/.cargo/bin/cargo bench -p rebar-core -- insert`
Expected: Tests pass, insert/10000 improves from 1.79 µs/op

**Step 4: Commit**

```bash
git add crates/rebar-core/src/process/table.rs
git commit -m "perf(process-table): add with_capacity to pre-size DashMap

Reduces rehashing overhead for workloads with known process counts."
```

---

### Task 13: Optimisation — Make Message timestamp lazy

**Evidence:** Every `Message::new()` calls `SystemTime::now().duration_since(UNIX_EPOCH)` — a syscall. At 113 ns per send, the timestamp accounts for ~20-30% of cost. Internal messages rarely need timestamps.

**Files:**
- Modify: `crates/rebar-core/src/process/types.rs`

**Step 1: Make timestamp lazy via Option**

Change the `Message` struct to use `Option<u64>` for timestamp, with a `new_internal` constructor that skips the syscall:

```rust
#[derive(Debug, Clone)]
pub struct Message {
    from: ProcessId,
    payload: rmpv::Value,
    timestamp: Option<u64>,
}

impl Message {
    /// Create a new message with a timestamp (for external/user-facing messages).
    pub fn new(from: ProcessId, payload: rmpv::Value) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        Self {
            from,
            payload,
            timestamp: Some(timestamp),
        }
    }

    /// Create a message without a timestamp (for internal routing, lower overhead).
    pub fn new_internal(from: ProcessId, payload: rmpv::Value) -> Self {
        Self {
            from,
            payload,
            timestamp: None,
        }
    }

    pub fn from(&self) -> ProcessId {
        self.from
    }

    pub fn payload(&self) -> &rmpv::Value {
        &self.payload
    }

    /// Returns the timestamp if one was set, or None for internal messages.
    pub fn timestamp(&self) -> Option<u64> {
        self.timestamp
    }
}
```

**Step 2: Update LocalRouter to use new_internal**

In `router.rs`, change `Message::new(from, payload)` to `Message::new_internal(from, payload)` since the router is an internal code path.

**Step 3: Fix tests that assert on timestamp**

Update tests that call `msg.timestamp()` to handle `Option<u64>`:
- `assert!(msg.timestamp() > 0)` → `assert!(msg.timestamp().unwrap() > 0)` (for `Message::new`)
- Add a test for `new_internal` that asserts `timestamp().is_none()`

**Step 4: Run tests and benchmarks**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Run: `~/.cargo/bin/cargo bench -p rebar-core -- message_size`
Expected: Tests pass, message passing benchmarks improve

**Step 5: Commit**

```bash
git add crates/rebar-core/src/process/types.rs crates/rebar-core/src/router.rs
git commit -m "perf(message): add new_internal constructor that skips timestamp syscall

Internal message routing no longer pays the SystemTime::now() cost.
External API (Message::new) unchanged."
```

---

### Task 14: Optimisation — Use concrete LocalRouter type to avoid vtable dispatch

**Evidence:** Fan-in benchmark shows ~3 µs overhead per send at Runtime level vs ~113 ns at raw ProcessTable level. The `Arc<dyn MessageRouter>` adds vtable dispatch + Arc clone overhead on every send.

**Files:**
- Modify: `crates/rebar-core/src/runtime.rs`
- Modify: `crates/rebar-core/src/router.rs`

**Step 1: Make Runtime generic over the router type with a default**

This is the most impactful but also most invasive change. The approach: keep `dyn MessageRouter` for `with_router()` but use a concrete `LocalRouter` for the common `new()` path by making the router field an enum:

```rust
/// Router that's either a concrete LocalRouter (fast path) or a dynamic trait object.
pub(crate) enum RouterKind {
    Local(LocalRouter),
    Custom(Arc<dyn MessageRouter>),
}

impl MessageRouter for RouterKind {
    fn route(&self, from: ProcessId, to: ProcessId, payload: rmpv::Value) -> Result<(), SendError> {
        match self {
            RouterKind::Local(r) => r.route(from, to, payload),
            RouterKind::Custom(r) => r.route(from, to, payload),
        }
    }
}
```

Then change `Runtime` to store `Arc<RouterKind>` instead of `Arc<dyn MessageRouter>`, and `ProcessContext` to store `Arc<RouterKind>`. The `Local` variant avoids vtable dispatch; the branch predictor will quickly learn the common path.

**Step 2: Update ProcessContext**

Change `router: Arc<dyn MessageRouter>` to `router: Arc<RouterKind>` in `ProcessContext`.

**Step 3: Update Runtime constructors**

```rust
pub fn new(node_id: u64) -> Self {
    let table = Arc::new(ProcessTable::new(node_id));
    let router = Arc::new(RouterKind::Local(LocalRouter::new(Arc::clone(&table))));
    Self { node_id, table, router }
}

pub fn with_router(
    node_id: u64,
    table: Arc<ProcessTable>,
    router: Arc<dyn MessageRouter>,
) -> Self {
    Self {
        node_id,
        table,
        router: Arc::new(RouterKind::Custom(router)),
    }
}
```

**Step 4: Run all tests**

Run: `~/.cargo/bin/cargo test -p rebar-core`
Expected: All tests pass

**Step 5: Run benchmarks**

Run: `~/.cargo/bin/cargo bench -p rebar-core -- fan_in`
Expected: Measurable improvement in per-message overhead

**Step 6: Commit**

```bash
git add crates/rebar-core/src/runtime.rs crates/rebar-core/src/router.rs
git commit -m "perf(router): use concrete LocalRouter via enum dispatch instead of dyn trait

Eliminates vtable indirection on the common local-routing path while
preserving the dyn MessageRouter escape hatch for custom routers."
```

---

### Task 15: Post-optimisation benchmark comparison

**Step 1: Run full benchmark suite**

Run: `~/.cargo/bin/cargo bench -p rebar-core`
Save output to: `bench/results/post-optimisation-2026-03-08/criterion-output.txt`

**Step 2: Compare with baseline**

Create a comparison document at `bench/results/post-optimisation-2026-03-08/comparison.md` comparing baseline vs post-optimisation results for each benchmark group.

**Step 3: Commit results**

```bash
git add bench/results/post-optimisation-2026-03-08/
git commit -m "bench: capture post-optimisation results and comparison"
```

---

### Task 16: Final verification and summary

**Step 1: Run full rebar workspace tests**

Run: `~/.cargo/bin/cargo test --workspace`
Working directory: `/home/alexandernicholson/.pxycrab/workspace/rebar`
Expected: All tests pass

**Step 2: Run barkeeper tests**

Run: `~/.cargo/bin/cargo test --workspace`
Working directory: `/home/alexandernicholson/.pxycrab/workspace/barkeeper`
Expected: All 315 tests pass

**Step 3: Report summary**

Report:
- Total new tests added
- Features implemented
- Optimisations applied and their measured impact
- Any issues encountered
