# Go Client Race Condition Fixes

## Context
The Go client (`clients/go/rebar/`) wraps `librebar_ffi` via cgo. Multiple data races
and concurrency bugs were identified via code review.

## Effort/Impact Table

| # | Opportunity | Effort | Impact | Action |
|---|-------------|--------|--------|--------|
| 1 | SpawnActor race on `activeActorID` | Quick | High | Implemented |
| 2 | `Runtime.Close()` double-free | Quick | High | Implemented |
| 3 | Runtime methods use-after-free | Easy | High | Implemented |
| 4 | `actorMap` never cleaned up | Quick | Medium | Implemented |
| 5 | Empty-name panic in Register/Whereis/SendNamed | Quick | Medium | Implemented |
| 6 | Missing concurrency tests | Easy | High | Implemented |
| 7 | `checkError` panics on null-ptr | Quick | Medium | Implemented |

## Changes Made

### #1 SpawnActor race fix
- `actorMu` held across both `activeActorID` write and `rebar_spawn_go` C call
- Callback no longer takes lock (it's already held by caller)

### #2 + #3 Runtime close safety
- Added `sync.Once` to `Close()` for idempotent, thread-safe close
- Added `sync.RWMutex` to `Runtime` — `Close()` takes write lock, all methods take read lock
- `acquire()` helper checks `closed` flag under read lock
- All methods return `ErrRuntimeClosed` after close

### #4 actorMap cleanup
- Added `unregisterActor(id)` — deletes entry from map
- Called on spawn failure

### #5 Empty-name guard
- `Register`, `Whereis`, `SendNamed` return `InvalidNameError` for empty name
- Prevents `&nameBytes[0]` panic on zero-length slice

### #6 Concurrency tests
- `TestConcurrentClose` — 10 goroutines close simultaneously
- `TestUseAfterClose` — all methods return `ErrRuntimeClosed`
- `TestConcurrentSpawnActor` — 10 concurrent spawns, verify unique PIDs
- `TestConcurrentSend` — 10 concurrent sends to invalid PIDs
- `TestEmptyNameErrors` — empty name returns `InvalidNameError`
- `TestCheckErrorNullPtr` — null-ptr returns error instead of panic

### #7 checkError no-panic
- `errNullPtr` returns `*RebarError` instead of panicking

## Execution Protocol
**DO NOT implement any changes without user approval.**
For EACH opportunity, use `AskUserQuestion`.
Options: "Implement" / "Skip (add to TODO.md)" / "Do not implement"
Ask all questions before beginning any implementation work
(do NOT do alternating ask then implement, ask then implement, etc.)
After all items resolved, run: `poetry run pytest`
