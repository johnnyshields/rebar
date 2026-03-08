//! API contract regression tests for rebar-ffi.
//!
//! These tests guard the C-ABI surface that Go, Python, and TypeScript clients
//! depend on. A failing test here means a breaking change for downstream FFI
//! consumers.

use std::sync::atomic::{AtomicBool, Ordering};

use rebar_ffi::{
    rebar_msg_create, rebar_msg_data, rebar_msg_free, rebar_msg_len, rebar_register,
    rebar_runtime_free, rebar_runtime_new, rebar_spawn, rebar_whereis, RebarPid,
};

const REBAR_OK: i32 = 0;

/// Contract: RebarPid is a #[repr(C)] struct with three fields
/// (node_id: u64, thread_id: u16, local_id: u64) for v5's thread-per-core model.
#[test]
fn rebar_pid_has_three_fields() {
    let pid = RebarPid {
        node_id: 7,
        thread_id: 0,
        local_id: 42,
    };
    assert_eq!(pid.node_id, 7);
    assert_eq!(pid.thread_id, 0);
    assert_eq!(pid.local_id, 42);
}

/// Contract: RebarPid implements Copy. FFI clients pass it by value across the
/// C boundary; if Copy is removed the ABI silently breaks.
#[test]
fn rebar_pid_is_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<RebarPid>();
}

/// Contract: all core FFI symbols resolve and can be called without panicking.
/// This catches link-time regressions where a function is renamed, removed, or
/// its signature changes.
#[test]
fn ffi_functions_resolve() {
    // --- message functions ---
    let msg = rebar_msg_create(std::ptr::null(), 0);
    assert!(!msg.is_null(), "rebar_msg_create(null, 0) must return a valid empty message");

    let len = rebar_msg_len(msg);
    assert_eq!(len, 0);

    let data = rebar_msg_data(msg);
    let _ = data;

    rebar_msg_free(msg);

    // --- runtime functions ---
    let rt = rebar_runtime_new(1);
    assert!(!rt.is_null(), "rebar_runtime_new must return a valid runtime");
    rebar_runtime_free(rt);
}

/// Contract: rebar_spawn invokes the extern "C" callback and returns a PID
/// whose node_id matches the runtime's node.
#[test]
fn runtime_spawn_and_send_lifecycle() {
    static CALLBACK_RAN: AtomicBool = AtomicBool::new(false);

    extern "C" fn callback(_pid: RebarPid) {
        CALLBACK_RAN.store(true, Ordering::SeqCst);
    }

    CALLBACK_RAN.store(false, Ordering::SeqCst);

    let rt = rebar_runtime_new(5);
    assert!(!rt.is_null());

    let mut pid_out = RebarPid {
        node_id: 0,
        thread_id: 0,
        local_id: 0,
    };
    let rc = rebar_spawn(rt, Some(callback), &mut pid_out);
    assert_eq!(rc, REBAR_OK, "rebar_spawn must succeed");

    std::thread::sleep(std::time::Duration::from_millis(50));

    assert!(
        CALLBACK_RAN.load(Ordering::SeqCst),
        "extern \"C\" callback must have been invoked by the spawned process"
    );
    assert_eq!(
        pid_out.node_id, 5,
        "spawned PID node_id must match the runtime's node_id"
    );

    rebar_runtime_free(rt);
}

/// Contract: the local name registry survives normal register/whereis usage
/// and returns matching PID values.
#[test]
fn registry_works_after_normal_usage() {
    let rt = rebar_runtime_new(3);
    assert!(!rt.is_null());

    let name = b"my_actor";
    let registered_pid = RebarPid {
        node_id: 3,
        thread_id: 0,
        local_id: 99,
    };

    let rc = rebar_register(rt, name.as_ptr(), name.len(), registered_pid);
    assert_eq!(rc, REBAR_OK, "rebar_register must succeed");

    let mut found_pid = RebarPid {
        node_id: 0,
        thread_id: 0,
        local_id: 0,
    };
    let rc = rebar_whereis(rt, name.as_ptr(), name.len(), &mut found_pid);
    assert_eq!(rc, REBAR_OK, "rebar_whereis must find the registered name");
    assert_eq!(
        found_pid.node_id, registered_pid.node_id,
        "looked-up node_id must match registered value"
    );
    assert_eq!(
        found_pid.local_id, registered_pid.local_id,
        "looked-up local_id must match registered value"
    );

    rebar_runtime_free(rt);
}
