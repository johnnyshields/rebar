use std::rc::Rc;
use std::time::Duration;

// Re-import std::convert::From since gen_server::From (the struct) shadows it.
use std::convert::From as StdFrom;

use rebar_core::executor::{ExecutorConfig, RebarExecutor};
use rebar_core::gen_server::{
    self, spawn_gen_server, CallError, CallReply, CastReply, GenServer, GenServerContext, InfoReply,
};
use rebar_core::gen_server::From as GsFrom;
use rebar_core::runtime::Runtime;
use rebar_core::time::sleep;

fn test_executor() -> RebarExecutor {
    RebarExecutor::new(ExecutorConfig::default()).unwrap()
}

// ---------------------------------------------------------------------------
// gen_server_codec! macro — generates From<T> for rmpv::Value and
// TryFrom<rmpv::Value> for T for string-keyed enums.
// ---------------------------------------------------------------------------

macro_rules! gen_server_codec {
    // Simple string variants: Variant => "string"
    ($name:ident { $($variant:ident => $tag:literal),+ $(,)? }) => {
        impl StdFrom<$name> for rmpv::Value {
            fn from(v: $name) -> rmpv::Value {
                match v {
                    $($name::$variant => rmpv::Value::String($tag.into()),)+
                }
            }
        }

        impl TryFrom<rmpv::Value> for $name {
            type Error = ();
            fn try_from(v: rmpv::Value) -> Result<Self, ()> {
                match v.as_str() {
                    $(Some($tag) => Ok($name::$variant),)+
                    _ => Err(()),
                }
            }
        }
    };
    // Value-carrying variant: Variant(Type) => integer
    ($name:ident { $($variant:ident ( $inner:ty ) => integer),+ $(,)? }) => {
        impl StdFrom<$name> for rmpv::Value {
            fn from(v: $name) -> rmpv::Value {
                match v {
                    $($name::$variant(n) => rmpv::Value::Integer(n.into()),)+
                }
            }
        }

        impl TryFrom<rmpv::Value> for $name {
            type Error = ();
            fn try_from(v: rmpv::Value) -> Result<Self, ()> {
                $(
                    if let Some(n) = v.as_u64() {
                        return Ok($name::$variant(n as $inner));
                    }
                )+
                Err(())
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Counter message types with gen_server_codec! macro
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum CounterCall {
    Get,
    IncrementAndGet,
}
gen_server_codec!(CounterCall { Get => "get", IncrementAndGet => "increment_and_get" });

#[derive(Debug)]
enum CounterCast {
    Increment,
    Reset,
}
gen_server_codec!(CounterCast { Increment => "increment", Reset => "reset" });

#[derive(Debug, PartialEq)]
enum CounterReply {
    Count(u64),
}
gen_server_codec!(CounterReply { Count(u64) => integer });

// ---------------------------------------------------------------------------
// Counter GenServer
// ---------------------------------------------------------------------------

struct Counter;

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
        _from: GsFrom,
        state: Self::State,
        _ctx: &GenServerContext,
    ) -> CallReply<Self::State> {
        match msg {
            CounterCall::Get => CallReply::Reply(rmpv::Value::Integer(state.into()), state),
            CounterCall::IncrementAndGet => {
                let new = state + 1;
                CallReply::Reply(rmpv::Value::Integer(new.into()), new)
            }
        }
    }

    async fn handle_cast(
        &self,
        msg: Self::Cast,
        state: Self::State,
        _ctx: &GenServerContext,
    ) -> CastReply<Self::State> {
        match msg {
            CounterCast::Increment => CastReply::NoReply(state + 1),
            CounterCast::Reset => CastReply::NoReply(0),
        }
    }

    async fn handle_info(
        &self,
        msg: rmpv::Value,
        state: Self::State,
        _ctx: &GenServerContext,
    ) -> InfoReply<Self::State> {
        // If we get a raw integer payload, add it to state
        if let Some(val) = msg.as_u64() {
            return InfoReply::NoReply(state + val);
        }
        InfoReply::NoReply(state)
    }
}

// ---------------------------------------------------------------------------
// Never type for servers that don't use call/cast/reply
// ---------------------------------------------------------------------------

enum Never {}
impl StdFrom<Never> for rmpv::Value {
    fn from(n: Never) -> rmpv::Value {
        match n {}
    }
}
impl TryFrom<rmpv::Value> for Never {
    type Error = ();
    fn try_from(_: rmpv::Value) -> Result<Self, ()> {
        Err(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn counter_get_initial() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Count(0));
    });
}

#[test]
fn counter_increment_and_get() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        let reply = server
            .call(CounterCall::IncrementAndGet, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(reply, CounterReply::Count(1));
    });
}

