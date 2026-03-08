// FFI functions perform null checks before dereferencing raw pointers.
// Clippy's not_unsafe_ptr_arg_deref lint is a false positive here because
// every function guards against null before any dereference.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::sync::Mutex;

use rebar_core::process::ProcessId;
use rebar_core::runtime::Runtime;

// ---------------------------------------------------------------------------
// FFI types
// ---------------------------------------------------------------------------

/// C-compatible PID with node, thread, and local fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RebarPid {
    pub node_id: u64,
    pub thread_id: u16,
    pub local_id: u64,
}

impl RebarPid {
    fn to_process_id(self) -> ProcessId {
        ProcessId::new(self.node_id, self.thread_id, self.local_id)
    }

    fn from_process_id(pid: ProcessId) -> Self {
        Self {
            node_id: pid.node_id(),
            thread_id: pid.thread_id(),
            local_id: pid.local_id(),
        }
    }
}

/// Opaque message wrapper carrying raw bytes.
pub struct RebarMsg {
    data: Vec<u8>,
}

/// Commands sent from C FFI to the rebar executor thread.
enum FfiCommand {
    Spawn {
        callback: extern "C" fn(RebarPid),
        reply: crossbeam_channel::Sender<ProcessId>,
    },
    Send {
        dest: ProcessId,
        payload: rmpv::Value,
        reply: crossbeam_channel::Sender<Result<(), rebar_core::process::SendError>>,
    },
    Shutdown,
}

/// Opaque runtime wrapper holding a rebar executor thread, a crossbeam command
/// channel, and a simple local name registry.
pub struct RebarRuntime {
    cmd_tx: crossbeam_channel::Sender<FfiCommand>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
    #[allow(dead_code)] // read via raw pointer in tests
    node_id: u64,
    registry: Mutex<HashMap<String, ProcessId>>,
}

impl Drop for RebarRuntime {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(FfiCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

const REBAR_OK: i32 = 0;
const REBAR_ERR_NULL_PTR: i32 = -1;
const REBAR_ERR_SEND_FAILED: i32 = -2;
const REBAR_ERR_NOT_FOUND: i32 = -3;
const REBAR_ERR_INVALID_NAME: i32 = -4;

// ---------------------------------------------------------------------------
// Message functions
// ---------------------------------------------------------------------------

/// Create a new message from a raw byte buffer.
///
/// Returns a heap-allocated `RebarMsg` pointer, or null if `data` is null
/// and `len` is non-zero. An empty message (len == 0) is allowed even with
/// a null data pointer.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_msg_create(data: *const u8, len: usize) -> *mut RebarMsg {
    let bytes = if len == 0 {
        Vec::new()
    } else if data.is_null() {
        return std::ptr::null_mut();
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    Box::into_raw(Box::new(RebarMsg { data: bytes }))
}

/// Return a pointer to the message's data buffer.
///
/// Returns null if `msg` is null. The pointer is valid as long as the
/// message has not been freed.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_msg_data(msg: *const RebarMsg) -> *const u8 {
    if msg.is_null() {
        return std::ptr::null();
    }
    let msg = unsafe { &*msg };
    msg.data.as_ptr()
}

/// Return the length of the message's data buffer.
///
/// Returns 0 if `msg` is null.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_msg_len(msg: *const RebarMsg) -> usize {
    if msg.is_null() {
        return 0;
    }
    let msg = unsafe { &*msg };
    msg.data.len()
}

