use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use rebar_core::process::mailbox::{Mailbox, MailboxRx};
use rebar_core::process::table::ProcessHandle;
use rebar_core::process::ProcessId;
use rebar_core::runtime::Runtime;

// ---------------------------------------------------------------------------
// FFI types
// ---------------------------------------------------------------------------

/// C-compatible PID with two u64 fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RebarPid {
    pub node_id: u64,
    pub local_id: u64,
}

impl RebarPid {
    fn to_process_id(self) -> ProcessId {
        ProcessId::new(self.node_id, self.local_id)
    }

    fn from_process_id(pid: ProcessId) -> Self {
        Self {
            node_id: pid.node_id(),
            local_id: pid.local_id(),
        }
    }
}

/// Opaque message wrapper carrying raw bytes.
pub struct RebarMsg {
    data: Vec<u8>,
}

/// Opaque runtime wrapper holding both a tokio runtime and the rebar runtime,
/// plus a simple local name registry.
pub struct RebarRuntime {
    tokio_rt: tokio::runtime::Runtime,
    runtime: Runtime,
    registry: Mutex<HashMap<String, ProcessId>>,
    mailboxes: Mutex<HashMap<ProcessId, MailboxRx>>,
    stop_senders: Mutex<HashMap<ProcessId, tokio::sync::oneshot::Sender<()>>>,
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

const REBAR_OK: i32 = 0;
const REBAR_ERR_NULL_PTR: i32 = -1;
const REBAR_ERR_SEND_FAILED: i32 = -2;
const REBAR_ERR_NOT_FOUND: i32 = -3;
const REBAR_ERR_INVALID_NAME: i32 = -4;
const REBAR_ERR_TIMEOUT: i32 = -5;

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
/// Returns a heap-allocated `RebarRuntime` pointer, or null if the tokio
/// runtime fails to build (should not happen under normal conditions).
#[unsafe(no_mangle)]
pub extern "C" fn rebar_runtime_new(node_id: u64) -> *mut RebarRuntime {
    let tokio_rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return std::ptr::null_mut(),
    };
    let runtime = Runtime::new(node_id);
    Box::into_raw(Box::new(RebarRuntime {
        tokio_rt,
        runtime,
        registry: Mutex::new(HashMap::new()),
        mailboxes: Mutex::new(HashMap::new()),
        stop_senders: Mutex::new(HashMap::new()),
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
/// The process stays alive in the process table until `rebar_stop_process`
/// is called. Messages can be received via `rebar_recv`.
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

    // Manually allocate PID and create mailbox so we can retain the
    // MailboxRx for polling via rebar_recv.
    let table = rt.runtime.table();
    let pid = table.allocate_pid();
    let (tx, rx) = Mailbox::unbounded();
    let handle = ProcessHandle::new(tx);
    table.insert(pid, handle);

    // Store the receiver so rebar_recv can use it.
    rt.mailboxes.lock().unwrap().insert(pid, rx);

    // Create a oneshot channel to keep the process alive until stopped.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    rt.stop_senders.lock().unwrap().insert(pid, stop_tx);

    // Spawn a keep-alive task that holds the process in the table.
    let table_clone = std::sync::Arc::clone(table);
    rt.tokio_rt.spawn(async move {
        let _ = stop_rx.await;
        table_clone.remove(&pid);
    });

    // Call the init callback synchronously.
    let ffi_pid = RebarPid::from_process_id(pid);
    cb(ffi_pid);

    unsafe {
        *pid_out = ffi_pid;
    }
    REBAR_OK
}

// ---------------------------------------------------------------------------
// Recv
// ---------------------------------------------------------------------------

/// Receive a message from a process's mailbox.
///
/// `timeout_ms` controls blocking behavior:
/// - `0`: non-blocking (return immediately if no message)
/// - `> 0`: block up to `timeout_ms` milliseconds
/// - `< 0`: block indefinitely until a message arrives
///
/// On success, writes a heap-allocated `RebarMsg` pointer to `msg_out`
/// and returns `REBAR_OK`. The caller must free the message with
/// `rebar_msg_free`.
///
/// Returns `REBAR_ERR_TIMEOUT` if no message is available within the
/// timeout, or `REBAR_ERR_NOT_FOUND` if the PID has no mailbox.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_recv(
    rt: *mut RebarRuntime,
    pid: RebarPid,
    msg_out: *mut *mut RebarMsg,
    timeout_ms: i64,
) -> i32 {
    if rt.is_null() || msg_out.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &*rt };
    let process_id = pid.to_process_id();

    // Take the MailboxRx out of the map so we can mutably borrow it.
    let mut rx = match rt.mailboxes.lock().unwrap().remove(&process_id) {
        Some(rx) => rx,
        None => return REBAR_ERR_NOT_FOUND,
    };

    let result = if timeout_ms == 0 {
        // Non-blocking: try_recv
        rx.try_recv()
    } else {
        // Blocking: use tokio runtime
        rt.tokio_rt.block_on(async {
            if timeout_ms < 0 {
                rx.recv().await
            } else {
                rx.recv_timeout(Duration::from_millis(timeout_ms as u64)).await
            }
        })
    };

    // Put the MailboxRx back.
    rt.mailboxes.lock().unwrap().insert(process_id, rx);

    match result {
        Some(msg) => {
            // Extract payload bytes.
            let bytes = match msg.payload() {
                rmpv::Value::Binary(b) => b.clone(),
                other => { let mut buf = Vec::new(); rmpv::encode::write_value(&mut buf, other).unwrap_or(()); buf },
            };
            let out = Box::into_raw(Box::new(RebarMsg { data: bytes }));
            unsafe {
                *msg_out = out;
            }
            REBAR_OK
        }
        None => REBAR_ERR_TIMEOUT,
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

    let result = rt.tokio_rt.block_on(async { rt.runtime.send(dest_pid, payload).await });

    match result {
        Ok(()) => REBAR_OK,
        Err(_) => REBAR_ERR_SEND_FAILED,
    }
}

// ---------------------------------------------------------------------------
// Stop Process
// ---------------------------------------------------------------------------

/// Stop a process, removing it from the process table.
///
/// After this call, the process can no longer receive messages and its
/// mailbox is dropped. Returns `REBAR_OK` on success or
/// `REBAR_ERR_NOT_FOUND` if the PID is not found.
#[unsafe(no_mangle)]
pub extern "C" fn rebar_stop_process(rt: *mut RebarRuntime, pid: RebarPid) -> i32 {
    if rt.is_null() {
        return REBAR_ERR_NULL_PTR;
    }
    let rt = unsafe { &*rt };
    let process_id = pid.to_process_id();

    // Remove the mailbox receiver.
    rt.mailboxes.lock().unwrap().remove(&process_id);

    // Send the stop signal to remove from process table.
    match rt.stop_senders.lock().unwrap().remove(&process_id) {
        Some(tx) => {
            let _ = tx.send(());
            REBAR_OK
        }
        None => REBAR_ERR_NOT_FOUND,
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
    let mut reg = rt.registry.lock().unwrap();
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
    let reg = rt.registry.lock().unwrap();
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
        let reg = rt_ref.registry.lock().unwrap();
        match reg.get(name_str) {
            Some(pid) => *pid,
            None => return REBAR_ERR_NOT_FOUND,
        }
    };

    let msg_ref = unsafe { &*msg };
    let payload = rmpv::Value::Binary(msg_ref.data.clone());

    let result = rt_ref.tokio_rt.block_on(async { rt_ref.runtime.send(dest_pid, payload).await });

    match result {
        Ok(()) => REBAR_OK,
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
        let size = 1024 * 1024;
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

        let node1 = unsafe { &*rt1 }.runtime.node_id();
        let node2 = unsafe { &*rt2 }.runtime.node_id();
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
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(noop_callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);
        assert_eq!(pid_out.node_id, 1);
        assert!(pid_out.local_id > 0);

        rebar_stop_process(rt, pid_out);
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
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);
        assert!(CALLBACK_RAN.load(Ordering::SeqCst));

        let data = b"hi";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let send_rc = rebar_send(rt, pid_out, msg);
        assert_eq!(send_rc, REBAR_OK);

        rebar_msg_free(msg);
        rebar_stop_process(rt, pid_out);
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

        extern "C" fn noop(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(noop), &mut pid_out);
        assert_eq!(rc, REBAR_OK);

        let name = b"my_service";
        let rc = rebar_register(rt, name.as_ptr(), name.len(), pid_out);
        assert_eq!(rc, REBAR_OK);

        let mut found = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        let rc = rebar_whereis(rt, name.as_ptr(), name.len(), &mut found);
        assert_eq!(rc, REBAR_OK);
        assert_eq!(found.node_id, pid_out.node_id);
        assert_eq!(found.local_id, pid_out.local_id);

        rebar_stop_process(rt, pid_out);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 14. send_named
    // -----------------------------------------------------------------------
    #[test]
    fn send_named() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop_callback(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(noop_callback), &mut pid_out);
        assert_eq!(rc, REBAR_OK);

        let name = b"worker";
        let rc = rebar_register(rt, name.as_ptr(), name.len(), pid_out);
        assert_eq!(rc, REBAR_OK);

        let data = b"payload";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let rc = rebar_send_named(rt, name.as_ptr(), name.len(), msg);
        assert_eq!(rc, REBAR_OK);

        rebar_msg_free(msg);
        rebar_stop_process(rt, pid_out);
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
            local_id: 0,
        };
        let rc = rebar_whereis(rt, name.as_ptr(), name.len(), &mut pid_out);
        assert_eq!(rc, REBAR_ERR_NOT_FOUND);

        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 16. recv_after_send
    // -----------------------------------------------------------------------
    #[test]
    fn recv_after_send() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        let rc = rebar_spawn(rt, Some(noop), &mut pid_out);
        assert_eq!(rc, REBAR_OK);

        let data = b"hello recv";
        let msg = rebar_msg_create(data.as_ptr(), data.len());
        let rc = rebar_send(rt, pid_out, msg);
        assert_eq!(rc, REBAR_OK);
        rebar_msg_free(msg);

        let mut msg_out: *mut RebarMsg = std::ptr::null_mut();
        let rc = rebar_recv(rt, pid_out, &mut msg_out, 0);
        assert_eq!(rc, REBAR_OK);
        assert!(!msg_out.is_null());

        let len = rebar_msg_len(msg_out);
        let ptr = rebar_msg_data(msg_out);
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert_eq!(slice, b"hello recv");

        rebar_msg_free(msg_out);
        rebar_stop_process(rt, pid_out);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 17. recv_nonblocking_returns_timeout
    // -----------------------------------------------------------------------
    #[test]
    fn recv_nonblocking_returns_timeout() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        rebar_spawn(rt, Some(noop), &mut pid_out);

        let mut msg_out: *mut RebarMsg = std::ptr::null_mut();
        let rc = rebar_recv(rt, pid_out, &mut msg_out, 0);
        assert_eq!(rc, REBAR_ERR_TIMEOUT);
        assert!(msg_out.is_null());

        rebar_stop_process(rt, pid_out);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 18. recv_timeout_expires
    // -----------------------------------------------------------------------
    #[test]
    fn recv_timeout_expires() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        rebar_spawn(rt, Some(noop), &mut pid_out);

        let mut msg_out: *mut RebarMsg = std::ptr::null_mut();
        let rc = rebar_recv(rt, pid_out, &mut msg_out, 10);
        assert_eq!(rc, REBAR_ERR_TIMEOUT);

        rebar_stop_process(rt, pid_out);
        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 19. stop_process
    // -----------------------------------------------------------------------
    #[test]
    fn stop_process() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        extern "C" fn noop(_pid: RebarPid) {}

        let mut pid_out = RebarPid {
            node_id: 0,
            local_id: 0,
        };
        rebar_spawn(rt, Some(noop), &mut pid_out);

        let rc = rebar_stop_process(rt, pid_out);
        assert_eq!(rc, REBAR_OK);

        let rc = rebar_stop_process(rt, pid_out);
        assert_eq!(rc, REBAR_ERR_NOT_FOUND);

        rebar_runtime_free(rt);
    }

    // -----------------------------------------------------------------------
    // 20. recv_invalid_pid_returns_not_found
    // -----------------------------------------------------------------------
    #[test]
    fn recv_invalid_pid_returns_not_found() {
        let rt = rebar_runtime_new(1);
        assert!(!rt.is_null());

        let bad_pid = RebarPid {
            node_id: 1,
            local_id: 999999,
        };
        let mut msg_out: *mut RebarMsg = std::ptr::null_mut();
        let rc = rebar_recv(rt, bad_pid, &mut msg_out, 0);
        assert_eq!(rc, REBAR_ERR_NOT_FOUND);

        rebar_runtime_free(rt);
    }
}