#[test]
fn counter_cast_increment_then_get() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        server.cast(CounterCast::Increment).unwrap();
        server.cast(CounterCast::Increment).unwrap();
        server.cast(CounterCast::Increment).unwrap();
        sleep(Duration::from_millis(20)).await;
        let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Count(3));
    });
}

#[test]
fn counter_cast_reset() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        server.cast(CounterCast::Increment).unwrap();
        server.cast(CounterCast::Increment).unwrap();
        sleep(Duration::from_millis(10)).await;
        server.cast(CounterCast::Reset).unwrap();
        sleep(Duration::from_millis(10)).await;
        let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Count(0));
    });
}

#[test]
fn counter_handle_info_via_send() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        // Send a raw message to the GenServer's PID via the runtime
        rt.send(server.pid(), rmpv::Value::Integer(5u64.into())).unwrap();
        sleep(Duration::from_millis(50)).await;
        let reply = server.call(CounterCall::Get, Duration::from_secs(1)).await.unwrap();
        assert_eq!(reply, CounterReply::Count(5));
    });
}

#[test]
fn counter_concurrent_calls() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);

        for _ in 0..10 {
            let s = server.clone();
            rebar_core::executor::spawn(async move {
                s.call(CounterCall::IncrementAndGet, Duration::from_secs(1))
                    .await
                    .unwrap();
            })
            .detach();
        }

        // Give concurrent calls time to complete
        sleep(Duration::from_millis(200)).await;

        let reply = server
            .call(CounterCall::Get, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(reply, CounterReply::Count(10));
    });
}

#[test]
fn gen_server_ref_clone_works() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let server = spawn_gen_server(&rt, Counter);
        let server2 = server.clone();
        assert_eq!(server.pid(), server2.pid());

        server.cast(CounterCast::Increment).unwrap();
        sleep(Duration::from_millis(10)).await;
        let reply = server2
            .call(CounterCall::Get, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(reply, CounterReply::Count(1));
    });
}

// ---------------------------------------------------------------------------
// Init failure test
// ---------------------------------------------------------------------------

struct FailInit;

impl GenServer for FailInit {
    type State = ();
    type Call = Never;
    type Cast = Never;
    type Reply = Never;

    async fn init(&self, _ctx: &GenServerContext) -> Result<Self::State, String> {
        Err("init failed".into())
    }

    async fn handle_call(
        &self,
        msg: Never,
        _from: GsFrom,
        _state: (),
        _ctx: &GenServerContext,
    ) -> CallReply<()> {
        match msg {}
    }

    async fn handle_cast(
        &self,
        msg: Never,
        _state: (),
        _ctx: &GenServerContext,
    ) -> CastReply<()> {
        match msg {}
    }
}

#[test]
fn gen_server_init_failure() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let pid = gen_server::start(&rt, FailInit);
        sleep(Duration::from_millis(50)).await;
        // Server should be dead, call should fail
        let result = gen_server::call_from_runtime(
            &rt,
            pid,
            rmpv::Value::Nil,
            Duration::from_millis(100),
        )
        .await;
        assert!(result.is_err());
    });
}

// ---------------------------------------------------------------------------
// Deferred reply test (v5-specific, using From struct)
// ---------------------------------------------------------------------------

enum DeferredCall {
    Deferred,
}
gen_server_codec!(DeferredCall { Deferred => "deferred" });

struct DeferredServer;

impl GenServer for DeferredServer {
    type State = Option<GsFrom>;
    type Call = DeferredCall;
    type Cast = Never;
    type Reply = Never;

    async fn init(&self, _ctx: &GenServerContext) -> Result<Option<GsFrom>, String> {
        Ok(None)
    }

    async fn handle_call(
        &self,
        msg: DeferredCall,
        from: GsFrom,
        _state: Option<GsFrom>,
        _ctx: &GenServerContext,
    ) -> CallReply<Option<GsFrom>> {
        match msg {
            DeferredCall::Deferred => CallReply::NoReply(Some(from)),
        }
    }

    async fn handle_cast(
        &self,
        msg: Never,
        _state: Option<GsFrom>,
        _ctx: &GenServerContext,
    ) -> CastReply<Option<GsFrom>> {
        match msg {}
    }

    async fn handle_info(
        &self,
        msg: rmpv::Value,
        state: Option<GsFrom>,
        ctx: &GenServerContext,
    ) -> InfoReply<Option<GsFrom>> {
        if msg.as_str() == Some("complete") {
            if let Some(ref from) = state {
                let _ = ctx.reply(from, rmpv::Value::String("deferred_result".into()));
            }
        }
        InfoReply::NoReply(None)
    }
}