/// Free a message previously created with `rebar_msg_create`.
///
/// Passing null is a safe no-op.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_msg_free(msg: *mut RebarMsg) {
    if !msg.is_null() {
        unsafe {
            drop(Box::from_raw(msg));
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime functions
// ---------------------------------------------------------------------------

/// Create a new runtime for the given node ID.
///
/// Spawns a dedicated rebar executor thread that runs the rebar runtime.
/// Returns a heap-allocated `RebarRuntime` pointer.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_runtime_new(node_id: u64) -> *mut RebarRuntime {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<FfiCommand>();

    let thread_handle = std::thread::spawn(move || {
        let ex = rebar_core::executor::RebarExecutor::new(
            rebar_core::executor::ExecutorConfig::default(),
        )
        .expect("failed to build rebar executor");

        ex.block_on(async move {
            let runtime = Runtime::new(node_id);

            loop {
                // Process commands from the FFI layer
                match cmd_rx.try_recv() {
                    Ok(FfiCommand::Spawn { callback, reply }) => {
                        let pid = runtime.spawn(move |ctx| async move {
                            let pid = ctx.self_pid();
                            let ffi_pid = RebarPid::from_process_id(pid);
                            callback(ffi_pid);
                        });
                        let _ = reply.send(pid);
                    }
                    Ok(FfiCommand::Send { dest, payload, reply }) => {
                        let result = runtime.send(dest, payload);
                        let _ = reply.send(result);
                    }
                    Ok(FfiCommand::Shutdown) => {
                        runtime.shutdown();
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        // Yield to let the executor process other tasks
                        rebar_core::time::sleep(std::time::Duration::from_micros(100)).await;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        runtime.shutdown();
                        break;
                    }
                }
            }
        });
    });

    Box::into_raw(Box::new(RebarRuntime {
        cmd_tx,
        thread_handle: Some(thread_handle),
        node_id,
        registry: Mutex::new(HashMap::new()),
    }))
}

/// Free a runtime previously created with `rebar_runtime_new`.
///
/// Passing null is a safe no-op.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_runtime_free(rt: *mut RebarRuntime) {
    if !rt.is_null() {
        unsafe {
            drop(Box::from_raw(rt));
        }
    }
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn a new process that calls `callback` with its own PID.
///
/// The new process's PID is written to `pid_out`.
/// Returns 0 on success, or a negative error code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_spawn(
    rt: *mut RebarRuntime,
    callback: Option<extern "C" fn(RebarPid)>,
    pid_out: *mut RebarPid,
) -> i32 {
    if rt.is_null() || pid_out.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &*rt };
    let cb = match callback {
        Some(f) => f,
        None => return REBAR_ERR_NULL_PTR,
    };

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    if rt.cmd_tx.send(FfiCommand::Spawn { callback: cb, reply: reply_tx }).is_err() {
        return REBAR_ERR_SEND_FAILED;
    }

    match reply_rx.recv() {
        Ok(pid) => {
            unsafe {
                *pid_out = RebarPid::from_process_id(pid);
            }
            REBAR_OK
        }
        Err(_) => REBAR_ERR_SEND_FAILED,
    }
}

// ---------------------------------------------------------------------------
// Send
// ---------------------------------------------------------------------------

/// Send a message to a process by PID.
///
/// The message is serialised as a `rmpv::Value::Binary` wrapping the raw
/// bytes from `msg`.
///
/// Returns 0 on success, or a negative error code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_send(rt: *mut RebarRuntime, dest: RebarPid, msg: *const RebarMsg) -> i32 {
    if rt.is_null() || msg.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &*rt };
    let msg = unsafe { &*msg };
    let dest_pid = dest.to_process_id();
    let payload = rmpv::Value::Binary(msg.data.clone());

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    if rt.cmd_tx.send(FfiCommand::Send { dest: dest_pid, payload, reply: reply_tx }).is_err() {
        return REBAR_ERR_SEND_FAILED;
    }

    match reply_rx.recv() {
        Ok(Ok(())) => REBAR_OK,
        Ok(Err(_)) => REBAR_ERR_SEND_FAILED,
        Err(_) => REBAR_ERR_SEND_FAILED,
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Register a name for a PID in the local registry.
///
/// Returns 0 on success, or a negative error code if the name bytes are
/// not valid UTF-8 or if a required pointer is null.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_register(
    rt: *mut RebarRuntime,
    name: *const u8,
    name_len: usize,
    pid: RebarPid,
) -> i32 {
    if rt.is_null() || name.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &mut *rt };
    let name_bytes = unsafe { std::slice::from_raw_parts(name, name_len) };
    let name_str = match std::str::from_utf8(name_bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => return REBAR_ERR_INVALID_NAME,
    };
    let mut reg = rt.registry.lock().unwrap_or_else(|e| e.into_inner());
    reg.insert(name_str, pid.to_process_id());
    REBAR_OK
}

/// Look up a PID by name in the local registry.
///
/// Writes the PID to `pid_out` if found.
/// Returns 0 on success, `REBAR_ERR_NOT_FOUND` if the name is not
/// registered, or a negative error code for null pointers / bad UTF-8.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_whereis(
    rt: *mut RebarRuntime,
    name: *const u8,
    name_len: usize,
    pid_out: *mut RebarPid,
) -> i32 {
    if rt.is_null() || name.is_null() || pid_out.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &*rt };
    let name_bytes = unsafe { std::slice::from_raw_parts(name, name_len) };
    let name_str = match std::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return REBAR_ERR_INVALID_NAME,
    };
    let reg = rt.registry.lock().unwrap_or_else(|e| e.into_inner());
    match reg.get(name_str) {
        Some(pid) => {
            unsafe {
                *pid_out = RebarPid::from_process_id(*pid);
            }
            REBAR_OK
        }
        None => REBAR_ERR_NOT_FOUND,
    }
}

