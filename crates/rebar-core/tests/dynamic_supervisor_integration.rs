use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rebar_core::executor::{ExecutorConfig, RebarExecutor};
use rebar_core::process::{ExitReason, ProcessId};
use rebar_core::runtime::Runtime;
use rebar_core::supervisor::{
    start_dynamic_supervisor, ChildEntry, ChildSpec, DynamicSupervisorSpec, RestartType,
    SupervisorError,
};
use rebar_core::time::sleep;

fn test_executor() -> RebarExecutor {
    RebarExecutor::new(ExecutorConfig::default()).unwrap()
}

#[test]
fn dynamic_supervisor_manages_many_children() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        // Start 10 children
        for i in 0..10 {
            let entry = ChildEntry::new(ChildSpec::new(format!("worker-{i}")), || async {
                loop {
                    sleep(Duration::from_secs(3600)).await;
                }
            });
            handle.start_child(entry).await.unwrap();
        }

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 10);
        assert_eq!(counts.specs, 10);

        // Collect PIDs to terminate 5
        let children = handle.which_children().await.unwrap();
        let pids_to_terminate: Vec<ProcessId> = children
            .iter()
            .filter_map(|c| c.pid)
            .take(5)
            .collect();

        for pid in pids_to_terminate {
            handle.terminate_child(pid).await.unwrap();
        }

        sleep(Duration::from_millis(50)).await;

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 5);

        handle.shutdown();
    });
}

#[test]
fn terminate_nonexistent_child_returns_error() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        let bogus_pid = ProcessId::new(0, 0, 99999);
        let result = handle.terminate_child(bogus_pid).await;
        assert!(result.is_err());

        handle.shutdown();
    });
}

#[test]
fn remove_running_child_returns_error() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        let entry = ChildEntry::new(ChildSpec::new("worker"), || async {
            loop {
                sleep(Duration::from_secs(3600)).await;
            }
        });
        let pid = handle.start_child(entry).await.unwrap();

        // Try to remove without terminating first — should fail
        let result = handle.remove_child(pid).await;
        assert!(matches!(result, Err(SupervisorError::StillRunning(_))));

        handle.shutdown();
    });
}

#[test]
fn transient_child_restarted_on_abnormal_exit() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let spec = DynamicSupervisorSpec::new()
            .max_restarts(10)
            .max_seconds(5);
        let handle = start_dynamic_supervisor(&rt, spec);

        let counter = Rc::new(Cell::new(0u32));
        let counter_clone = Rc::clone(&counter);

        let entry = ChildEntry::new(
            ChildSpec::new("crasher").restart(RestartType::Transient),
            move || {
                let c = Rc::clone(&counter_clone);
                async move {
                    c.set(c.get() + 1);
                    ExitReason::Abnormal("crash".into())
                }
            },
        );

        handle.start_child(entry).await.unwrap();

        // Wait for restarts
        sleep(Duration::from_millis(200)).await;

        let start_count = counter.get();
        assert!(
            start_count >= 2,
            "expected at least 2 starts (original + restart), got {start_count}",
        );

        handle.shutdown();
    });
}

#[test]
fn transient_child_not_restarted_on_normal_exit() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        let counter = Rc::new(Cell::new(0u32));
        let counter_clone = Rc::clone(&counter);

        let entry = ChildEntry::new(
            ChildSpec::new("normal-exit").restart(RestartType::Transient),
            move || {
                let c = Rc::clone(&counter_clone);
                async move {
                    c.set(c.get() + 1);
                    ExitReason::Normal
                }
            },
        );

        handle.start_child(entry).await.unwrap();

        // Wait to confirm no restart
        sleep(Duration::from_millis(200)).await;

        let start_count = counter.get();
        assert_eq!(
            start_count, 1,
            "transient child should NOT restart on normal exit, got {start_count} starts",
        );

        handle.shutdown();
    });
}

