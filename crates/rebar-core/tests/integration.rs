//! Integration tests for rebar-core.
//!
//! These tests exercise the full runtime from the outside, using only
//! the crate's public API.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rebar_core::process::ExitReason;
use rebar_core::runtime::{ProcessContext, Runtime};
use rebar_core::supervisor::{
    ChildEntry, ChildSpec, RestartStrategy, RestartType, ShutdownStrategy, SupervisorSpec,
    start_supervisor,
};

// ---------------------------------------------------------------------------
// 1. ping_pong_between_processes
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn ping_pong_between_processes() {
    let rt = Runtime::new(1);
    let result = Rc::new(RefCell::new(None));
    let result_clone = Rc::clone(&result);

    // Spawn process B first so A can address it.
    let b_pid = rt.spawn(|mut ctx: ProcessContext| async move {
        let msg = ctx.recv().await.unwrap();
        assert_eq!(msg.payload().as_str().unwrap(), "ping");
        let sender = msg.from();
        ctx.send(sender, rmpv::Value::String("pong".into()))
            .unwrap();
    });

    // Spawn process A: sends "ping" to B, waits for "pong".
    rt.spawn(move |mut ctx: ProcessContext| async move {
        ctx.send(b_pid, rmpv::Value::String("ping".into()))
            .unwrap();
        let reply = ctx.recv().await.unwrap();
        let payload = reply.payload().as_str().unwrap().to_string();
        *result_clone.borrow_mut() = Some(payload);
    });

    monoio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*result.borrow(), Some("pong".to_string()));
}

// ---------------------------------------------------------------------------
// 2. fan_out_to_multiple_workers
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn fan_out_to_multiple_workers() {
    let rt = Runtime::new(1);
    let n: u64 = 10;
    let results = Rc::new(RefCell::new(Vec::new()));

    // Spawn N workers.
    let mut worker_pids = Vec::new();
    for _ in 0..n {
        let results = Rc::clone(&results);
        let pid = rt.spawn(move |mut ctx: ProcessContext| async move {
            let msg = ctx.recv().await.unwrap();
            let val = msg.payload().as_u64().unwrap();
            results.borrow_mut().push(val * 2);
        });
        worker_pids.push(pid);
    }

    // Dispatcher sends a different value to each worker.
    rt.spawn(move |ctx: ProcessContext| async move {
        for (i, pid) in worker_pids.iter().enumerate() {
            ctx.send(*pid, rmpv::Value::Integer((i as u64).into()))
                .unwrap();
        }
    });

    monoio::time::sleep(Duration::from_millis(200)).await;

    let mut r = results.borrow().clone();
    r.sort();
    let expected: Vec<u64> = (0..n).map(|i| i * 2).collect();
    assert_eq!(r, expected);
}

// ---------------------------------------------------------------------------
// 3. chain_of_three_processes
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn chain_of_three_processes() {
    let rt = Runtime::new(1);
    let result = Rc::new(Cell::new(0u64));
    let result_clone = Rc::clone(&result);

    // C — final receiver.
    let c = rt.spawn(move |mut ctx: ProcessContext| async move {
        let msg = ctx.recv().await.unwrap();
        let val = msg.payload().as_u64().unwrap();
        result_clone.set(val);
    });

    // B — transforms: val + 10.
    let b = rt.spawn(move |mut ctx: ProcessContext| async move {
        let msg = ctx.recv().await.unwrap();
        let val = msg.payload().as_u64().unwrap();
        ctx.send(c, rmpv::Value::Integer((val + 10).into()))
            .unwrap();
    });

    // A — starts the chain with value 5.
    rt.spawn(move |ctx: ProcessContext| async move {
        ctx.send(b, rmpv::Value::Integer(5u64.into())).unwrap();
    });

    monoio::time::sleep(Duration::from_millis(200)).await;
    // A sends 5 → B adds 10 → C receives 15.
    assert_eq!(result.get(), 15);
}

// ---------------------------------------------------------------------------
// 4. supervisor_restarts_crashed_worker
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn supervisor_restarts_crashed_worker() {
    let rt = Runtime::new(1);
    let start_count = Arc::new(AtomicU32::new(0));
    let sc = Arc::clone(&start_count);

    let entry = ChildEntry::new(
        ChildSpec::new("crasher").restart(RestartType::Permanent),
        move || {
            let sc = Arc::clone(&sc);
            async move {
                let n = sc.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    ExitReason::Abnormal("boom".into())
                } else {
                    monoio::time::sleep(Duration::from_secs(30)).await;
                    ExitReason::Normal
                }
            }
        },
    );

    let spec = SupervisorSpec::new(RestartStrategy::OneForOne).max_restarts(5);
    let handle = start_supervisor(&rt, spec, vec![entry]);

    monoio::time::sleep(Duration::from_millis(500)).await;
    assert!(start_count.load(Ordering::SeqCst) >= 2);

    handle.shutdown();
    monoio::time::sleep(Duration::from_millis(50)).await;
}

