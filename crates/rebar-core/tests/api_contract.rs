//! API contract tests for rebar-core v5.
//!
//! These tests lock down the v5 API surface: ProcessId construction, Display format,
//! trait bounds (including intentional !Send/!Sync), sync spawn/send, the
//! test_executor pattern, GenServer typed associated types, and panic isolation.

use std::rc::Rc;

use rebar_core::executor::{ExecutorConfig, RebarExecutor};
use rebar_core::process::table::ProcessTable;
use rebar_core::process::{ProcessId, SendError};
use rebar_core::router::{LocalRouter, MessageRouter};
use rebar_core::runtime::Runtime;

fn test_executor() -> RebarExecutor {
    RebarExecutor::new(ExecutorConfig::default()).unwrap()
}

// ============================================================================
// ProcessId contracts
// ============================================================================

/// ProcessId::new takes three arguments (node_id, thread_id, local_id).
#[test]
fn process_id_is_three_arg() {
    let pid = ProcessId::new(1, 0, 42);
    assert_eq!(pid.node_id(), 1);
    assert_eq!(pid.thread_id(), 0);
    assert_eq!(pid.local_id(), 42);

    let pid2 = ProcessId::new(5, 3, 99);
    assert_eq!(pid2.node_id(), 5);
    assert_eq!(pid2.thread_id(), 3);
    assert_eq!(pid2.local_id(), 99);
}

/// Display format is `<node.thread.local>` (3-part).
#[test]
fn process_id_display_format() {
    assert_eq!(format!("{}", ProcessId::new(1, 0, 42)), "<1.0.42>");
    assert_eq!(format!("{}", ProcessId::new(0, 0, 0)), "<0.0.0>");
    assert_eq!(format!("{}", ProcessId::new(99, 7, 1000)), "<99.7.1000>");
}

/// ProcessId is Copy (it's just u64+u16+u64 primitives).
#[test]
fn process_id_is_copy() {
    let a = ProcessId::new(1, 0, 42);
    let b = a; // copy
    assert_eq!(a, b); // original still usable
}

// ============================================================================
// Trait bound contracts — v5 is intentionally !Send/!Sync (thread-per-core)
// ============================================================================

// Runtime is NOT Send and NOT Sync (contains Rc).
static_assertions::assert_not_impl_any!(Runtime: Send, Sync);

// LocalRouter is NOT Send and NOT Sync (contains Rc<ProcessTable>).
static_assertions::assert_not_impl_any!(LocalRouter: Send, Sync);

// ProcessTable is NOT Send and NOT Sync (contains Cell/RefCell).
static_assertions::assert_not_impl_any!(ProcessTable: Send, Sync);

// ProcessId IS Send+Sync+Copy (just primitives).
static_assertions::assert_impl_all!(ProcessId: Copy, Send, Sync);

// ============================================================================
// Sync spawn / sync send contracts
// ============================================================================

/// Runtime::spawn is synchronous — returns ProcessId directly, no .await.
#[test]
fn spawn_is_sync_returns_pid() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let pid: ProcessId = rt.spawn(|_ctx| async {}); // no .await
        assert_eq!(pid.node_id(), 1);
        assert!(pid.local_id() > 0);
    });
}

/// Runtime::send is synchronous — returns Result directly, no .await.
#[test]
fn send_is_sync() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let pid = rt.spawn(|mut ctx| async move {
            let _ = ctx.recv().await;
        });
        let result: Result<(), SendError> = rt.send(pid, rmpv::Value::Nil); // no .await
        assert!(result.is_ok());
    });
}

/// Runtime::with_router accepts Rc<ProcessTable> and Rc<dyn MessageRouter>.
#[test]
fn runtime_with_router_accepts_rc() {
    test_executor().block_on(async {
        let table = Rc::new(ProcessTable::new(1, 0));
        let router: Rc<dyn MessageRouter> = Rc::new(LocalRouter::new(Rc::clone(&table)));
        let rt = Runtime::with_router(1, table, router);
        assert_eq!(rt.node_id(), 1);
    });
}

