use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::process::mailbox::Mailbox;
use crate::process::table::{ProcessHandle, ProcessTable};
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
    router: Rc<dyn MessageRouter>,
}

impl GenServerContext {
    pub fn self_pid(&self) -> ProcessId {
        self.pid
    }

    pub fn cast(&self, dest: ProcessId, request: rmpv::Value) -> Result<(), SendError> {
        self.router.route(self.pid, dest, cast_envelope(request))
    }

    pub fn send_info(&self, dest: ProcessId, msg: rmpv::Value) -> Result<(), SendError> {
        self.router.route(self.pid, dest, msg)
    }

    pub fn reply(&self, from: &From, response: rmpv::Value) -> Result<(), SendError> {
        self.router
            .route(self.pid, from.pid, reply_envelope(from.ref_id, response))
    }
}

// ---------------------------------------------------------------------------
// GenServer trait (using RPITIT, no async_trait needed)
// ---------------------------------------------------------------------------

pub trait GenServer: 'static {
    type State: 'static;
    type Call: 'static + Into<rmpv::Value> + TryFrom<rmpv::Value>;
    type Cast: 'static + Into<rmpv::Value> + TryFrom<rmpv::Value>;
    type Reply: 'static + Into<rmpv::Value> + TryFrom<rmpv::Value>;

    fn init(
        &self,
        ctx: &GenServerContext,
    ) -> impl std::future::Future<Output = Result<Self::State, String>>;

    fn handle_call(
        &self,
        msg: Self::Call,
        from: From,
        state: Self::State,
        ctx: &GenServerContext,
    ) -> impl std::future::Future<Output = CallReply<Self::State>>;

    fn handle_cast(
        &self,
        msg: Self::Cast,
        state: Self::State,
        ctx: &GenServerContext,
    ) -> impl std::future::Future<Output = CastReply<Self::State>>;

    fn handle_info(
        &self,
        _msg: rmpv::Value,
        state: Self::State,
        _ctx: &GenServerContext,
    ) -> impl std::future::Future<Output = InfoReply<Self::State>> {
        async { InfoReply::NoReply(state) }
    }

    fn terminate(
        &self,
        _reason: &str,
        _state: Self::State,
    ) -> impl std::future::Future<Output = ()> {
        async {}
    }
}

// ---------------------------------------------------------------------------
// Wire-protocol helpers
// ---------------------------------------------------------------------------

fn extract_gs_type(value: &rmpv::Value) -> Option<&str> {
    if let rmpv::Value::Map(pairs) = value {
        for (k, v) in pairs {
            if let (rmpv::Value::String(key), rmpv::Value::String(val)) = (k, v)
                && key.as_str() == Some("$gs")
            {
                return val.as_str();
            }
        }
    }
    None
}

fn extract_field(value: &rmpv::Value, field: &str) -> Option<rmpv::Value> {
    if let rmpv::Value::Map(pairs) = value {
        for (k, v) in pairs {
            if let rmpv::Value::String(key) = k
                && key.as_str() == Some(field)
            {
                return Some(v.clone());
            }
        }
    }
    None
}

fn call_envelope(ref_id: u64, request: rmpv::Value) -> rmpv::Value {
    rmpv::Value::Map(vec![
        (
            rmpv::Value::String("$gs".into()),
            rmpv::Value::String("call".into()),
        ),
        (
            rmpv::Value::String("ref".into()),
            rmpv::Value::Integer(ref_id.into()),
        ),
        (rmpv::Value::String("req".into()), request),
    ])
}

fn cast_envelope(request: rmpv::Value) -> rmpv::Value {
    rmpv::Value::Map(vec![
        (
            rmpv::Value::String("$gs".into()),
            rmpv::Value::String("cast".into()),
        ),
        (rmpv::Value::String("req".into()), request),
    ])
}

fn reply_envelope(ref_id: u64, response: rmpv::Value) -> rmpv::Value {
    rmpv::Value::Map(vec![
        (
            rmpv::Value::String("$gs".into()),
            rmpv::Value::String("reply".into()),
        ),
        (
            rmpv::Value::String("ref".into()),
            rmpv::Value::Integer(ref_id.into()),
        ),
        (rmpv::Value::String("val".into()), response),
    ])
}

