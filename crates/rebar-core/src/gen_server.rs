use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::process::{ExitReason, ProcessId, RegistryError, SendError};
use crate::router::MessageRouter;
use crate::runtime::{ProcessContext, Runtime};
use crate::supervisor::engine::ChildEntry;
use crate::supervisor::spec::ChildSpec;

// ---------------------------------------------------------------------------
// Reply types
// ---------------------------------------------------------------------------

pub enum CallReply<State> {
    Reply(rmpv::Value, State),
    NoReply(State),
    Stop(String, rmpv::Value, State),
}

pub enum CastReply<State> {
    NoReply(State),
    Stop(String, State),
}

pub enum InfoReply<State> {
    NoReply(State),
    Stop(String, State),
}

// ---------------------------------------------------------------------------
// From (deferred reply handle)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct From {
    pub pid: ProcessId,
    pub ref_id: u64,
}

// ---------------------------------------------------------------------------
// GenServerContext
// ---------------------------------------------------------------------------

pub struct GenServerContext {
    pid: ProcessId,
    router: Arc<dyn MessageRouter>,
}

impl GenServerContext {
    pub fn self_pid(&self) -> ProcessId {
        self.pid
    }

    pub fn cast(&self, dest: ProcessId, request: rmpv::Value) -> Result<(), SendError> {
        let envelope = rmpv::Value::Map(vec![
            (rmpv::Value::String("$gs".into()), rmpv::Value::String("cast".into())),
            (rmpv::Value::String("req".into()), request),
        ]);
        self.router.route(self.pid, dest, envelope)
    }

    pub fn send_info(&self, dest: ProcessId, msg: rmpv::Value) -> Result<(), SendError> {
        self.router.route(self.pid, dest, msg)
    }

    pub fn reply(&self, from: &From, response: rmpv::Value) -> Result<(), SendError> {
        let envelope = rmpv::Value::Map(vec![
            (rmpv::Value::String("$gs".into()), rmpv::Value::String("reply".into())),
            (rmpv::Value::String("ref".into()), rmpv::Value::Integer(from.ref_id.into())),
            (rmpv::Value::String("val".into()), response),
        ]);
        self.router.route(self.pid, from.pid, envelope)
    }
}

// ---------------------------------------------------------------------------
// GenServer trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait GenServer: Send + Sync + 'static {
    type State: Send + 'static;

    async fn init(&self, args: rmpv::Value, ctx: &GenServerContext) -> Result<Self::State, String>;

    async fn handle_call(
        &self,
        request: rmpv::Value,
        from: From,
        state: Self::State,
        ctx: &GenServerContext,
    ) -> CallReply<Self::State>;

    async fn handle_cast(
        &self,
        request: rmpv::Value,
        state: Self::State,
        ctx: &GenServerContext,
    ) -> CastReply<Self::State>;

    async fn handle_info(
        &self,
        _msg: rmpv::Value,
        state: Self::State,
        _ctx: &GenServerContext,
    ) -> InfoReply<Self::State> {
        InfoReply::NoReply(state)
    }

    async fn terminate(&self, _reason: &str, _state: Self::State) {}
}

// ---------------------------------------------------------------------------
// Wire-protocol helpers
// ---------------------------------------------------------------------------

fn extract_gs_type(value: &rmpv::Value) -> Option<&str> {
    if let rmpv::Value::Map(pairs) = value {
        for (k, v) in pairs {
            if let (rmpv::Value::String(key), rmpv::Value::String(val)) = (k, v) {
                if key.as_str() == Some("$gs") {
                    return val.as_str();
                }
            }
        }
    }
    None
}

