# Harden: Phase 2 — Graceful Runtime Shutdown

## Changes Reviewed
- Adds `CancellationToken` (from `tokio-util`) to `Runtime` and `ProcessContext`, enabling cooperative graceful shutdown.
- `ProcessContext` gains `is_shutting_down()` and `cancelled()` methods so processes can react to shutdown.
- `Runtime::shutdown(self, timeout)` cancels the token and awaits all task handles with a timeout.
- `Runtime` tracks spawned `JoinHandle`s in `Arc<Mutex<Vec<JoinHandle<()>>>>`.
- Adds `with_mailbox_capacity` builder method for bounded mailboxes on spawn.
- Five new tests covering shutdown, cancellation propagation, empty runtime, bounded/unbounded mailboxes.

## Effort/Impact Table

| # | Opportunity | Effort | Impact | Action |
|---|-------------|--------|--------|--------|
| 1 | Use `tokio::task::JoinSet` instead of `Vec<JoinHandle>` + `std::sync::Mutex` | Quick | Medium | Ask first |
| 2 | Completed-handle accumulation: handles never pruned from `task_handles` | Quick | Medium | Ask first |
| 3 | `shutdown` sequential join: use `join_all` or `JoinSet` for concurrent awaiting | Quick | Medium | Ask first |
| 4 | Missing test: shutdown timeout actually fires (process ignores cancellation) | Quick | Medium | Ask first |
| 5 | Missing test: spawn-after-shutdown behavior is undefined/untested | Quick | Low | Ask first |
| 6 | `with_mailbox_capacity(0)` is silently accepted but semantically dubious | Quick | Low | Ask first |
| 7 | Constructor duplication between `new` and `with_router` | Easy | Low | Ask first |

## Opportunity Details

### 1. Use `tokio::task::JoinSet` instead of `Vec<JoinHandle>` + `std::sync::Mutex`
- **What**: Replace `Arc<Mutex<Vec<JoinHandle<()>>>>` with `tokio::task::JoinSet<()>`. `JoinSet` is purpose-built for tracking a dynamic set of spawned tasks: it handles spawning, joining, and cancellation in one type.
- **Where**: `Runtime` struct definition, `spawn()`, `shutdown()`.
- **Why**: Eliminates the `std::sync::Mutex` (which can panic on poison and blocks the thread), removes manual handle bookkeeping, and `JoinSet` automatically cleans up completed handles (solving opportunity #2). It also enables concurrent join (solving opportunity #3).
- **Trade-offs**: Requires `&mut self` or interior mutability for `JoinSet::spawn`. Since `spawn` takes `&self`, you would need either `Mutex<JoinSet>` (tokio Mutex this time) or restructure ownership. If restructuring is too invasive, a `tokio::sync::Mutex<JoinSet>` is still better than `std::sync::Mutex<Vec<JoinHandle>>` since the lock is held only briefly and it avoids poison. Alternatively, keep `&self` on spawn and use `Arc<tokio::sync::Mutex<JoinSet<()>>>`.

### 2. Completed-handle accumulation: handles never pruned from `task_handles`
- **What**: Every call to `spawn` pushes a `JoinHandle` into the vec, but completed handles are never removed. For long-running runtimes with many short-lived processes, this leaks memory proportional to total spawned processes (not just live ones).
- **Where**: `Runtime::spawn()` line 171, `Runtime::shutdown()` line 190.
- **Why**: In a long-lived runtime (e.g., a server that spawns thousands of request-handler processes), the vec grows unboundedly. Each completed `JoinHandle` is small but the vec itself is never shrunk.
- **Trade-offs**: Solved for free by adopting `JoinSet` (opportunity #1). If not using `JoinSet`, periodic pruning (e.g., retain only handles that are not finished) adds complexity.

### 3. `shutdown` sequential join: use concurrent awaiting
- **What**: `shutdown()` awaits handles one at a time in a `for` loop. If handle N is slow but N+1 through N+100 are already done, the timeout clock ticks while waiting on N unnecessarily.
- **Where**: `Runtime::shutdown()` lines 191-196.
- **Why**: With sequential joins, the effective timeout is per-remaining-handle rather than total. A single slow process could consume the entire timeout budget before other handles are checked. Using `futures::future::join_all` or `JoinSet::join_next` in a loop would wait concurrently.
- **Trade-offs**: Minor: `join_all` requires pulling in `futures` or using `JoinSet`. The sequential approach is technically correct (all handles will complete or the outer timeout fires), but the timeout semantics are slightly surprising.

### 4. Missing test: shutdown timeout actually fires
- **What**: No test verifies that `shutdown` returns within the timeout when a process ignores `cancelled()` and keeps running.
- **Where**: New test in `runtime.rs` tests module.
- **Why**: The timeout path is the most critical safety net in graceful shutdown. Without a test, a regression (e.g., accidentally removing the `tokio::time::timeout` wrapper) would go undetected.
- **Trade-offs**: None significant. The test would spawn a process that ignores cancellation, call `shutdown` with a short timeout, and assert it returns promptly.

### 5. Missing test: spawn-after-shutdown behavior
- **What**: After `shutdown(self, ...)` consumes the runtime, no further spawns are possible (ownership moved). But if cancellation is triggered and `shutdown` is not yet called, `spawn` still works, creating a process with an already-cancelled token.
- **Where**: `Runtime::spawn()`, conceptual test.
- **Why**: It would be good to document (via test) what happens if you cancel the token manually or if there is a race between spawn and shutdown. Currently `shutdown` takes `self` so the race is prevented at the type level, which is good. This is more of a documentation-via-test opportunity.
- **Trade-offs**: Low value since the type system already prevents the most dangerous case.

### 6. `with_mailbox_capacity(0)` is silently accepted
- **What**: `with_mailbox_capacity(0)` creates a zero-capacity bounded channel, which means no message can ever be buffered. Depending on `tokio::sync::mpsc::channel(0)` behavior (it panics), this could be a runtime crash.
- **Where**: `Runtime::with_mailbox_capacity()`.
- **Why**: A zero-capacity mailbox is almost certainly a bug. Either assert/panic at construction time with a clear message, or document the minimum as 1.
- **Trade-offs**: Adding a runtime check (`assert!(capacity > 0)` or returning `Result`) is trivial.

### 7. Constructor duplication between `new` and `with_router`
- **What**: `new()` and `with_router()` both manually construct `Self { ... }` with identical boilerplate for `cancel_token`, `task_handles`, and `default_mailbox_capacity`. Adding a new field requires updating both.
- **Where**: `Runtime::new()` and `Runtime::with_router()`.
- **Why**: Have `new()` delegate to `with_router()` to eliminate the duplication.
- **Trade-offs**: Trivial refactor, very low risk.

## Execution Protocol
**DO NOT implement any changes without user approval.**
For EACH opportunity, use `AskUserQuestion`.
Options: "Implement" / "Skip (add to TODO.md)" / "Do not implement"
Ask all questions before beginning any implementation work.
After all items resolved, run tests: `cargo test --workspace`