// ============================================================================
// Error variant contracts
// ============================================================================

/// SendError has exactly three variants: ProcessDead, MailboxFull, NodeUnreachable.
/// No NameNotFound, no MalformedFrame, no #[non_exhaustive].
#[test]
fn send_error_variants_exhaustive() {
    let errors: Vec<SendError> = vec![
        SendError::ProcessDead(ProcessId::new(1, 0, 1)),
        SendError::MailboxFull(ProcessId::new(1, 0, 2)),
        SendError::NodeUnreachable(99),
    ];

    for err in &errors {
        match err {
            SendError::ProcessDead(pid) => { let _ = pid; }
            SendError::MailboxFull(pid) => { let _ = pid; }
            SendError::NodeUnreachable(node) => { let _ = node; }
        }
    }
}

/// CallError has exactly two variants: Timeout, ServerDead (no SendFailed).
#[test]
fn call_error_variants() {
    let err = rebar_core::gen_server::CallError::Timeout;
    match err {
        rebar_core::gen_server::CallError::Timeout => {}
        rebar_core::gen_server::CallError::ServerDead => {}
    }
}

// ============================================================================
// Panic isolation
// ============================================================================

/// A panicking process does not crash the runtime. After the panic, a new
/// healthy process can be spawned and runs successfully.
#[test]
fn panic_isolation() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);

        rt.spawn(|_ctx| async move {
            panic!("intentional panic for contract test");
        });

        // Let the panic propagate
        rebar_core::time::sleep(std::time::Duration::from_millis(50)).await;

        // Runtime survives — spawn a healthy process
        let done = Rc::new(std::cell::Cell::new(false));
        let done_clone = Rc::clone(&done);
        rt.spawn(move |_ctx| async move {
            done_clone.set(true);
        });

        rebar_core::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(done.get(), "runtime must survive a process panic");
    });
}

/// After a process panics, sending to its PID returns ProcessDead.
#[test]
fn panicking_process_cleaned_up() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);

        let pid = rt.spawn(|_ctx| async move {
            panic!("intentional panic for cleanup test");
        });

        rebar_core::time::sleep(std::time::Duration::from_millis(100)).await;

        let result = rt.send(pid, rmpv::Value::Nil);
        assert!(
            matches!(result, Err(SendError::ProcessDead(_))),
            "sending to a panicked PID must return ProcessDead, got: {:?}",
            result
        );
    });
}

// ============================================================================
// GenServer typed associated types (compile-time test)
// ============================================================================

/// GenServer trait requires typed Call, Cast, and Reply associated types.
#[test]
fn gen_server_has_typed_associated_types() {
    use rebar_core::gen_server::{
        CallReply, CastReply, From, GenServer, GenServerContext, InfoReply,
    };

    struct TestServer;

    impl GenServer for TestServer {
        type State = u64;
        type Call = String;
        type Cast = String;
        type Reply = String;

        async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
            Ok(0)
        }

        async fn handle_call(
            &self,
            _msg: Self::Call,
            _from: From,
            state: Self::State,
            _ctx: &GenServerContext,
        ) -> CallReply<Self::State> {
            CallReply::Reply(rmpv::Value::Nil, state)
        }

        async fn handle_cast(
            &self,
            _msg: Self::Cast,
            state: Self::State,
            _ctx: &GenServerContext,
        ) -> CastReply<Self::State> {
            CastReply::NoReply(state)
        }

        async fn handle_info(
            &self,
            _msg: rmpv::Value,
            state: Self::State,
            _ctx: &GenServerContext,
        ) -> InfoReply<Self::State> {
            InfoReply::NoReply(state)
        }
    }

    // If this compiles, the typed associated types contract holds.
    let _ = std::any::type_name::<TestServer>();
}

// ============================================================================
// DynamicSupervisorHandle is Clone
// ============================================================================

#[test]
fn dynamic_supervisor_handle_is_clone() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<rebar_core::supervisor::DynamicSupervisorHandle>();
}
