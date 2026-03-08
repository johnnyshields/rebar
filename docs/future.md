# Rebar — Future Extensions

This document tracks potential improvements and open design questions for the Rebar runtime architecture.

## io_uring Fixed-Buffer Registration

compio-driver abstracts over io_uring but does not expose the underlying ring directly. This means we cannot register fixed buffers with `io_uring_register_buffers()` for true zero-copy I/O — reads and writes still go through the kernel's copy path even on Linux.

Options:
- Upstream PR to compio-driver exposing buffer registration APIs
- Fork compio-driver with a thin extension trait for fixed-buffer ops
- Bypass compio for the hot-path read/write and call io_uring directly via the turbine-core ring

## Adaptive Epoch Tuning

The `IouringBufferPool` (turbine-core) rotates buffer arenas on a fixed epoch interval. Under varying load, this can lead to either premature rotation (wasting partially-filled arenas) or delayed rotation (holding onto stale buffers too long).

Potential approach: monitor arena utilization at each epoch boundary. If utilization is consistently low, extend the rotation interval. If arenas are filling up before rotation, shorten it. This requires exposing utilization metrics from turbine-core.

## CPU Pinning / NUMA Awareness

`multi.rs` spawns N OS threads but does not pin them to specific CPU cores. On NUMA systems, this means a thread's memory allocations may land on a remote NUMA node, adding latency to every cache miss.

Improvements:
- Use `libc::sched_setaffinity` to pin each executor thread to a core
- Allocate per-thread arenas from the local NUMA node via `libc::set_mempolicy` or `numactl`
- Expose a `CoreAffinity` config in `RebarExecutor::Config`

## Work Stealing / Process Migration

All core types are `!Send`, which makes it impossible to migrate a process from one executor thread to another. Under uneven load, some threads may be saturated while others are idle.

Possible approaches:
- Explicit serialization: serialize a process's mailbox contents + state, send via crossbeam, deserialize on the target thread. Requires processes to opt in via a `Migratable` trait.
- Mailbox-only rebalancing: redistribute incoming messages across threads without moving the process itself (useful for stateless request handlers).
- Spawn-time load balancing: choose the least-loaded thread when spawning new processes (simpler, no migration needed).

## Tick Budget / Preemption

The executor uses a `tick_budget` of 128 — after 128 task polls in one tick, it yields to the Proactor for I/O events. However, a single CPU-bound GenServer that never yields can consume the entire tick budget, starving I/O and other tasks on the same thread.

Potential mitigations:
- Per-task reduction counting (Erlang-style): each task gets a budget of reductions, and the executor forcibly yields it after exhausting the budget
- Expose `yield_now()` as a first-class primitive that cooperatively returns control to the executor
- Detect long-running polls via timing and demote the offending task to a lower priority

## Cluster Migration to RebarExecutor

`rebar-cluster` still uses tokio for its transport layer (TCP, QUIC), connection management, and SWIM protocol ticks. Migrating it to `RebarExecutor` requires:

- Replacing `tokio::net::TcpListener`/`TcpStream` with the rebar-core I/O types
- Replacing `tokio::spawn` with executor task spawning
- Replacing `tokio::time::interval` with the executor timer queue
- Rethinking the `quinn` (QUIC) integration, which is deeply coupled to tokio
- Ensuring the transport layer can still do cross-thread work (cluster connections serve all threads)

This is a large effort and may not be worth doing until the cluster module needs features that only the custom executor provides.

## FFI Migration

`rebar-ffi` currently wraps the core runtime with an internal `tokio::runtime::Runtime` to bridge synchronous FFI calls to the async process model. This adds tokio as a transitive dependency for any FFI consumer.

Future option: expose `RebarExecutor` directly through the FFI boundary. The challenge is that `RebarExecutor::block_on()` is not reentrant — FFI callbacks that re-enter the runtime would need careful handling. A dedicated FFI executor thread with a command channel may be the right pattern.

## local-sync Replacement

The `local-sync` crate provides `!Send` mpsc channels used for process mailboxes. It originates from the monoio ecosystem and pulls in more dependencies than necessary for our use case.

Plan: vendor a minimal `!Send` mpsc implementation (unbounded, single-consumer) to eliminate the monoio ecosystem dependency. The core API surface needed is small: `Sender::send()`, `Receiver::recv()` (async), and `Receiver::try_recv()`.