// ---------------------------------------------------------------------------
// 5. supervisor_one_for_all_cascade
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn supervisor_one_for_all_cascade() {
    let rt = Runtime::new(1);
    let counters: Vec<Arc<AtomicU32>> = (0..3).map(|_| Arc::new(AtomicU32::new(0))).collect();

    let mut entries = Vec::new();
    for i in 0..3usize {
        let counter = Arc::clone(&counters[i]);
        entries.push(ChildEntry::new(
            ChildSpec::new(format!("child_{}", i))
                .restart(RestartType::Permanent)
                .shutdown(ShutdownStrategy::BrutalKill),
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    if i == 0 && n == 1 {
                        ExitReason::Abnormal("crash".into())
                    } else {
                        monoio::time::sleep(Duration::from_secs(30)).await;
                        ExitReason::Normal
                    }
                }
            },
        ));
    }

    let spec = SupervisorSpec::new(RestartStrategy::OneForAll).max_restarts(5);
    let handle = start_supervisor(&rt, spec, entries);

    monoio::time::sleep(Duration::from_millis(500)).await;

    for (i, c) in counters.iter().enumerate() {
        assert!(
            c.load(Ordering::SeqCst) >= 2,
            "child {} started only {} time(s)",
            i,
            c.load(Ordering::SeqCst)
        );
    }

    handle.shutdown();
    monoio::time::sleep(Duration::from_millis(50)).await;
}

// ---------------------------------------------------------------------------
// 6. process_death_detection
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn process_death_detection() {
    let rt = Runtime::new(1);
    let detected = Rc::new(Cell::new(false));
    let detected_clone = Rc::clone(&detected);

    // Target process: exits immediately.
    let target = rt.spawn(|_ctx: ProcessContext| async move {});

    // Watcher process: repeatedly tries to send to target until it gets an error.
    rt.spawn(move |ctx: ProcessContext| async move {
        for _ in 0..50 {
            monoio::time::sleep(Duration::from_millis(20)).await;
            if ctx.send(target, rmpv::Value::Nil).is_err() {
                detected_clone.set(true);
                break;
            }
        }
    });

    monoio::time::sleep(Duration::from_millis(500)).await;
    assert!(detected.get(), "watcher should have detected target exit");
}

// ---------------------------------------------------------------------------
// 7. linked_process_dies_together (via OneForAll supervisor)
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn linked_process_dies_together() {
    let rt = Runtime::new(1);
    let peer_start_count = Arc::new(AtomicU32::new(0));
    let crasher_count = Arc::new(AtomicU32::new(0));
    let cc = Arc::clone(&crasher_count);
    let psc = Arc::clone(&peer_start_count);

    let crasher_entry = ChildEntry::new(
        ChildSpec::new("crasher")
            .restart(RestartType::Permanent)
            .shutdown(ShutdownStrategy::BrutalKill),
        move || {
            let cc = Arc::clone(&cc);
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    ExitReason::Abnormal("crash".into())
                } else {
                    monoio::time::sleep(Duration::from_secs(30)).await;
                    ExitReason::Normal
                }
            }
        },
    );

    let peer_entry = ChildEntry::new(
        ChildSpec::new("peer")
            .restart(RestartType::Permanent)
            .shutdown(ShutdownStrategy::BrutalKill),
        move || {
            let psc = Arc::clone(&psc);
            async move {
                psc.fetch_add(1, Ordering::SeqCst);
                monoio::time::sleep(Duration::from_secs(30)).await;
                ExitReason::Normal
            }
        },
    );

    let spec = SupervisorSpec::new(RestartStrategy::OneForAll).max_restarts(5);
    let handle = start_supervisor(&rt, spec, vec![crasher_entry, peer_entry]);

    monoio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        peer_start_count.load(Ordering::SeqCst) >= 2,
        "peer should have been restarted when crasher died"
    );

    handle.shutdown();
    monoio::time::sleep(Duration::from_millis(50)).await;
}

// ---------------------------------------------------------------------------
// 8. spawn_1000_processes_all_complete
// ---------------------------------------------------------------------------

#[monoio::test(enable_timer = true)]
async fn spawn_1000_processes_all_complete() {
    let rt = Runtime::new(1);
    let count = Rc::new(Cell::new(0u64));

    for _ in 0..1000u64 {
        let count = Rc::clone(&count);
        rt.spawn(move |_ctx: ProcessContext| async move {
            count.set(count.get() + 1);
        });
    }

    monoio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count.get(), 1000, "expected all 1000 processes to report in");
}
