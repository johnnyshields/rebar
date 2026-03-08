# Baseline Performance Analysis — 2026-03-08

## Criterion Results Summary

### Message Passing
| Benchmark | Time | Throughput |
|-----------|------|------------|
| ping_pong/100 | 42.0 µs | 2.38 Melem/s |
| ping_pong/1000 | 267.3 µs | 3.74 Melem/s |
| ping_pong/10000 | 2.52 ms | 3.96 Melem/s |
| fan_out/10 | 52.0 µs | 192 Kelem/s |
| fan_out/50 | 136.9 µs | 365 Kelem/s |
| fan_out/100 | 258.9 µs | 386 Kelem/s |
| fan_in/10 | 55.9 µs | 179 Kelem/s |
| fan_in/50 | 153.8 µs | 325 Kelem/s |
| fan_in/100 | 308.7 µs | 324 Kelem/s |
| message_size/nil | 36.9 µs | — |
| message_size/small_string | 37.4 µs | — |
| message_size/1kb_binary | 37.8 µs | — |
| message_size/64kb_binary | 40.6 µs | — |

### Process Lifecycle
| Benchmark | Time | Throughput |
|-----------|------|------------|
| spawn_single | 37.4 µs | — |
| spawn_batch/10 | 50.9 µs | 197 Kelem/s |
| spawn_batch/100 | 325.8 µs | 307 Kelem/s |
| spawn_batch/1000 | 3.26 ms | 307 Kelem/s |
| spawn_teardown/10 | 38.6 µs | 259 Kelem/s |
| spawn_teardown/100 | 291.4 µs | 343 Kelem/s |
| spawn_teardown/500 | 1.46 ms | 343 Kelem/s |
| spawn_and_send | 36.7 µs | — |

### Process Table (raw operations, no tokio)
| Benchmark | Time |
|-----------|------|
| allocate_pid/single_thread | 5.4 ns |
| allocate_pid/contended/2 | 92.4 µs (100 ops/thread) |
| allocate_pid/contended/4 | 145.4 µs |
| allocate_pid/contended/8 | 255.7 µs |
| insert/100 | 43.5 µs (435 ns/op) |
| insert/1000 | 514.3 µs (514 ns/op) |
| insert/10000 | 17.9 ms (1.79 µs/op) |
| lookup/get/100 | 37.7 ns |
| lookup/get/1000 | 36.3 ns |
| lookup/get/10000 | 38.7 ns |
| send/100 | 113.1 ns |
| send/1000 | 114.4 ns |
| concurrent_rw/2 | 190.3 µs |
| concurrent_rw/4 | 284.1 µs |
| concurrent_rw/8 | 495.5 µs |

### Supervisor
| Benchmark | Time | Throughput |
|-----------|------|------------|
| startup/1 | 11.35 ms | 88 elem/s |
| startup/5 | 11.25 ms | 445 elem/s |
| startup/10 | 11.30 ms | 885 elem/s |
| startup/50 | 11.26 ms | 4.4 Kelem/s |
| add_child/1 | 11.43 ms | 88 elem/s |
| add_child/10 | 11.43 ms | 875 elem/s |
| add_child/50 | 12.75 ms | 3.9 Kelem/s |
| restart_one_for_one | 11.36 ms | — |

## Key Observations

### Good Performance
- **Process table lookup is excellent**: ~37 ns regardless of table size (100-10k). DashMap scales well for reads.
- **PID allocation is fast**: 5.4 ns single-threaded (AtomicU64 fetch_add).
- **Message size has minimal impact**: nil (36.9 µs) vs 64KB binary (40.6 µs) — only 10% difference. Tokio channels handle large payloads well since they move ownership, not copy.
- **Ping-pong throughput scales well**: 2.4M→4.0M elem/s as batch size increases, showing amortised overhead.

### Performance Concerns
- **Supervisor operations are dominated by the 10ms sleep**: All supervisor benchmarks cluster around 11.2-11.4ms regardless of child count. The `tokio::time::sleep(Duration::from_millis(10))` in the shutdown path is the bottleneck — not the actual supervisor logic.
- **Spawn has ~37 µs overhead**: Each process spawn costs ~37 µs (allocate PID + create mailbox + insert into table + tokio::spawn wrapper + tokio::spawn inner). The double-spawn for panic isolation is a significant contributor.
- **Insert degrades at scale**: 435 ns/op at 100 entries → 1.79 µs/op at 10k entries (4x degradation). DashMap's hash map grows and rehashes.
- **Fan-out/fan-in per-process cost**: ~5 µs/process for fan-out, ~5.5 µs for fan-in. The per-process spawn overhead dominates at small N.

## Top 5 Optimisation Opportunities

### 1. Double-spawn overhead in Runtime::spawn
**Evidence:** spawn_single takes 37 µs. The double-spawn pattern (outer tokio::spawn + inner tokio::spawn for panic isolation) allocates two tasks, two JoinHandles, and requires two scheduler round-trips. A single spawn with `std::panic::catch_unwind` on the future output could halve the task allocation cost.
**Impact:** High — spawn is the most common operation. Every process creation and every supervised child restart pays this cost.

### 2. Supervisor shutdown sleep dominates benchmarks
**Evidence:** All supervisor benchmarks are ~11.3ms regardless of child count (1 vs 50 children). The `tokio::time::sleep(Duration::from_millis(10))` in the bench teardown masks the actual supervisor performance. Additionally, the `stop_child` function has a `tokio::time::sleep(Duration::from_millis(1).min(*duration))` and a `yield_now()` — these are artificial delays that add latency to every restart.
**Impact:** Medium — affects restart latency and shutdown time. Replacing sleep-based synchronisation with proper channel-based completion notification would eliminate these delays.

### 3. ProcessTable insert allocation overhead at scale
**Evidence:** Insert cost grows from 435 ns/op (100 entries) to 1.79 µs/op (10k entries) — a 4x degradation. Each insert creates a new Mailbox (allocates tokio mpsc channel internals). At scale, DashMap shard contention and hash map growth contribute.
**Impact:** Medium — affects batch process creation and systems with many concurrent processes. Pre-sizing the DashMap with `with_capacity()` would reduce rehashing. Pooling mailbox allocations could reduce allocation pressure.

### 4. Message creation timestamp overhead
**Evidence:** Every `Message::new()` calls `SystemTime::now().duration_since(UNIX_EPOCH)` which is a syscall. At 113 ns per send (process_table/send), the timestamp likely accounts for 20-30% of the cost. For internal messages where timestamps aren't needed, this is unnecessary work.
**Impact:** Medium — affects all message passing throughput. Making timestamp optional or using a cheaper clock (TSC-based or cached) would improve send throughput.

### 5. Arc clone overhead in message routing
**Evidence:** Every `Runtime::spawn` clones `Arc<ProcessTable>` and `Arc<dyn MessageRouter>`. Every `ProcessContext::send` goes through `Arc<dyn MessageRouter>` which involves vtable dispatch + DashMap lookup. The fan-in benchmark (where N processes each send one message) shows ~3 µs overhead per send at the Runtime level vs ~113 ns at the raw ProcessTable level — the difference is Arc + vtable + router abstraction overhead.
**Impact:** Low-Medium — the absolute cost is small (~3 µs per send), but for high-throughput actor systems processing millions of messages, this adds up. Using a concrete router type instead of `dyn MessageRouter` for the common case would eliminate vtable dispatch.