fn extract_field(value: &rmpv::Value, field: &str) -> Option<rmpv::Value> {
    if let rmpv::Value::Map(pairs) = value {
        for (k, v) in pairs {
            if let rmpv::Value::String(key) = k {
                if key.as_str() == Some(field) {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Process loop
// ---------------------------------------------------------------------------

async fn gen_server_loop<S: GenServer>(
    server: Arc<S>,
    args: rmpv::Value,
    mut process_ctx: ProcessContext,
) -> ExitReason {
    let ctx = GenServerContext {
        pid: process_ctx.self_pid(),
        router: process_ctx.router().clone(),
    };

    let mut state = match server.init(args, &ctx).await {
        Ok(s) => s,
        Err(reason) => return ExitReason::Abnormal(reason),
    };

    while let Some(msg) = process_ctx.recv().await {
        let payload = msg.payload().clone();
        match extract_gs_type(&payload) {
            Some("call") => {
                let ref_id = extract_field(&payload, "ref")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let request = extract_field(&payload, "req").unwrap_or(rmpv::Value::Nil);
                let from = From { pid: msg.from(), ref_id };

                match server.handle_call(request, from.clone(), state, &ctx).await {
                    CallReply::Reply(response, new_state) => {
                        let _ = ctx.reply(&from, response);
                        state = new_state;
                    }
                    CallReply::NoReply(new_state) => {
                        state = new_state;
                    }
                    CallReply::Stop(reason, response, final_state) => {
                        let _ = ctx.reply(&from, response);
                        server.terminate(&reason, final_state).await;
                        return ExitReason::Normal;
                    }
                }
            }
            Some("cast") => {
                let request = extract_field(&payload, "req").unwrap_or(rmpv::Value::Nil);
                match server.handle_cast(request, state, &ctx).await {
                    CastReply::NoReply(new_state) => {
                        state = new_state;
                    }
                    CastReply::Stop(reason, final_state) => {
                        server.terminate(&reason, final_state).await;
                        return ExitReason::Normal;
                    }
                }
            }
            Some("reply") => {
                match server.handle_info(payload, state, &ctx).await {
                    InfoReply::NoReply(new_state) => state = new_state,
                    InfoReply::Stop(reason, final_state) => {
                        server.terminate(&reason, final_state).await;
                        return ExitReason::Normal;
                    }
                }
            }
            _ => {
                match server.handle_info(payload, state, &ctx).await {
                    InfoReply::NoReply(new_state) => state = new_state,
                    InfoReply::Stop(reason, final_state) => {
                        server.terminate(&reason, final_state).await;
                        return ExitReason::Normal;
                    }
                }
            }
        }
    }

    server.terminate("normal", state).await;
    ExitReason::Normal
}

// ---------------------------------------------------------------------------
// CallError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CallError {
    Timeout,
    SendFailed(SendError),
    ServerExited,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Timeout => write!(f, "call timed out"),
            CallError::SendFailed(e) => write!(f, "send failed: {}", e),
            CallError::ServerExited => write!(f, "server exited"),
        }
    }
}

impl std::error::Error for CallError {}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

static CALL_REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Spawn a GenServer as a rebar process.
pub async fn start<S: GenServer>(
    runtime: &Runtime,
    server: S,
    args: rmpv::Value,
) -> ProcessId {
    let server = Arc::new(server);
    runtime.spawn(move |ctx| async move {
        gen_server_loop(server, args, ctx).await;
    }).await
}

/// Spawn a named GenServer.
pub async fn start_named<S: GenServer>(
    runtime: &Runtime,
    name: String,
    server: S,
    args: rmpv::Value,
) -> Result<ProcessId, RegistryError> {
    let pid = start(runtime, server, args).await;
    runtime.register(name, pid)?;
    Ok(pid)
}

/// Synchronous call from outside any process (spawns a temporary process).
pub async fn call_from_runtime(
    runtime: &Runtime,
    dest: ProcessId,
    request: rmpv::Value,
    timeout: std::time::Duration,
) -> Result<rmpv::Value, CallError> {
    let ref_id = CALL_REF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = tokio::sync::oneshot::channel();

    runtime.spawn(move |mut ctx| async move {
        let envelope = rmpv::Value::Map(vec![
            (rmpv::Value::String("$gs".into()), rmpv::Value::String("call".into())),
            (rmpv::Value::String("ref".into()), rmpv::Value::Integer(ref_id.into())),
            (rmpv::Value::String("req".into()), request),
        ]);
        if let Err(e) = ctx.send(dest, envelope).await {
            let _ = tx.send(Err(CallError::SendFailed(e)));
            return;
        }

        loop {
            match ctx.recv_timeout(timeout).await {
                Some(msg) => {
                    let payload = msg.payload().clone();
                    if extract_gs_type(&payload) == Some("reply") {
                        if let Some(r) = extract_field(&payload, "ref").and_then(|v| v.as_u64()) {
                            if r == ref_id {
                                let val = extract_field(&payload, "val").unwrap_or(rmpv::Value::Nil);
                                let _ = tx.send(Ok(val));
                                return;
                            }
                        }
                    }
                }
                None => {
                    let _ = tx.send(Err(CallError::Timeout));
                    return;
                }
            }
        }
    }).await;

    rx.await.map_err(|_| CallError::ServerExited)?
}

/// Fire-and-forget cast from outside any process.
pub async fn cast_from_runtime(
    runtime: &Runtime,
    dest: ProcessId,
    request: rmpv::Value,
) -> Result<(), SendError> {
    let envelope = rmpv::Value::Map(vec![
        (rmpv::Value::String("$gs".into()), rmpv::Value::String("cast".into())),
        (rmpv::Value::String("req".into()), request),
    ]);
    runtime.send(dest, envelope).await
}

/// Reply to a From (for deferred replies outside the GenServer callbacks).
pub async fn reply_from_runtime(
    runtime: &Runtime,
    from: &From,
    response: rmpv::Value,
) -> Result<(), SendError> {
    let envelope = rmpv::Value::Map(vec![
        (rmpv::Value::String("$gs".into()), rmpv::Value::String("reply".into())),
        (rmpv::Value::String("ref".into()), rmpv::Value::Integer(from.ref_id.into())),
        (rmpv::Value::String("val".into()), response),
    ]);
    runtime.send(from.pid, envelope).await
}

/// Create a ChildEntry for supervision.
pub fn child_entry<S: GenServer + Clone>(
    runtime: Arc<Runtime>,
    server: S,
    args: rmpv::Value,
    spec: ChildSpec,
) -> ChildEntry {
    ChildEntry::new(spec, move || {
        let runtime = Arc::clone(&runtime);
        let server = server.clone();
        let args = args.clone();
        async move {
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            let server = Arc::new(server);
            runtime.spawn(move |ctx| async move {
                let reason = gen_server_loop(server, args, ctx).await;
                let _ = exit_tx.send(reason);
            }).await;
            match exit_rx.await {
                Ok(reason) => reason,
                Err(_) => ExitReason::Abnormal("process dropped without sending exit reason".into()),
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // -- Counter server used across most tests --

    #[derive(Clone)]
    struct CounterServer;

    #[async_trait]
    impl GenServer for CounterServer {
        type State = u64;

        async fn init(&self, args: rmpv::Value, _ctx: &GenServerContext) -> Result<u64, String> {
            Ok(args.as_u64().unwrap_or(0))
        }

        async fn handle_call(
            &self,
            request: rmpv::Value,
            _from: From,
            state: u64,
            _ctx: &GenServerContext,
        ) -> CallReply<u64> {
            match request.as_str() {
                Some("get") => CallReply::Reply(rmpv::Value::Integer(state.into()), state),
                Some("increment") => CallReply::Reply(rmpv::Value::Integer((state + 1).into()), state + 1),
                Some("stop") => CallReply::Stop("requested".into(), rmpv::Value::String("bye".into()), state),
                _ => CallReply::Reply(rmpv::Value::Nil, state),
            }
        }

        async fn handle_cast(
            &self,
            request: rmpv::Value,
            state: u64,
            _ctx: &GenServerContext,
        ) -> CastReply<u64> {
            match request.as_str() {
                Some("increment") => CastReply::NoReply(state + 1),
                Some("stop") => CastReply::Stop("cast_stop".into(), state),
                _ => CastReply::NoReply(state),
            }
        }
    }

    fn new_runtime() -> Runtime {
        Runtime::new(1)
    }

    // 1. basic_call
    #[tokio::test]
    async fn basic_call() {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("increment".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_u64().unwrap(), 1);
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("increment".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_u64().unwrap(), 2);
    }

    // 2. basic_cast
    #[tokio::test]
    async fn basic_cast() {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).await.unwrap();
        // Give the cast time to process
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("get".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_u64().unwrap(), 1);
    }

    // 3. call_timeout
    #[tokio::test]
    async fn call_timeout() {
        let rt = new_runtime();
        // Spawn a process that never processes messages (blocks forever)
        let pid = rt.spawn(|mut ctx| async move {
            loop {
                ctx.recv().await;
                // Read but never reply
            }
        }).await;
        let result = call_from_runtime(&rt, pid, rmpv::Value::String("hello".into()), Duration::from_millis(100)).await;
        assert!(matches!(result, Err(CallError::Timeout)));
    }

    // 4. handle_info
    #[tokio::test]
    async fn handle_info() {
        struct InfoServer {
            tx: tokio::sync::mpsc::Sender<rmpv::Value>,
        }

        #[async_trait]
        impl GenServer for InfoServer {
            type State = ();
            async fn init(&self, _args: rmpv::Value, _ctx: &GenServerContext) -> Result<(), String> {
                Ok(())
            }
            async fn handle_call(&self, _req: rmpv::Value, _from: From, state: (), _ctx: &GenServerContext) -> CallReply<()> {
                CallReply::Reply(rmpv::Value::Nil, state)
            }
            async fn handle_cast(&self, _req: rmpv::Value, state: (), _ctx: &GenServerContext) -> CastReply<()> {
                CastReply::NoReply(state)
            }
            async fn handle_info(&self, msg: rmpv::Value, state: (), _ctx: &GenServerContext) -> InfoReply<()> {
                let _ = self.tx.send(msg).await;
                InfoReply::NoReply(state)
            }
        }

        let rt = new_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let pid = start(&rt, InfoServer { tx }, rmpv::Value::Nil).await;

        // Send a raw message (no $gs envelope) — should go to handle_info
        rt.send(pid, rmpv::Value::String("raw_info".into())).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
        assert_eq!(received.as_str().unwrap(), "raw_info");
    }

    // 5. stop_from_call
    #[tokio::test]
    async fn stop_from_call() {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("stop".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_str().unwrap(), "bye");
        // Process should exit
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
    }

    // 6. stop_from_cast
    #[tokio::test]
    async fn stop_from_cast() {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;
        cast_from_runtime(&rt, pid, rmpv::Value::String("stop".into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
    }

    // 7. named_start
    #[tokio::test]
    async fn named_start() {
        let rt = new_runtime();
        let pid = start_named(&rt, "my_counter".into(), CounterServer, rmpv::Value::Integer(0u64.into())).await.unwrap();
        assert_eq!(rt.whereis("my_counter"), Some(pid));
    }

    // 8. concurrent_callers
    #[tokio::test]
    async fn concurrent_callers() {
        let rt = Arc::new(new_runtime());
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;

        let mut handles = Vec::new();
        for _ in 0..10 {
            let rt = Arc::clone(&rt);
            handles.push(tokio::spawn(async move {
                call_from_runtime(&rt, pid, rmpv::Value::String("increment".into()), Duration::from_secs(2)).await
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            let val = h.await.unwrap().unwrap();
            results.push(val.as_u64().unwrap());
        }
        // Each caller should get a unique incremented value, all in 1..=10
        results.sort();
        assert_eq!(results.len(), 10);
        assert_eq!(*results.first().unwrap(), 1);
        assert_eq!(*results.last().unwrap(), 10);
    }

    // 9. deferred_reply
    #[tokio::test]
    async fn deferred_reply() {
        struct DeferredServer;

        #[async_trait]
        impl GenServer for DeferredServer {
            type State = Option<From>;

            async fn init(&self, _args: rmpv::Value, _ctx: &GenServerContext) -> Result<Option<From>, String> {
                Ok(None)
            }

            async fn handle_call(
                &self,
                request: rmpv::Value,
                from: From,
                _state: Option<From>,
                _ctx: &GenServerContext,
            ) -> CallReply<Option<From>> {
                match request.as_str() {
                    Some("deferred") => {
                        // Store from, reply later via handle_info
                        CallReply::NoReply(Some(from))
                    }
                    _ => CallReply::Reply(rmpv::Value::Nil, None),
                }
            }

            async fn handle_cast(&self, _req: rmpv::Value, state: Option<From>, _ctx: &GenServerContext) -> CastReply<Option<From>> {
                CastReply::NoReply(state)
            }

            async fn handle_info(
                &self,
                msg: rmpv::Value,
                state: Option<From>,
                ctx: &GenServerContext,
            ) -> InfoReply<Option<From>> {
                if msg.as_str() == Some("complete") {
                    if let Some(ref from) = state {
                        let _ = ctx.reply(from, rmpv::Value::String("deferred_result".into()));
                    }
                }
                InfoReply::NoReply(None)
            }
        }

        let rt = Arc::new(Runtime::new(1));
        let pid = start(&rt, DeferredServer, rmpv::Value::Nil).await;

        let rt_call = Arc::clone(&rt);
        let call_handle = tokio::spawn(async move {
            call_from_runtime(&rt_call, pid, rmpv::Value::String("deferred".into()), Duration::from_secs(2)).await
        });

        // Give the call time to reach the server
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Trigger completion via raw info message
        rt.send(pid, rmpv::Value::String("complete".into())).await.unwrap();

        let result = call_handle.await.unwrap().unwrap();
        assert_eq!(result.as_str().unwrap(), "deferred_result");
    }

    // 10. cast_from_runtime_test
    #[tokio::test]
    async fn cast_from_runtime_test() {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(10u64.into())).await;
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).await.unwrap();
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("get".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_u64().unwrap(), 12);
    }

    // 11. init_failure
    #[tokio::test]
    async fn init_failure() {
        struct FailServer;

        #[async_trait]
        impl GenServer for FailServer {
            type State = ();
            async fn init(&self, _args: rmpv::Value, _ctx: &GenServerContext) -> Result<(), String> {
                Err("init failed".into())
            }
            async fn handle_call(&self, _req: rmpv::Value, _from: From, state: (), _ctx: &GenServerContext) -> CallReply<()> {
                CallReply::Reply(rmpv::Value::Nil, state)
            }
            async fn handle_cast(&self, _req: rmpv::Value, state: (), _ctx: &GenServerContext) -> CastReply<()> {
                CastReply::NoReply(state)
            }
        }

        let rt = new_runtime();
        let pid = start(&rt, FailServer, rmpv::Value::Nil).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
    }

    // 12. gen_server_context_self_pid
    #[tokio::test]
    async fn gen_server_context_self_pid() {
        struct PidReporter;

        #[async_trait]
        impl GenServer for PidReporter {
            type State = ProcessId;
            async fn init(&self, _args: rmpv::Value, ctx: &GenServerContext) -> Result<ProcessId, String> {
                Ok(ctx.self_pid())
            }
            async fn handle_call(&self, _req: rmpv::Value, _from: From, state: ProcessId, _ctx: &GenServerContext) -> CallReply<ProcessId> {
                let pid_val = rmpv::Value::Array(vec![
                    rmpv::Value::Integer(state.node_id().into()),
                    rmpv::Value::Integer(state.local_id().into()),
                ]);
                CallReply::Reply(pid_val, state)
            }
            async fn handle_cast(&self, _req: rmpv::Value, state: ProcessId, _ctx: &GenServerContext) -> CastReply<ProcessId> {
                CastReply::NoReply(state)
            }
        }

        let rt = new_runtime();
        let pid = start(&rt, PidReporter, rmpv::Value::Nil).await;
        let reply = call_from_runtime(&rt, pid, rmpv::Value::Nil, Duration::from_secs(1)).await.unwrap();
        if let rmpv::Value::Array(arr) = reply {
            let node = arr[0].as_u64().unwrap();
            let local = arr[1].as_u64().unwrap();
            assert_eq!(node, pid.node_id());
            assert_eq!(local, pid.local_id());
        } else {
            panic!("expected array reply");
        }
    }

    // 13. call_error_display
    #[test]
    fn call_error_display() {
        assert_eq!(format!("{}", CallError::Timeout), "call timed out");
        assert_eq!(format!("{}", CallError::ServerExited), "server exited");
        let send_err = SendError::ProcessDead(ProcessId::new(1, 5));
        let display = format!("{}", CallError::SendFailed(send_err));
        assert!(display.contains("send failed"));
    }

    // 14. gen_server_is_send_sync
    #[test]
    fn gen_server_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CounterServer>();
    }

    // 15. child_entry_creates_supervised
    #[test]
    fn child_entry_creates_supervised() {
        let rt = Arc::new(Runtime::new(1));
        let spec = ChildSpec::new("counter");
        let entry = child_entry(rt, CounterServer, rmpv::Value::Integer(0u64.into()), spec);
        assert_eq!(entry.spec.id, "counter");
    }

    // 16. raw_malformed_call_envelope
    #[tokio::test]
    async fn raw_malformed_call_envelope() {
        // Send a $gs:call envelope missing the "ref" field directly to the server.
        // The server should still process it (ref_id defaults to 0).
        let rt = new_runtime();
        let pid = start(&rt, CounterServer, rmpv::Value::Integer(0u64.into())).await;

        // Manually construct a call envelope without "ref"
        let malformed = rmpv::Value::Map(vec![
            (rmpv::Value::String("$gs".into()), rmpv::Value::String("call".into())),
            (rmpv::Value::String("req".into()), rmpv::Value::String("increment".into())),
        ]);
        rt.send(pid, malformed).await.unwrap();

        // The server should process the call (increment) but the reply goes to PID(node,0)
        // which is the runtime's synthetic sender. The server should NOT crash.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify server is still alive and state was updated
        let reply = call_from_runtime(&rt, pid, rmpv::Value::String("get".into()), Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply.as_u64().unwrap(), 1);
    }

    // 17. child_entry_propagates_abnormal_exit
    #[tokio::test]
    async fn child_entry_propagates_abnormal_exit() {
        #[derive(Clone)]
        struct FailInitServer;

        #[async_trait]
        impl GenServer for FailInitServer {
            type State = ();
            async fn init(&self, _args: rmpv::Value, _ctx: &GenServerContext) -> Result<(), String> {
                Err("init failed".into())
            }
            async fn handle_call(&self, _req: rmpv::Value, _from: From, s: (), _ctx: &GenServerContext) -> CallReply<()> {
                CallReply::Reply(rmpv::Value::Nil, s)
            }
            async fn handle_cast(&self, _req: rmpv::Value, s: (), _ctx: &GenServerContext) -> CastReply<()> {
                CastReply::NoReply(s)
            }
        }

        let rt = Arc::new(Runtime::new(1));
        let spec = ChildSpec::new("fail_init");
        let entry = child_entry(Arc::clone(&rt), FailInitServer, rmpv::Value::Nil, spec);

        // Run the factory directly
        let reason = (entry.factory)().await;
        assert!(matches!(reason, ExitReason::Abnormal(ref msg) if msg == "init failed"),
            "expected Abnormal(\"init failed\"), got {:?}", reason);
    }

    // 18. child_entry_propagates_normal_exit
    #[tokio::test]
    async fn child_entry_propagates_normal_exit() {
        #[derive(Clone)]
        struct QuickStopServer;

        #[async_trait]
        impl GenServer for QuickStopServer {
            type State = ();
            async fn init(&self, _args: rmpv::Value, _ctx: &GenServerContext) -> Result<(), String> {
                Ok(())
            }
            async fn handle_call(&self, _req: rmpv::Value, _from: From, s: (), _ctx: &GenServerContext) -> CallReply<()> {
                CallReply::Reply(rmpv::Value::Nil, s)
            }
            async fn handle_cast(&self, request: rmpv::Value, s: (), _ctx: &GenServerContext) -> CastReply<()> {
                if request.as_str() == Some("stop") {
                    CastReply::Stop("done".into(), s)
                } else {
                    CastReply::NoReply(s)
                }
            }
        }

        let rt = Arc::new(Runtime::new(1));
        let spec = ChildSpec::new("quick_stop");
        let entry = child_entry(Arc::clone(&rt), QuickStopServer, rmpv::Value::Nil, spec);
        let factory = entry.factory.clone();

        let handle = tokio::spawn(async move {
            factory().await
        });

        // Wait for the server to start, then find and stop it
        tokio::time::sleep(Duration::from_millis(100)).await;
        let pids = rt.list_processes();
        if let Some(&pid) = pids.last() {
            let _ = cast_from_runtime(&rt, pid, rmpv::Value::String("stop".into())).await;
        }

        let reason = tokio::time::timeout(Duration::from_secs(2), handle)
            .await.expect("should complete").expect("should not panic");
        assert!(matches!(reason, ExitReason::Normal));
    }
}