#[test]
fn deferred_reply() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let pid = gen_server::start(&rt, DeferredServer);

        let result = Rc::new(std::cell::RefCell::new(None));
        let result_clone = Rc::clone(&result);
        // SAFETY: rt lives on the stack for the duration of this test.
        // The spawned task completes before rt is dropped.
        let rt_ptr: *const Runtime = &rt;
        rebar_core::executor::spawn(async move {
            let reply = gen_server::call_from_runtime(
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
        rt.send(pid, rmpv::Value::String("complete".into())).unwrap();

        sleep(Duration::from_millis(100)).await;
        let reply = result.borrow().as_ref().unwrap().as_ref().unwrap().clone();
        assert_eq!(reply.as_str().unwrap(), "deferred_result");
    });
}

// ---------------------------------------------------------------------------
// Child entry supervision test (v5-specific)
// ---------------------------------------------------------------------------

#[test]
fn child_entry_supervision() {
    use rebar_core::gen_server::child_entry;
    use rebar_core::supervisor::spec::ChildSpec;

    #[derive(Clone)]
    struct SupervisedCounter;

    impl GenServer for SupervisedCounter {
        type State = u64;
        type Call = CounterCall;
        type Cast = CounterCast;
        type Reply = CounterReply;

        async fn init(&self, _ctx: &GenServerContext) -> Result<u64, String> {
            Ok(0)
        }

        async fn handle_call(
            &self,
            msg: CounterCall,
            _from: GsFrom,
            state: u64,
            _ctx: &GenServerContext,
        ) -> CallReply<u64> {
            match msg {
                CounterCall::Get => CallReply::Reply(rmpv::Value::Integer(state.into()), state),
                CounterCall::IncrementAndGet => {
                    let new = state + 1;
                    CallReply::Reply(rmpv::Value::Integer(new.into()), new)
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
                CounterCast::Reset => CastReply::NoReply(0),
            }
        }
    }

    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let rt_rc = Rc::new(rt);
        let spec = ChildSpec::new("counter");
        let entry = child_entry(Rc::clone(&rt_rc), SupervisedCounter, spec);
        // The child_entry should have the correct id
        assert_eq!(entry.spec.id, "counter");
    });
}

// ---------------------------------------------------------------------------
// Cast-only server test: GenServer with type Call = Never, only uses cast
// ---------------------------------------------------------------------------

struct CastOnlyServer;

#[derive(Debug)]
enum CastOnlyMsg {
    Ping,
}
gen_server_codec!(CastOnlyMsg { Ping => "ping" });

impl GenServer for CastOnlyServer {
    type State = u64;
    type Call = Never;
    type Cast = CastOnlyMsg;
    type Reply = Never;

    async fn init(&self, _ctx: &GenServerContext) -> Result<u64, String> {
        Ok(0)
    }

    async fn handle_call(
        &self,
        msg: Never,
        _from: GsFrom,
        _state: u64,
        _ctx: &GenServerContext,
    ) -> CallReply<u64> {
        match msg {}
    }

    async fn handle_cast(
        &self,
        _msg: CastOnlyMsg,
        state: u64,
        _ctx: &GenServerContext,
    ) -> CastReply<u64> {
        CastReply::NoReply(state + 1)
    }
}

#[test]
fn cast_only_server() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let pid = gen_server::start(&rt, CastOnlyServer);

        // Cast should work
        gen_server::cast_from_runtime(&rt, pid, rmpv::Value::String("ping".into())).unwrap();
        sleep(Duration::from_millis(50)).await;

        // Call should fail because Never can't be decoded
        let result = gen_server::call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("anything".into()),
            Duration::from_millis(200),
        )
        .await;
        // The call gets a reply (bad_call: could not decode message), not a timeout
        assert!(result.is_ok());
        let reply = result.unwrap();
        assert!(reply.as_str().unwrap().contains("bad_call"));
    });
}

// ---------------------------------------------------------------------------
// Dead server call test: call a dead server, verify error
// ---------------------------------------------------------------------------

#[test]
fn dead_server_call() {
    let ex = test_executor();
    ex.block_on(async {
        let rt = Runtime::new(1);
        let pid = gen_server::start(&rt, FailInit);
        // Wait for init failure to kill the server
        sleep(Duration::from_millis(50)).await;
        assert!(!rt.is_alive(pid));

        let result = gen_server::call_from_runtime(
            &rt,
            pid,
            rmpv::Value::String("get".into()),
            Duration::from_millis(200),
        )
        .await;
        assert!(
            matches!(result, Err(CallError::Timeout) | Err(CallError::ServerDead)),
            "expected Timeout or ServerDead, got {:?}",
            result
        );
    });
}
