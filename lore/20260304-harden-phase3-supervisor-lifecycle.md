# Harden: Phase 3 — Fix Supervisor Child Lifecycle

## Changes Reviewed
This PR replaces the hacky static `AtomicU64` PID counter (starting at 1,000,000) in `start_child` with proper PID allocation from the runtime's `ProcessTable`. It also adds `JoinHandle` tracking to `ChildState` so that `stop_child` can actually await task completion instead of relying on `yield_now()` or hardcoded sleeps. Two new tests validate both behaviors.

## Effort/Impact Table

| # | Opportunity | Effort | Impact | Action |
|---|-------------|--------|--------|--------|
| 1 | `BrutalKill` should abort the task, not just drop the sender | Quick | High | Ask first |
| 2 | Duplicated `ChildState` construction (3 sites) | Quick | Low | Ask first |
| 3 | `stop_child` BrutalKill + Timeout share redundant join_handle fallback logic | Quick | Medium | Ask first |
| 4 | Stale comment in `start_supervisor` about PID allocation | Quick | Low | Auto-fix |
| 5 | `stop_child` Timeout path does early return, skipping the final `child.pid = None` in certain flows | Quick | High | Ask first |
| 6 | No test for `BrutalKill` with `join_handle` awaiting | Quick | Medium | Ask first |
| 7 | Children spawned by supervisor are not registered in `ProcessTable` (only PID allocated) | Moderate | High | Ask first |

## Opportunity Details

### 1. `BrutalKill` should abort the task, not just drop the sender
- **What**: `BrutalKill` currently drops the `oneshot::Sender`, which sends a "closed" signal on `shutdown_rx`. The child task then hits the `shutdown_rx` branch of `tokio::select!` and sends a `ChildExited` message. But this is semantically identical to `Timeout` -- the child gets a graceful signal either way. For `BrutalKill`, the task should be `abort()`ed via its `JoinHandle`.
- **Where**: `stop_child()` in `engine.rs`, the `BrutalKill` match arm.
- **Why**: `BrutalKill` is supposed to be a hard kill. Currently it just drops a channel, which is indistinguishable from a graceful shutdown from the child's perspective. With the new `join_handle` field, we can call `handle.abort()` for true brutal kill semantics.
- **Trade-offs**: `abort()` is cancellation-unsafe in the general case, but that is exactly what `BrutalKill` should mean. The child task's `ChildExited` message may not be sent (since the task is aborted), so the supervisor must handle the `JoinError::Cancelled` case or simply accept that the child is gone.

### 2. Duplicated `ChildState` construction (3 sites)
- **What**: `ChildState { spec, factory, pid: None, shutdown_tx: None, join_handle: None }` is constructed identically in three places: initial children loop, `AddChild` handler, and would appear in any future code path. A `ChildState::new(spec, factory)` or `From<ChildEntry>` impl would consolidate this.
- **Where**: `supervisor_loop()` lines 141-147, and `AddChild` handler lines 220-226.
- **Why**: Reduces the chance of forgetting a field when `ChildState` gains new fields (which just happened with `join_handle`).
- **Trade-offs**: Minor -- adds one small method.

### 3. `stop_child` BrutalKill + Timeout share redundant join_handle fallback logic
- **What**: Both the `BrutalKill` path (which falls through to the bottom) and the "no handle" fallback at the bottom of `stop_child` do `tokio::time::timeout(100ms, handle).await`. The `Timeout` path has its own inline await. The control flow is tangled with an early `return` in the `Timeout` path and fallthrough for `BrutalKill`.
- **Where**: `stop_child()` in `engine.rs`.
- **Why**: The early return in the `Timeout` branch and the fallthrough for `BrutalKill` make the function harder to reason about. A cleaner structure would match on `(shutdown_strategy, has_handle)` more explicitly.
- **Trade-offs**: Cosmetic, but reduces future bugs.

### 4. Stale comment in `start_supervisor` about PID allocation
- **What**: Lines 108-111 still contain the old comments: "We need the supervisor PID. Spawn the supervisor as a tokio task (not via Runtime::spawn since that expects ProcessContext-based handler). Instead, we'll allocate a PID for it conceptually. Actually let's use Runtime::spawn to get a real PID." This stream-of-consciousness comment is now misleading since the code does use `Runtime::spawn`.
- **Where**: `start_supervisor()` lines 108-111.
- **Why**: Stale comments mislead readers.
- **Trade-offs**: None.

### 5. `stop_child` Timeout path inconsistency in `pid` clearing
- **What**: In the `Timeout` path, when a `join_handle` is present, the code sets `child.pid = None` and does an early `return`. But when there is no `join_handle` (e.g., it was already taken), it falls through to the bottom where `child.pid = None` is set again. The asymmetry is not a bug today, but the `Timeout` path without a handle silently falls through to the `BrutalKill` fallback (100ms timeout), which is semantically wrong for a Timeout strategy.
- **Where**: `stop_child()` lines 328-345.
- **Why**: If `join_handle` is `None` but `shutdown_tx` is `Some`, the Timeout path sends the shutdown signal but then falls through to the 100ms BrutalKill fallback instead of respecting the configured timeout duration.
- **Trade-offs**: None -- this is a correctness fix.

### 6. No test for `BrutalKill` with `join_handle` awaiting
- **What**: Test 26 (`stop_child_awaits_completion`) only tests the `Timeout` path. There is no unit test for `stop_child` with `BrutalKill` strategy verifying that the `join_handle` is awaited (or aborted, per opportunity #1).
- **Where**: Tests section of `engine.rs`.
- **Why**: The `BrutalKill` codepath in `stop_child` is untested at the unit level (test 18 `brutal_kill_immediate` is an integration test via the full supervisor, not a direct `stop_child` call).
- **Trade-offs**: None.

### 7. Children spawned by supervisor are not registered in `ProcessTable`
- **What**: `start_child` calls `table.allocate_pid()` to get a unique PID, but never calls `table.insert(pid, handle)`. The child process gets a valid PID from the table's counter, but is not actually registered in the table. This means `table.get(&child_pid)` returns `None`, and `table.send(child_pid, msg)` returns `ProcessDead`. The PID is effectively just a unique ID, not a routable address.
- **Where**: `start_child()` in `engine.rs`.
- **Why**: If other parts of the system try to send messages to supervisor children by PID (via the process table), they will fail. This breaks the actor model's location-transparent messaging. The supervisor-spawned children exist outside the process table, creating two classes of processes.
- **Trade-offs**: Registering children requires creating a mailbox for each child, which means changing the child factory signature or wrapping children with a mailbox. This is a larger architectural change that may belong in a separate PR.

## Execution Protocol
**DO NOT implement any changes without user approval.**
For EACH opportunity, use `AskUserQuestion`.
Options: "Implement" / "Skip (add to TODO.md)" / "Do not implement"
Ask all questions before beginning any implementation work
After all items resolved, run tests: `cargo test --workspace`
