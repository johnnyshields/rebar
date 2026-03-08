# Panic Isolation in Rebar

**Date:** 2026-03-07
**Status:** Analysis complete, implementation pending

## Overview

Rebar's panic isolation was lost during the rebar-v5 rewrite that replaced the Tokio-based runtime with a custom single-threaded executor built on compio + turbine. This document captures the history, analysis, and implementation plan for restoring it.

## History

### Origin/main: Tokio double-spawn pattern

`crates/rebar-core/src/runtime.rs` on `origin/main` used a double-spawn pattern:

```rust
tokio::spawn(async move {
    let inner = tokio::spawn(handler(ctx));
    let _ = inner.await; // JoinError absorbs panics
    table.remove(&pid);
});
```

Tokio's `JoinHandle` converts panics into `JoinError` rather than propagating them. The outer task always runs cleanup regardless of whether the inner task panicked. Test `process_panic_does_not_crash_runtime` verified this.

### rebar-v5: Lost during executor rewrite

The custom executor (`executor.rs`) and task system (`task.rs`) replaced Tokio. `RawTask::poll` directly calls `future.as_mut().poll(&mut cx)` with no `catch_unwind`. A panicking actor unwinds through `poll_tasks` and kills the executor thread.

### phase1-eliminate-panics branch

This branch took a complementary approach: converting `.unwrap()`/`.expect()` on untrusted input into `Result` returns (router.rs, types.rs, ffi). It eliminated panic *sources* but did not add a panic *boundary*.

## Key Architectural Decisions

### Why catch_unwind in RawTask::poll (not elsewhere)

- The poll site in `task.rs:41` is the single point where all user futures execute
- Wrapping here catches panics from any spawned task uniformly
- The executor loop in `poll_tasks` stays clean — it just checks the return value
- This mirrors what Tokio does internally in its task harness

### Turbine compatibility: confirmed safe

- `LeasedBuffer` uses `Drop` to release leases back to the arena
- Rust unwind runs `Drop` impls, so leases are properly released during panic
- `Rc`-based types aren't `UnwindSafe` by default — requires `AssertUnwindSafe`
- `panic = "abort"` must NOT be set in Cargo.toml (it isn't, and shouldn't be)

### AssertUnwindSafe justification

The future inside `RawTask` contains `Rc`, `Cell`, `RefCell` — none of which are `UnwindSafe`. Using `AssertUnwindSafe` is correct here because:
1. Each task is isolated — no shared mutable state leaks across tasks
2. After a panic, the task is marked completed and never polled again
3. All `Drop` impls run, cleaning up resources (including turbine leases)

## Important Files

| File | Role |
|------|------|
| `crates/rebar-core/src/task.rs` | `RawTask::poll` — the catch_unwind boundary goes here |
| `crates/rebar-core/src/executor.rs` | `poll_tasks` — consumes the poll result |
| `crates/rebar-core/src/runtime.rs` (origin/main) | Original Tokio double-spawn implementation |
| `crates/rebar-core/src/supervisor/engine.rs` | Supervisor that should receive panic exit reasons |

## Dependencies

None new — `std::panic::catch_unwind` and `std::panic::AssertUnwindSafe` are in std.

## Security Considerations

- Without panic isolation, a single malformed message can crash an entire rebar node
- Barkeeper (etcd-compatible KV store) runs Raft as rebar actors — a panic kills the whole node instead of triggering supervised restart
- phase1-eliminate-panics reduced the attack surface but didn't eliminate it

## Testing Approach

- Port the `process_panic_does_not_crash_runtime` test from origin/main to the new executor
- Test that turbine `LeasedBuffer` held during panic is properly released (no leak)
- Test that panicked task's `JoinHandle` resolves (with error or cancellation)
- Test that supervisor receives exit reason on actor panic

## Future Enhancements

- Panic exit reasons: convert panic payload into a structured `ExitReason::Panic(String)` for supervisor
- Panic logging: capture panic info (message + location) before marking task complete
- Per-task panic hooks: allow actors to register custom panic handlers
- Metrics: count panics per actor type for observability