// ---------------------------------------------------------------------------
// Process loop
// ---------------------------------------------------------------------------

async fn gen_server_loop<S: GenServer>(
    server: Rc<S>,
    mut process_ctx: ProcessContext,
) -> ExitReason {
    let ctx = GenServerContext {
        pid: process_ctx.self_pid(),
        router: process_ctx.router().clone(),
    };

    let mut state = match server.init(&ctx).await {
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
                let from = From {
                    pid: msg.from(),
                    ref_id,
                };
                if ref_id == 0 {
                    let _ = ctx.reply(
                        &from,
                        rmpv::Value::String("bad_call: missing ref_id".into()),
                    );
                    continue;
                }
                let raw_request = extract_field(&payload, "req").unwrap_or(rmpv::Value::Nil);

                let call_msg = match S::Call::try_from(raw_request) {
                    Ok(m) => m,
                    Err(_) => {
                        // Conversion failed — send error reply
                        let _ = ctx.reply(
                            &from,
                            rmpv::Value::String("bad_call: could not decode message".into()),
                        );
                        continue;
                    }
                };

                match server.handle_call(call_msg, from.clone(), state, &ctx).await {
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
                let raw_request = extract_field(&payload, "req").unwrap_or(rmpv::Value::Nil);
                let cast_msg = match S::Cast::try_from(raw_request) {
                    Ok(m) => m,
                    Err(_) => continue, // Ignore undecodable casts
                };
                match server.handle_cast(cast_msg, state, &ctx).await {
                    CastReply::NoReply(new_state) => {
                        state = new_state;
                    }
                    CastReply::Stop(reason, final_state) => {
                        server.terminate(&reason, final_state).await;
                        return ExitReason::Normal;
                    }
                }
            }
            Some("reply") => continue,
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
    ServerDead,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Timeout => write!(f, "call timed out"),
            CallError::ServerDead => write!(f, "server is dead"),
        }
    }
}

impl std::error::Error for CallError {}

// ---------------------------------------------------------------------------
// TempPidGuard (RAII cleanup for temporary call mailboxes)
// ---------------------------------------------------------------------------

struct TempPidGuard<'a> {
    table: &'a ProcessTable,
    pid: ProcessId,
    defused: bool,
}

impl Drop for TempPidGuard<'_> {
    fn drop(&mut self) {
        if !self.defused {
            self.table.remove(&self.pid);
        }
    }
}

// ---------------------------------------------------------------------------
// GenServerRef
// ---------------------------------------------------------------------------

/// Typed reference to a running GenServer process.
///
/// Provides typed `call()` and `cast()` methods that convert between the
/// GenServer's associated types and the wire-protocol `rmpv::Value`.
pub struct GenServerRef<S: GenServer> {
    pid: ProcessId,
    router: Rc<dyn MessageRouter>,
    table: Rc<ProcessTable>,
    _phantom: std::marker::PhantomData<S>,
}

impl<S: GenServer> GenServerRef<S> {
    /// Return the PID of the referenced GenServer process.
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Send a typed cast (fire-and-forget) to the GenServer.
    pub fn cast(&self, msg: S::Cast) -> Result<(), SendError> {
        // Use a synthetic sender PID (node 0, thread 0, local 0)
        let from = ProcessId::new(0, 0, 0);
        self.router.route(from, self.pid, cast_envelope(msg.into()))
    }