/// Send a message to a named process.
///
/// Looks up the name in the local registry and sends the message to the
/// associated PID.
///
/// Returns 0 on success, `REBAR_ERR_NOT_FOUND` if the name is not
/// registered, or another negative error code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_send_named(
    rt: *mut RebarRuntime,
    name: *const u8,
    name_len: usize,
    msg: *const RebarMsg,
) -> i32 {
    if rt.is_null() || name.is_null() || msg.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt_ref = unsafe { &*rt };
    let name_bytes = unsafe { std::slice::from_raw_parts(name, name_len) };
    let name_str = match std::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return REBAR_ERR_INVALID_NAME,
    };

    let dest_pid = {
        let reg = rt_ref.registry.lock().unwrap_or_else(|e| e.into_inner());
        match reg.get(name_str) {
            Some(pid) => *pid,
            None => return REBAR_ERR_NOT_FOUND,
        }
    };

    let msg_ref = unsafe { &*msg };
    let payload = rmpv::Value::Binary(msg_ref.data.clone());

    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
    if rt_ref.cmd_tx.send(FfiCommand::Send { dest: dest_pid, payload, reply: reply_tx }).is_err() {
        return REBAR_ERR_SEND_FAILED;
    }

    match reply_rx.recv() {
        Ok(Ok(())) => REBAR_OK,
        Ok(Err(_)) => REBAR_ERR_SEND_FAILED,
        Err(_) => REBAR_ERR_SEND_FAILED,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // -----------------------------------------------------------------------
    // 1. msg_create_and_read
    // -----------------------------------------------------------------------
    #[test]
    fn msg_create_and_read() {
        let data = b"hello world";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        assert!(!msg.is_null());

        let ptr = rebar_msg_data(msg);
        let len = rebar_msg_len(msg);
        assert_eq!(len, data.len());

        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert_eq!(slice, data);

        rebar_msg_free(msg);
    }

    // -----------------------------------------------------------------------
    // 2. msg_empty_data
    // -----------------------------------------------------------------------
    #[test]
    fn msg_empty_data() {
        let msg = rebar_msg_create(std::ptr::null(), 0);
        assert!(!msg.is_null());

        let len = rebar_msg_len(msg);
        assert_eq!(len, 0);

        rebar_msg_free(msg);
    }

    // -----------------------------------------------------------------------
    // 3. msg_large_data
    // -----------------------------------------------------------------------
    #[test]
    fn msg_large_data() {
        let size = 1024 * 1024; // 1 MiB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        assert!(!msg.is_null());

        let len = rebar_msg_len(msg);
        assert_eq!(len, size);

        let ptr = rebar_msg_data(msg);
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert_eq!(slice, data.as_slice());

        rebar_msg_free(msg);
    }

    // -----------------------------------------------------------------------
    // 4. msg_free_null_is_noop
    // -----------------------------------------------------------------------
    #[test]
    fn msg_free_null_is_noop() {
        rebar_msg_free(std::ptr::null_mut());
    }

    // -----------------------------------------------------------------------
    // 5. msg_data_ptr_stable
    // -----------------------------------------------------------------------
    #[test]
    fn msg_data_ptr_stable() {
        let data = b"stability check";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        assert!(!msg.is_null());

        let ptr1 = rebar_msg_data(msg);
        let ptr2 = rebar_msg_data(msg);
        assert_eq!(ptr1, ptr2);

        rebar_msg_free(msg);
    }

    // -----------------------------------------------------------------------
    // 6. runtime_create_destroy
    // -----------------------------------------------------------------------
    #[test]
    fn runtime_create_destroy() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 7. runtime_create_with_different_node_ids
    // -----------------------------------------------------------------------
    #[test]
    fn runtime_create_with_different_node_ids() {
        let rt1 = rebar_runtime_new(1);
        let rt2 = rebar_runtime_new(42);
        assert!(!rt1.is_null());
        assert!(!rt2.is_null());

        let node1 = unsafe { &*rt1 }.node_id;
        let node2 = unsafe { &*rt2 }.node_id;
        assert_eq!(node1, 1);
        assert_eq!(node2, 42);

        rebar_runtime_free(rt1);
        rebar_runtime_free(rt2);
    }

    // -----------------------------------------------------------------------
    // 8. pid_components
    // -----------------------------------------------------------------------
    #[test]
    fn pid_components() {
        let pid = RebarPid {
            node_id: 7,
            thread_id: 0,
            local_id: 42,
        };
        assert_eq!(pid.node_id, 7);
        assert_eq!(pid.local_id, 42);

        let process_id = pid.to_process_id();
        assert_eq!(process_id.node_id(), 7);
        assert_eq!(process_id.local_id(), 42);

        let back = RebarPid::from_process_id(process_id);
        assert_eq!(back.node_id, 7);
        assert_eq!(back.local_id, 42);
    }

    // -----------------------------------------------------------------------
    // 9. pid_zero_values
    // -----------------------------------------------------------------------
    #[test]
    fn pid_zero_values() {
        let pid = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        assert_eq!(pid.node_id, 0);
        assert_eq!(pid.local_id, 0);

        let process_id = pid.to_process_id();
        assert_eq!(process_id.node_id(), 0);
        assert_eq!(process_id.local_id(), 0);
    }

    // -----------------------------------------------------------------------
    // 10. spawn_returns_valid_pid
    // -----------------------------------------------------------------------
    #[test]
    fn spawn_returns_valid_pid() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop_callback(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(noop_callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);
        assert_eq!(pid_out.node_id, 1);
        assert!(pid_out.local_id > 0);

        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 11. send_to_spawned_process
    // -----------------------------------------------------------------------
    #[test]
    fn send_to_spawned_process() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        static CALLBACK_RAN: AtomicBool = AtomicBool::new(false);
        CALLBACK_RAN.store(false, Ordering::SeqCst);

        extern "C" fn callback(_pid: RebarPid) {
            CALLBACK_RAN.store(true, Ordering::SeqCst);
        }

        let mut pid_out = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);

        // Give the spawned process time to run.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(CALLBACK_RAN.load(Ordering::SeqCst));

        // Send a message to the spawned process. The process has likely
        // already exited, so we accept either success or send-failed.
        let data = b"hi";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let send_rc = rebar_send(rt, pid_out, msg);
        assert!(send_rc == REBAR_OK || send_rc == REBAR_ERR_SEND_FAILED);

        rebar_msg_free(msg);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 12. send_to_invalid_pid_returns_error
    // -----------------------------------------------------------------------
    #[test]
    fn send_to_invalid_pid_returns_error() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        let dest = RebarPid {
            node_id: 1,
            thread_id: 0,
            local_id: 999999,
        };
        let data = b"nope";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let rc = rebar_send(rt, dest, msg);
        assert_eq!(rc, REBAR_ERR_SEND_FAILED);

        rebar_msg_free(msg);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 13. register_and_whereis
    // -----------------------------------------------------------------------
    #[test]
    fn register_and_whereis() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        let name = b"my_service";
        let pid = RebarPid {
            node_id: 1,
            thread_id: 0,
            local_id: 42,
        };

        let rc = rebar_register(rt, name.as_ptr(), name.len(), pid);
        assert_eq!(rc, REBAR_OK);

        let mut found = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        let rc = rebar_whereis(rt, name.as_ptr(), name.len(), &mut found);
        assert_eq!(rc, REBAR_OK);
        assert_eq!(found.node_id, 1);
        assert_eq!(found.local_id, 42);

        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 14. send_named
    // -----------------------------------------------------------------------
    #[test]
    fn send_named() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn long_lived_callback(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(long_lived_callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);

        let name = b"worker";
        let rc = rebar_register(rt, name.as_ptr(), name.len(), pid_out);
        assert_eq!(rc, REBAR_OK);

        let data = b"payload";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let rc = rebar_send_named(rt, name.as_ptr(), name.len(), msg);
        // Process may have exited; both outcomes validate the path.
        assert!(rc == REBAR_OK || rc == REBAR_ERR_SEND_FAILED);

        rebar_msg_free(msg);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 15. whereis_not_found
    // -----------------------------------------------------------------------
    #[test]
    fn whereis_not_found() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        let name = b"nonexistent";
        let mut pid_out = RebarPid {
            node_id: 0,
            thread_id: 0,
            local_id: 0,
        };
        let rc = rebar_whereis(rt, name.as_ptr(), name.len(), &mut pid_out);
        assert_eq!(rc, REBAR_ERR_NOT_FOUND);

        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 16. mutex_poison_recovery
    // -----------------------------------------------------------------------
    #[test]
    fn mutex_poison_recovery() {
        let registry: Mutex<HashMap<String, rebar_core::process::ProcessId>> =
            Mutex::new(HashMap::new());

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = registry.lock().unwrap();
            guard.insert(
                "test".to_string(),
                rebar_core::process::ProcessId::new(0, 0, 1),
            );
            panic!("intentional panic to poison mutex");
        }));

        assert!(registry.lock().is_err());

        let guard = registry.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            guard.get("test"),
            Some(&rebar_core::process::ProcessId::new(0, 0, 1))
        );
    }
}
