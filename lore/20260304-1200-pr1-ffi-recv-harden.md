# PR1 Harden: FFI recv, stop_process, and client message loops

## Context
PR1 (`pr1-ffi-recv`) adds `rebar_recv`, `rebar_stop_process`, and `rebar_unregister` to the FFI layer, plus corresponding client wrappers (Go, Python, TypeScript) with actor message loops. Review identified several issues ranging from a missing Rust implementation to thread-safety concerns and code quality.

## Effort/Impact Table

| # | Opportunity | Effort | Impact | Action |
|---|-------------|--------|--------|--------|
| 1 | Missing `rebar_unregister` Rust implementation | Quick | High | Auto-fix |
| 2 | Go errors.go: misplaced doc comment for `InvalidNameError` | Quick | Low | Auto-fix |
| 3 | Go errors.go: `errTimeout` alignment (tab vs spaces) | Quick | Low | Auto-fix |
| 4 | Go `Recv` returns `TimeoutError` instead of `nil` on timeout | Quick | Medium | Auto-fix |
| 5 | Python `recv` has unused `TimeoutError` import | Quick | Low | Auto-fix |
| 6 | Go actor message loop has no graceful shutdown mechanism | Easy | Medium | Ask first |
| 7 | Python actor message loop has no graceful shutdown mechanism | Easy | Medium | Ask first |
| 8 | TypeScript actor message loop has no graceful shutdown (and uses sync `recv` in async loop) | Easy | Medium | Ask first |
| 9 | `rebar_recv` remove-and-reinsert pattern is not thread-safe for concurrent recv on same PID | Moderate | High | Ask first |
| 10 | Add FFI test for `rebar_unregister` | Easy | Medium | Auto-fix |

## Opportunity Details

### 1. Missing `rebar_unregister` Rust implementation
- **What**: `rebar_unregister` is declared in the C header and used by all 3 clients (Go, Python, TypeScript) but has no implementation in `crates/rebar-ffi/src/lib.rs`. This will cause a linker error.
- **Where**: `crates/rebar-ffi/src/lib.rs` (add after `rebar_register`)
- **Why**: Correctness. Clients will crash at link time.

### 2. Go errors.go: misplaced doc comment
- **What**: The doc comment `// InvalidNameError is returned when a name is not valid UTF-8.` sits above `TimeoutError`, not `InvalidNameError`. The `TimeoutError` type was inserted between the comment and `InvalidNameError`.
- **Where**: `clients/go/rebar/errors.go:37-43`
- **Why**: Misleading docs.

### 3. Go errors.go: `errTimeout` alignment
- **What**: `errTimeout = -5` uses spaces instead of tab alignment matching the other constants.
- **Where**: `clients/go/rebar/errors.go:14`
- **Why**: Inconsistent formatting.

### 4. Go `Recv` returns `TimeoutError` instead of `nil` on timeout
- **What**: `Runtime.Recv()` calls `checkError(rc)` which throws `TimeoutError` for rc=-5. But the Go `actor.go` message loop checks `errTimeout` directly on the raw C return code, bypassing this. The public `Recv()` API should match Python/TS behavior and return `(nil, nil)` on timeout instead of an error.
- **Where**: `clients/go/rebar/runtime.go:102-113`
- **Why**: API inconsistency across clients. Python/TS return nil/None on timeout; Go throws an error.

### 5. Python `recv` has unused `TimeoutError` import
- **What**: `TimeoutError` is imported in `runtime.py` but `recv()` handles rc=-5 with a magic number check and returns `None` directly, never raising `TimeoutError`.
- **Where**: `clients/python/rebar/runtime.py:10`
- **Why**: Dead import.

### 6. Go actor message loop has no graceful shutdown
- **What**: The goroutine in `goRebarProcessCallback` loops forever polling `rebar_recv` with 100ms timeout. When the process is stopped, `rebar_recv` returns `REBAR_ERR_NOT_FOUND` (caught by `int(rc) != errOK`), which does exit the loop. However, the goroutine is never cleaned up from the `actorMap`, and there's no way to signal a clean stop from Go.
- **Where**: `clients/go/rebar/actor.go:95-118`
- **Why**: Resource leak — `actorMap` entries accumulate forever. Minor concern: if `HandleMessage` panics, the goroutine silently dies.
- **Trade-offs**: Adds complexity. The current behavior is "good enough" for short-lived processes.

### 7. Python actor message loop has no graceful shutdown
- **What**: The daemon thread in `spawn_actor` loops forever. When the process is stopped, `recv()` will raise a non-timeout error (NotFoundError from `check_error`), which is unhandled and will crash the thread silently.
- **Where**: `clients/python/rebar/runtime.py:167-176`
- **Why**: Silent crash on stop. Should catch `NotFoundError` and exit cleanly.

### 8. TypeScript actor message loop has no graceful shutdown
- **What**: The async IIFE loops forever calling `runtimeRef.recv(pid, 100n)`. When stopped, `recv` throws (via `checkError`), and the unhandled rejection propagates. Also, using synchronous `block_on` in `rebar_recv` from an async context is architecturally odd but functional with Deno FFI.
- **Where**: `clients/typescript/src/runtime.ts:228-238`
- **Why**: Unhandled rejection on process stop.

### 9. `rebar_recv` remove-and-reinsert pattern is not thread-safe for concurrent recv on same PID
- **What**: `rebar_recv` removes the `MailboxRx` from the map, uses it, then reinserts. If two threads call `rebar_recv` for the same PID concurrently, the second call sees no entry and returns `REBAR_ERR_NOT_FOUND`. This is a correctness bug.
- **Where**: `crates/rebar-ffi/src/lib.rs:243-264`
- **Why**: The remove-use-reinsert pattern creates a window where the mailbox is "missing." While single-threaded use works fine, FFI consumers (especially Go with goroutines) could hit this.
- **Trade-offs**: Fix requires either keeping the `MailboxRx` in the map behind an `Arc<Mutex<MailboxRx>>`, or documenting that concurrent recv on same PID is unsupported.

### 10. Add FFI test for `rebar_unregister`
- **What**: Add a test that registers a name, unregisters it, and verifies `whereis` returns `REBAR_ERR_NOT_FOUND`.
- **Where**: `crates/rebar-ffi/src/lib.rs` (tests module)
- **Why**: Test coverage for the new function from item #1.

## Execution Protocol
**DO NOT implement any changes without user approval.**
For EACH opportunity, use `AskUserQuestion`.
Options: "Implement" / "Skip (add to TODO.md)" / "Do not implement"
Ask all questions before beginning any implementation work
(do NOT do alternating ask then implement, ask then implement, etc.)
After all items resolved, run: `cargo test -p rebar-ffi`