    /// Send a typed call and wait for a typed reply.
    ///
    /// Spawns a temporary process to receive the reply, similar to
    /// `call_from_runtime` but with typed conversion.
    pub async fn call(
        &self,
        msg: S::Call,
        timeout: std::time::Duration,
    ) -> Result<S::Reply, CallError> {
        let ref_id = CALL_REF_COUNTER.fetch_add(1, Ordering::Relaxed);

        // Create a temporary mailbox to receive the reply
        let temp_pid = self.table.allocate_pid();
        let (tx, mut rx) = Mailbox::unbounded();
        self.table.insert(temp_pid, ProcessHandle::new(tx));
        let mut guard = TempPidGuard {
            table: &self.table,
            pid: temp_pid,
            defused: false,
        };

        if self
            .router
            .route(temp_pid, self.pid, call_envelope(ref_id, msg.into()))
            .is_err()
        {
            return Err(CallError::ServerDead);
        }

        // Wait for reply with timeout
        let result = crate::time::timeout(timeout, async {
            while let Some(msg) = rx.recv().await {
                let payload = msg.payload().clone();
                if extract_gs_type(&payload) == Some("reply")
                    && extract_field(&payload, "ref")
                        .and_then(|v| v.as_u64())
                        == Some(ref_id)
                {
                    let val = extract_field(&payload, "val").unwrap_or(rmpv::Value::Nil);
                    return S::Reply::try_from(val).map_err(|_| CallError::ServerDead);
                }
            }
            Err(CallError::ServerDead)
        })
        .await;

        // Defuse guard so drop doesn't double-remove
        guard.defused = true;
        self.table.remove(&temp_pid);

        match result {
            Ok(r) => r,
            Err(_) => Err(CallError::Timeout),
        }
    }
}

impl<S: GenServer> Clone for GenServerRef<S> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            router: Rc::clone(&self.router),
            table: Rc::clone(&self.table),
            _phantom: std::marker::PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

static CALL_REF_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Spawn a GenServer as a rebar process, returning a typed reference.
pub fn spawn_gen_server<S: GenServer>(runtime: &Runtime, server: S) -> GenServerRef<S> {
    let pid = start(runtime, server);
    GenServerRef {
        pid,
        router: runtime.router().clone(),
        table: runtime.table().clone(),
        _phantom: std::marker::PhantomData,
    }
}

/// Spawn a GenServer as a rebar process.
pub fn start<S: GenServer>(runtime: &Runtime, server: S) -> ProcessId {
    let server = Rc::new(server);
    runtime.spawn(move |ctx| async move {
        gen_server_loop(server, ctx).await;
    })
}

/// Spawn a named GenServer.
pub fn start_named<S: GenServer>(
    runtime: &Runtime,
    name: String,
    server: S,
) -> Result<ProcessId, RegistryError> {
    let pid = start(runtime, server);
    runtime.register(name, pid)?;
    Ok(pid)
}

/// Synchronous call from outside any process (spawns a temporary process).
///
/// Must be called from within the executor runtime. Returns a future that
/// resolves when the reply arrives or the timeout expires.
pub async fn call_from_runtime(
    runtime: &Runtime,
    dest: ProcessId,
    request: rmpv::Value,
    timeout: std::time::Duration,
) -> Result<rmpv::Value, CallError> {
    let ref_id = CALL_REF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = crate::channel::oneshot::channel();

    runtime.spawn(move |mut ctx| async move {
        if ctx.send(dest, call_envelope(ref_id, request)).is_err() {
            let _ = tx.send(Err(CallError::ServerDead));
            return;
        }

        loop {
            match ctx.recv_timeout(timeout).await {
                Some(msg) => {
                    let payload = msg.payload().clone();
                    if extract_gs_type(&payload) == Some("reply")
                        && let Some(r) = extract_field(&payload, "ref").and_then(|v| v.as_u64())
                        && r == ref_id
                    {
                        let val =
                            extract_field(&payload, "val").unwrap_or(rmpv::Value::Nil);
                        let _ = tx.send(Ok(val));
                        return;
                    }
                }
                None => {
                    let _ = tx.send(Err(CallError::Timeout));
                    return;
                }
            }
        }
    });

    rx.await.map_err(|_| CallError::ServerDead)?
}

/// Fire-and-forget cast from outside any process.
pub fn cast_from_runtime(
    runtime: &Runtime,
    dest: ProcessId,
    request: rmpv::Value,
) -> Result<(), SendError> {
    runtime.send(dest, cast_envelope(request))
}

/// Reply to a From (for deferred replies outside the GenServer callbacks).
pub fn reply_from_runtime(
    runtime: &Runtime,
    from: &From,
    response: rmpv::Value,
) -> Result<(), SendError> {
    runtime.send(from.pid, reply_envelope(from.ref_id, response))
}