#[test]
fn max_children_limit() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let spec = DynamicSupervisorSpec::new().max_children(2);
        let handle = start_dynamic_supervisor(&rt, spec);

        let e1 = ChildEntry::new(ChildSpec::new("w1"), || async {
            loop { sleep(Duration::from_secs(60)).await; }
        });
        let e2 = ChildEntry::new(ChildSpec::new("w2"), || async {
            loop { sleep(Duration::from_secs(60)).await; }
        });
        let e3 = ChildEntry::new(ChildSpec::new("w3"), || async {
            loop { sleep(Duration::from_secs(60)).await; }
        });

        assert!(handle.start_child(e1).await.is_ok());
        assert!(handle.start_child(e2).await.is_ok());
        let result = handle.start_child(e3).await;
        assert!(matches!(result, Err(SupervisorError::MaxChildren)));

        handle.shutdown();
    });
}

#[test]
fn shutdown_stops_all_children() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        for i in 0..5 {
            let entry = ChildEntry::new(ChildSpec::new(format!("w{i}")), || async {
                loop { sleep(Duration::from_secs(60)).await; }
            });
            handle.start_child(entry).await.unwrap();
        }

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 5);

        handle.shutdown();

        sleep(Duration::from_millis(100)).await;

        let result = handle.count_children().await;
        assert!(result.is_err());
    });
}

#[test]
fn panicked_child_restarted() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let sc = Arc::new(AtomicU32::new(0));
        let scc = Arc::clone(&sc);

        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(&rt, spec);

        let entry = ChildEntry::new(
            ChildSpec::new("panicker").restart(RestartType::Permanent),
            move || {
                let s = Arc::clone(&scc);
                async move {
                    let count = s.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        panic!("first run panics");
                    }
                    // Second run stays alive
                    sleep(Duration::from_secs(60)).await;
                    ExitReason::Normal
                }
            },
        );
        handle.start_child(entry).await.unwrap();

        sleep(Duration::from_millis(500)).await;
        assert_eq!(sc.load(Ordering::SeqCst), 2);

        handle.shutdown();
    });
}

#[test]
fn remove_child_happy_path() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let handle = start_dynamic_supervisor(&rt, DynamicSupervisorSpec::new());

        // Use a Temporary child that exits normally — it won't restart,
        // so it ends up in the terminated list (eligible for remove_child).
        let entry = ChildEntry::new(
            ChildSpec::new("removable").restart(RestartType::Temporary),
            || async {
                ExitReason::Normal
            },
        );
        let pid = handle.start_child(entry).await.unwrap();

        // Wait for the child to exit and be recorded as terminated
        sleep(Duration::from_millis(100)).await;

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);
        assert_eq!(counts.specs, 1); // spec still tracked

        // Remove the terminated child spec
        handle.remove_child(pid).await.unwrap();

        let counts = handle.count_children().await.unwrap();
        assert_eq!(counts.active, 0);
        assert_eq!(counts.specs, 0);

        handle.shutdown();
    });
}

#[test]
fn which_children_after_restart() {
    test_executor().block_on(async {
        let rt = Runtime::new(1);
        let spec = DynamicSupervisorSpec::new()
            .max_restarts(5)
            .max_seconds(10);
        let handle = start_dynamic_supervisor(&rt, spec);

        let crash_once = Arc::new(AtomicU32::new(0));
        let crash_once_clone = Arc::clone(&crash_once);

        let entry = ChildEntry::new(
            ChildSpec::new("crasher").restart(RestartType::Permanent),
            move || {
                let c = Arc::clone(&crash_once_clone);
                async move {
                    let count = c.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        // First run: crash
                        ExitReason::Abnormal("crash once".into())
                    } else {
                        // Second run: stay alive
                        sleep(Duration::from_secs(60)).await;
                        ExitReason::Normal
                    }
                }
            },
        );

        let original_pid = handle.start_child(entry).await.unwrap();

        // Wait for the crash and restart
        sleep(Duration::from_millis(300)).await;

        assert!(crash_once.load(Ordering::SeqCst) >= 2, "expected at least 2 starts");

        let children = handle.which_children().await.unwrap();
        assert_eq!(children.len(), 1);
        let new_pid = children[0].pid;
        assert!(new_pid.is_some(), "restarted child should be running");
        assert_ne!(
            new_pid.unwrap(),
            original_pid,
            "PID should change after restart"
        );

        handle.shutdown();
    });
}