/// Create a ChildEntry for supervision.
pub fn child_entry<S: GenServer + Clone>(
    runtime: Rc<Runtime>,
    server: S,
    spec: ChildSpec,
) -> ChildEntry {
    ChildEntry::new(spec, move || {
        let runtime = Rc::clone(&runtime);
        let server = server.clone();
        async move {
            let (exit_tx, exit_rx) = crate::channel::oneshot::channel();
            let server = Rc::new(server);
            runtime.spawn(move |ctx| async move {
                let reason = gen_server_loop(server, ctx).await;
                let _ = exit_tx.send(reason);
            });
            match exit_rx.await {
                Ok(reason) => reason,
                Err(_) => {
                    ExitReason::Abnormal("process dropped without sending exit reason".into())
                }
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
    // Re-import std::convert::From since super::From (the struct) shadows it
    use std::convert::From as StdFrom;
    use crate::executor::{ExecutorConfig, RebarExecutor};
    use crate::time::sleep;
    use std::time::Duration;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    // -- Never type for servers that don't use call/cast/reply --

    enum Never {}
    impl StdFrom<Never> for rmpv::Value {
        fn from(n: Never) -> rmpv::Value { match n {} }
    }
    impl TryFrom<rmpv::Value> for Never {
        type Error = ();
        fn try_from(_: rmpv::Value) -> Result<Self, ()> { Err(()) }
    }

    // -- Counter message types --

    enum CounterCall {
        Get,
        Increment,
        Stop,
    }

    impl StdFrom<CounterCall> for rmpv::Value {
        fn from(c: CounterCall) -> rmpv::Value {
            match c {
                CounterCall::Get => rmpv::Value::String("get".into()),
                CounterCall::Increment => rmpv::Value::String("increment".into()),
                CounterCall::Stop => rmpv::Value::String("stop".into()),
            }
        }
    }

    impl TryFrom<rmpv::Value> for CounterCall {
        type Error = ();
        fn try_from(v: rmpv::Value) -> Result<Self, ()> {
            match v.as_str() {
                Some("get") => Ok(CounterCall::Get),
                Some("increment") => Ok(CounterCall::Increment),
                Some("stop") => Ok(CounterCall::Stop),
                _ => Err(()),
            }
        }
    }

    enum CounterCast {
        Increment,
        Stop,
    }

    impl StdFrom<CounterCast> for rmpv::Value {
        fn from(c: CounterCast) -> rmpv::Value {
            match c {
                CounterCast::Increment => rmpv::Value::String("increment".into()),
                CounterCast::Stop => rmpv::Value::String("stop".into()),
            }
        }
    }

    impl TryFrom<rmpv::Value> for CounterCast {
        type Error = ();
        fn try_from(v: rmpv::Value) -> Result<Self, ()> {
            match v.as_str() {
                Some("increment") => Ok(CounterCast::Increment),
                Some("stop") => Ok(CounterCast::Stop),
                _ => Err(()),
            }
        }
    }

    #[derive(Debug, PartialEq)]
    enum CounterReply {
        Value(u64),
        Bye,
    }

    impl StdFrom<CounterReply> for rmpv::Value {
        fn from(r: CounterReply) -> rmpv::Value {
            match r {
                CounterReply::Value(n) => rmpv::Value::Integer(n.into()),
                CounterReply::Bye => rmpv::Value::String("bye".into()),
            }
        }
    }

    impl TryFrom<rmpv::Value> for CounterReply {
        type Error = ();
        fn try_from(v: rmpv::Value) -> Result<Self, ()> {
            if let Some(n) = v.as_u64() {
                return Ok(CounterReply::Value(n));
            }
            if v.as_str() == Some("bye") {
                return Ok(CounterReply::Bye);
            }
            Err(())
        }
    }

    // -- Counter server used across most tests --

    #[derive(Clone)]
    struct CounterServer {
        initial: u64,
    }

    impl CounterServer {
        fn new(initial: u64) -> Self {
            Self { initial }
        }
    }

    impl GenServer for CounterServer {
        type State = u64;
        type Call = CounterCall;
        type Cast = CounterCast;
        type Reply = CounterReply;

        async fn init(&self, _ctx: &GenServerContext) -> Result<u64, String> {
            Ok(self.initial)
        }

        async fn handle_call(
            &self,
            msg: CounterCall,
            _from: From,
            state: u64,
            _ctx: &GenServerContext,
        ) -> CallReply<u64> {
            match msg {
                CounterCall::Get => {
                    CallReply::Reply(rmpv::Value::Integer(state.into()), state)
                }
                CounterCall::Increment => {
                    CallReply::Reply(rmpv::Value::Integer((state + 1).into()), state + 1)
                }
                CounterCall::Stop => {
                    CallReply::Stop("requested".into(), rmpv::Value::String("bye".into()), state)
                }
            }
        }

        async fn handle_cast(
            &self,
            msg: CounterCast,
            state: u64,
            _ctx: &GenServerContext,
        ) -> CastReply<u64> {
            match msg {
                CounterCast::Increment => CastReply::NoReply(state + 1),
                CounterCast::Stop => CastReply::Stop("cast_stop".into(), state),
            }
        }
    }

    fn new_runtime() -> Runtime {
        Runtime::new(1)
    }

    // 1. basic_call
    #[test]
    fn basic_call() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(0));
        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("increment".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_u64().unwrap(), 1);
        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("increment".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_u64().unwrap(), 2);
        });
    }

    // 2. basic_cast
    #[test]
    fn basic_cast() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(0));
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).unwrap();
        // Give the cast time to process
        sleep(Duration::from_millis(50)).await;
        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("get".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_u64().unwrap(), 1);
        });
    }

    // 3. call_timeout
    #[test]
    fn call_timeout() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        // Spawn a process that never processes messages (blocks forever)
        let pid = rt.spawn(|mut ctx| async move {
            loop {
                ctx.recv().await;
                // Read but never reply
            }
        });
        let result = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("hello".into()),
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(result, Err(CallError::Timeout)));
        });
    }

    // -- InfoServer (handle_info test) --

    struct InfoServer {
        received: Rc<std::cell::RefCell<Option<rmpv::Value>>>,
    }

    impl GenServer for InfoServer {
        type State = ();
        type Call = Never;
        type Cast = Never;
        type Reply = Never;

        async fn init(&self, _ctx: &GenServerContext) -> Result<(), String> {
            Ok(())
        }
        async fn handle_call(
            &self, msg: Never, _from: From, _state: (), _ctx: &GenServerContext,
        ) -> CallReply<()> {
            match msg {}
        }
        async fn handle_cast(
            &self, msg: Never, _state: (), _ctx: &GenServerContext,
        ) -> CastReply<()> {
            match msg {}
        }
        async fn handle_info(
            &self, msg: rmpv::Value, state: (), _ctx: &GenServerContext,
        ) -> InfoReply<()> {
            *self.received.borrow_mut() = Some(msg);
            InfoReply::NoReply(state)
        }
    }

    // 4. handle_info
    #[test]
    fn handle_info() {
        let ex = test_executor();
        ex.block_on(async {
        let received = Rc::new(std::cell::RefCell::new(None));
        let rt = new_runtime();
        let received_clone = Rc::clone(&received);
        let pid = start(
            &rt,
            InfoServer {
                received: received_clone,
            },
        );

        // Send a raw message (no $gs envelope) — should go to handle_info
        rt.send(pid, rmpv::Value::String("raw_info".into()))
            .unwrap();

        sleep(Duration::from_millis(50)).await;
        assert_eq!(
            received.borrow().as_ref().unwrap().as_str().unwrap(),
            "raw_info"
        );
        });
    }

    // 5. stop_from_call
    #[test]
    fn stop_from_call() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(0));
        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("stop".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_str().unwrap(), "bye");
        // Process should exit
        sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
        });
    }

    // 6. stop_from_cast
    #[test]
    fn stop_from_cast() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(0));
        cast_from_runtime(&rt, pid, rmpv::Value::String("stop".into())).unwrap();
        sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
        });
    }

    // 7. named_start
    #[test]
    fn named_start() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start_named(
            &rt,
            "my_counter".into(),
            CounterServer::new(0),
        )
        .unwrap();
        assert_eq!(rt.whereis("my_counter"), Some(pid));
        });
    }

    // -- DeferredServer (deferred_reply test) --

    enum DeferredCall { Deferred, Other }
    impl StdFrom<DeferredCall> for rmpv::Value {
        fn from(c: DeferredCall) -> rmpv::Value {
            match c {
                DeferredCall::Deferred => rmpv::Value::String("deferred".into()),
                DeferredCall::Other => rmpv::Value::Nil,
            }
        }
    }
    impl TryFrom<rmpv::Value> for DeferredCall {
        type Error = ();
        fn try_from(v: rmpv::Value) -> Result<Self, ()> {
            match v.as_str() {
                Some("deferred") => Ok(DeferredCall::Deferred),
                _ => Ok(DeferredCall::Other),
            }
        }
    }

    struct DeferredServer;

    impl GenServer for DeferredServer {
        type State = Option<From>;
        type Call = DeferredCall;
        type Cast = Never;
        type Reply = Never;

        async fn init(&self, _ctx: &GenServerContext) -> Result<Option<From>, String> {
            Ok(None)
        }
        async fn handle_call(
            &self, msg: DeferredCall, from: From, _state: Option<From>, _ctx: &GenServerContext,
        ) -> CallReply<Option<From>> {
            match msg {
                DeferredCall::Deferred => CallReply::NoReply(Some(from)),
                DeferredCall::Other => CallReply::Reply(rmpv::Value::Nil, None),
            }
        }
        async fn handle_cast(
            &self, msg: Never, _state: Option<From>, _ctx: &GenServerContext,
        ) -> CastReply<Option<From>> {
            match msg {}
        }
        async fn handle_info(
            &self, msg: rmpv::Value, state: Option<From>, ctx: &GenServerContext,
        ) -> InfoReply<Option<From>> {
            if msg.as_str() == Some("complete") {
                if let Some(ref from) = state {
                    let _ = ctx.reply(from, rmpv::Value::String("deferred_result".into()));
                }
            }
            InfoReply::NoReply(None)
        }
    }

    // 8. deferred_reply
    #[test]
    fn deferred_reply() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = Runtime::new(1);
        let pid = start(&rt, DeferredServer);

        // Spawn the call in the background
        let result = Rc::new(std::cell::RefCell::new(None));
        let result_clone = Rc::clone(&result);
        // SAFETY: rt lives on the stack for the duration of this test.
        // The spawned task completes before rt is dropped.
        let rt_ptr: *const Runtime = &rt;
        crate::executor::spawn(async move {
            let reply = call_from_runtime(
                unsafe { &*rt_ptr },
                pid,
                rmpv::Value::String("deferred".into()),
                Duration::from_secs(2),
            )
            .await;
            *result_clone.borrow_mut() = Some(reply);
        })
        .detach();

        // Give the call time to reach the server
        sleep(Duration::from_millis(50)).await;

        // Trigger completion via raw info message
        rt.send(pid, rmpv::Value::String("complete".into()))
            .unwrap();

        sleep(Duration::from_millis(100)).await;
        let reply = result.borrow().as_ref().unwrap().as_ref().unwrap().clone();
        assert_eq!(reply.as_str().unwrap(), "deferred_result");
        });
    }

    // 9. cast_from_runtime_test
    #[test]
    fn cast_from_runtime_test() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(10));
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).unwrap();
        cast_from_runtime(&rt, pid, rmpv::Value::String("increment".into())).unwrap();
        sleep(Duration::from_millis(50)).await;
        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("get".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_u64().unwrap(), 12);
        });
    }

    // -- FailServer (init_failure test) --

    struct FailServer;

    impl GenServer for FailServer {
        type State = ();
        type Call = Never;
        type Cast = Never;
        type Reply = Never;

        async fn init(&self, _ctx: &GenServerContext) -> Result<(), String> {
            Err("init failed".into())
        }
        async fn handle_call(
            &self, msg: Never, _from: From, _state: (), _ctx: &GenServerContext,
        ) -> CallReply<()> {
            match msg {}
        }
        async fn handle_cast(
            &self, msg: Never, _state: (), _ctx: &GenServerContext,
        ) -> CastReply<()> {
            match msg {}
        }
    }

    // 10. init_failure
    #[test]
    fn init_failure() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, FailServer);
        sleep(Duration::from_millis(100)).await;
        assert!(!rt.is_alive(pid));
        });
    }

    // -- PidReporter (self_pid test) --

    struct PidReporter;

    // PidReporter accepts any call, returns the server's own PID
    enum PidCall { GetPid }
    impl StdFrom<PidCall> for rmpv::Value {
        fn from(_: PidCall) -> rmpv::Value { rmpv::Value::String("get_pid".into()) }
    }
    impl TryFrom<rmpv::Value> for PidCall {
        type Error = ();
        fn try_from(_: rmpv::Value) -> Result<Self, ()> { Ok(PidCall::GetPid) }
    }

    impl GenServer for PidReporter {
        type State = ProcessId;
        type Call = PidCall;
        type Cast = Never;
        type Reply = Never;

        async fn init(&self, ctx: &GenServerContext) -> Result<ProcessId, String> {
            Ok(ctx.self_pid())
        }
        async fn handle_call(
            &self, _msg: PidCall, _from: From, state: ProcessId, _ctx: &GenServerContext,
        ) -> CallReply<ProcessId> {
            let pid_val = rmpv::Value::Array(vec![
                rmpv::Value::Integer(state.node_id().into()),
                rmpv::Value::Integer(state.local_id().into()),
            ]);
            CallReply::Reply(pid_val, state)
        }
        async fn handle_cast(
            &self, msg: Never, _state: ProcessId, _ctx: &GenServerContext,
        ) -> CastReply<ProcessId> {
            match msg {}
        }
    }

    // 11. gen_server_context_self_pid
    #[test]
    fn gen_server_context_self_pid() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, PidReporter);
        let reply =
            call_from_runtime(&rt, pid, rmpv::Value::Nil, Duration::from_secs(1))
                .await
                .unwrap();
        if let rmpv::Value::Array(arr) = reply {
            let node = arr[0].as_u64().unwrap();
            let local = arr[1].as_u64().unwrap();
            assert_eq!(node, pid.node_id());
            assert_eq!(local, pid.local_id());
        } else {
            panic!("expected array reply");
        }
        });
    }

    // 12. call_error_display
    #[test]
    fn call_error_display() {
        assert_eq!(format!("{}", CallError::Timeout), "call timed out");
        assert_eq!(format!("{}", CallError::ServerDead), "server is dead");
    }

    // 13. child_entry_creates_supervised
    #[test]
    fn child_entry_creates_supervised() {
        let rt = Rc::new(Runtime::new(1));
        let spec = ChildSpec::new("counter");
        let entry = child_entry(rt, CounterServer::new(0), spec);
        assert_eq!(entry.spec.id, "counter");
    }

    // 14. raw_malformed_call_envelope
    #[test]
    fn raw_malformed_call_envelope() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let pid = start(&rt, CounterServer::new(0));

        let malformed = rmpv::Value::Map(vec![
            (
                rmpv::Value::String("$gs".into()),
                rmpv::Value::String("call".into()),
            ),
            (
                rmpv::Value::String("req".into()),
                rmpv::Value::String("increment".into()),
            ),
        ]);
        rt.send(pid, malformed).unwrap();

        sleep(Duration::from_millis(100)).await;

        let reply = call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("get".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply.as_u64().unwrap(), 0);
        });
    }

    // -- FailInitServer (child_entry test) --

    #[derive(Clone)]
    struct FailInitServer;

    impl GenServer for FailInitServer {
        type State = ();
        type Call = Never;
        type Cast = Never;
        type Reply = Never;

        async fn init(&self, _ctx: &GenServerContext) -> Result<(), String> {
            Err("init failed".into())
        }
        async fn handle_call(
            &self, msg: Never, _from: From, _s: (), _ctx: &GenServerContext,
        ) -> CallReply<()> {
            match msg {}
        }
        async fn handle_cast(
            &self, msg: Never, _s: (), _ctx: &GenServerContext,
        ) -> CastReply<()> {
            match msg {}
        }
    }

    // 15. child_entry_propagates_abnormal_exit
    #[test]
    fn child_entry_propagates_abnormal_exit() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = Rc::new(Runtime::new(1));
        let spec = ChildSpec::new("fail_init");
        let entry = child_entry(Rc::clone(&rt), FailInitServer, spec);

        // Run the factory directly
        let reason = (entry.factory)().await;
        assert!(
            matches!(reason, ExitReason::Abnormal(ref msg) if msg == "init failed"),
            "expected Abnormal(\"init failed\"), got {:?}",
            reason
        );
        });
    }

    // -- QuickStopServer (child_entry normal exit test) --

    enum QuickCast { Stop, Other }
    impl StdFrom<QuickCast> for rmpv::Value {
        fn from(c: QuickCast) -> rmpv::Value {
            match c {
                QuickCast::Stop => rmpv::Value::String("stop".into()),
                QuickCast::Other => rmpv::Value::Nil,
            }
        }
    }
    impl TryFrom<rmpv::Value> for QuickCast {
        type Error = ();
        fn try_from(v: rmpv::Value) -> Result<Self, ()> {
            match v.as_str() {
                Some("stop") => Ok(QuickCast::Stop),
                _ => Ok(QuickCast::Other),
            }
        }
    }

    #[derive(Clone)]
    struct QuickStopServer;

    impl GenServer for QuickStopServer {
        type State = ();
        type Call = Never;
        type Cast = QuickCast;
        type Reply = Never;

        async fn init(&self, _ctx: &GenServerContext) -> Result<(), String> {
            Ok(())
        }
        async fn handle_call(
            &self, msg: Never, _from: From, _s: (), _ctx: &GenServerContext,
        ) -> CallReply<()> {
            match msg {}
        }
        async fn handle_cast(
            &self, msg: QuickCast, s: (), _ctx: &GenServerContext,
        ) -> CastReply<()> {
            match msg {
                QuickCast::Stop => CastReply::Stop("done".into(), s),
                QuickCast::Other => CastReply::NoReply(s),
            }
        }
    }

    // 16. child_entry_propagates_normal_exit
    #[test]
    fn child_entry_propagates_normal_exit() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = Rc::new(Runtime::new(1));
        let spec = ChildSpec::new("quick_stop");
        let entry = child_entry(Rc::clone(&rt), QuickStopServer, spec);
        let factory = entry.factory.clone();

        let result = Rc::new(std::cell::RefCell::new(None));
        let result_clone = Rc::clone(&result);
        crate::executor::spawn(async move {
            let reason = factory().await;
            *result_clone.borrow_mut() = Some(reason);
        })
        .detach();

        // Wait for the server to start, then find and stop it
        sleep(Duration::from_millis(100)).await;
        let pids = rt.list_processes();
        if let Some(&pid) = pids.last() {
            let _ = cast_from_runtime(&rt, pid, rmpv::Value::String("stop".into()));
        }

        sleep(Duration::from_millis(200)).await;
        let reason = result.borrow().clone();
        assert!(matches!(reason, Some(ExitReason::Normal)));
        });
    }

    // 17. spawn_gen_server_typed_call
    #[test]
    fn spawn_gen_server_typed_call() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let server_ref = spawn_gen_server(&rt, CounterServer::new(0));

        // Typed cast
        server_ref.cast(CounterCast::Increment).unwrap();
        sleep(Duration::from_millis(50)).await;

        // Typed call
        let reply = server_ref.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Value(1));

        let reply = server_ref.call(CounterCall::Increment, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Value(2));
        });
    }

    // 18. gen_server_ref_clone
    #[test]
    fn gen_server_ref_clone() {
        let ex = test_executor();
        ex.block_on(async {
        let rt = new_runtime();
        let server_ref = spawn_gen_server(&rt, CounterServer::new(0));
        let cloned = server_ref.clone();
        assert_eq!(server_ref.pid(), cloned.pid());

        cloned.cast(CounterCast::Increment).unwrap();
        sleep(Duration::from_millis(50)).await;

        let reply = server_ref.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Value(1));
        });
    }
}
