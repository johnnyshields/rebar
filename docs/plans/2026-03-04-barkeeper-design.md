# barkeeper — etcd-compatible Distributed KV Store on Rebar

**Date:** 2026-03-04
**Status:** Approved
**Repository:** `alexandernicholson/barkeeper` (new, separate repo)
**License:** Apache 2.0 (matching etcd)

## Overview

barkeeper is an etcd-compatible distributed key-value store built entirely on the Rebar actor runtime. It implements the full etcd v3 gRPC API plus an HTTP/JSON gateway, providing a drop-in replacement for etcd backed by Raft consensus implemented as Rebar actors and redb (pure Rust) for persistent storage.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Compatibility | etcd v3 gRPC API + HTTP gateway | Drop-in replacement for etcd clients |
| Consistency | Full linearizable | Matches etcd's default, required for compatibility |
| Raft | Built from scratch as Rebar actors | Showcase of Rebar's capabilities, deep integration |
| Storage | redb (pure Rust) | No C dependencies, clean build |
| Architecture | Actor-Per-Responsibility | Natural actor model fit, fault isolation per subsystem |
| Repo layout | Separate repo | barkeeper is an application built ON rebar |
| etcd protos | Vendored from etcd-io/etcd | Apache 2.0, proper attribution required |

## Architecture

```
                    ┌─────────────────────────────────────┐
                    │           barkeeper node             │
                    │                                      │
  gRPC/HTTP ───────►│  GrpcFrontend ──► RaftNode           │
  clients           │                    │                 │
                    │              ┌─────┼─────┐           │
                    │              ▼     ▼     ▼           │
                    │         LogStore  SM  Elections       │
                    │           (redb) (redb)               │
                    │                                      │
                    │  WatchHub    LeaseManager    Auth     │
                    │                                      │
                    │  ─── Rebar DistributedRouter ───     │
                    │  ─── SWIM membership ────────────    │
                    └─────────────────────────────────────┘
                         ▲              ▲
                    QUIC │              │ QUIC
                         ▼              ▼
                    [other barkeeper nodes]
```

### Actors

| Actor | Responsibility |
|-------|---------------|
| GrpcFrontend | Accepts tonic gRPC + axum HTTP, routes writes to Raft leader, serves reads |
| RaftNode | Raft state machine (Leader/Follower/Candidate), manages log, sends RPCs via Rebar messages |
| LogStore | Persists Raft log entries to redb WAL, fsync for durability |
| StateMachine | Applies committed entries to KV redb store (MVCC with revision numbers) |
| WatchHub | Manages watch streams, notified on every committed KV mutation |
| LeaseManager | TTL-based leases, grants/revokes/keepalives |
| AuthManager | Users, roles, permissions (etcd v3 auth) |
| ClusterManager | Bridges SWIM membership and Raft membership |

### Rebar Integration Points

- **Raft RPCs** (AppendEntries, RequestVote, InstallSnapshot) are Rebar messages via DistributedRouter
- **SWIM** discovers new nodes → Raft adds them as learners
- **Supervisor trees** restart crashed actors (e.g. WatchHub crash doesn't affect consensus)
- **Graceful drain** triggers Raft leadership transfer before shutdown
- **QUIC transport** for low-latency peer communication

## Raft Consensus

### RaftNode Actor

The central consensus coordinator. Holds current `term`, `voted_for`, `commit_index`, `last_applied`. Transitions between Leader/Follower/Candidate states internally (single actor with state enum — state transitions must be atomic).

**Inbound messages:**
- `AppendEntries` / `RequestVote` / `InstallSnapshot` — from peer RaftNodes via DistributedRouter
- `ClientProposal` — from GrpcFrontend (write requests)
- `ElectionTimeout` — from election timer helper process
- `HeartbeatTick` — periodic (leader only)

**Outbound messages:**
- Raft RPCs to peer RaftNodes
- Apply commands to StateMachine
- Responses to GrpcFrontend

### Election Timer

Spawns a helper process that sends `ElectionTimeout` messages at randomized intervals (150-300ms, matching etcd defaults). Reset on every valid `AppendEntries` or `RequestVote` grant.

### LogStore Actor

Wraps a redb database for Raft log entries.

**Operations:**
- `Append(entries)` — persist new log entries (fsync)
- `GetRange(start..end)` — read entries for replication
- `Truncate(after_index)` — remove conflicting entries
- `LastIndex` / `LastTerm` — for consistency checks
- `SaveSnapshot(data)` / `LoadSnapshot()` — for snapshot transfer

### Membership Changes

- Single-node changes only (etcd's approach — safer than joint consensus)
- Add: learner first, then promote to voter once caught up
- Remove: if removing leader, transfer leadership first, then step down

## KV Store — MVCC Data Model

### redb Tables

**KV table** — `(key_bytes, revision) → KeyValue`:
```rust
struct KeyValue {
    key: Vec<u8>,
    value: Vec<u8>,
    create_revision: i64,   // revision when key was first created
    mod_revision: i64,      // revision of this mutation
    version: i64,           // modification count (resets on delete+recreate)
    lease_id: i64,          // 0 if no lease attached
}
```

**Revision table** — `revision → Vec<(key, EventType)>`:
- Used by Watch: "give me all changes since revision N"
- Used by Compaction: delete all entries before revision N
- EventType: PUT or DELETE

### Global State

- `current_revision: i64` — monotonically increasing, incremented per transaction
- `compact_revision: i64` — oldest queryable revision

### Operations (etcd v3 API)

| Operation | Description |
|-----------|-------------|
| Range | Get key or key range, optionally at a specific revision, with sorting/limit/pagination |
| Put | Insert/update key, bumps revision, optionally returns previous value |
| DeleteRange | Delete key or range, creates tombstone entries at new revision |
| Txn | Atomic compare-and-swap: `if (conditions) then (ops) else (ops)` |
| Compact | Remove all revisions before a given revision to reclaim space |

### StateMachine Flow

1. Receives committed log entry from RaftNode
2. Begins redb write transaction
3. Applies operation(s), increments revision
4. Commits transaction
5. Sends `WatchEvent` to WatchHub
6. Responds to the original client proposal

## Watch Service

### WatchHub Actor

Maintains a registry of active watchers: `HashMap<WatchId, WatchState>`. Each watcher tracks key/range filter, start revision, and whether it wants `prev_kv`.

**Watch flow:**
1. Client opens `Watch` gRPC stream → GrpcFrontend → WatchHub receives `CreateWatch`
2. If `start_revision` set, replays historical events from revision table
3. On each committed mutation, StateMachine sends `WatchEvent` to WatchHub
4. WatchHub matches against registered watchers and pushes to client streams

**etcd compatibility:**
- Watch IDs are per-stream, server-assigned
- Multiplexed watches on a single stream
- Fragmented events for large responses
- Progress notifications (periodic empty events)
- Compaction check: if watcher's revision is compacted, send `Compacted` error

**Fault isolation:** WatchHub crash → supervisor restarts → resumes from connected streams' last sent revision. Consensus unaffected.

## Lease Service

### LeaseManager Actor

Maintains `HashMap<LeaseId, LeaseState>`. Lease operations are Raft-replicated (Grant, Revoke, KeepAlive go through consensus).

**Lease flow:**
1. `LeaseGrant { ttl }` → RaftNode proposal → committed → lease created
2. `Put { key, lease_id }` → key attached to lease
3. Client sends periodic `LeaseKeepAlive` to reset TTL
4. TTL expires → LeaseManager submits `LeaseRevoke` → all attached keys deleted

**Leader-only concern:** Only the leader's LeaseManager actively tracks TTLs. Follower LeaseManagers maintain state from applied log entries but don't run expiry timers. On leadership change, new leader picks up from replicated state.

## gRPC API + HTTP Gateway

### etcd v3 Services

| Service | RPCs | Priority |
|---------|------|----------|
| KV | Range, Put, DeleteRange, Txn, Compact | Must have |
| Watch | Watch (bidirectional stream) | Must have |
| Lease | LeaseGrant, LeaseRevoke, LeaseKeepAlive, LeaseTimeToLive, LeaseLeases | Must have |
| Cluster | MemberAdd, MemberRemove, MemberUpdate, MemberList, MemberPromote | Must have |
| Maintenance | Alarm, Status, Defragment, Hash, HashKV, Snapshot, MoveLeader | Should have |
| Auth | Full user/role/permission RBAC | Should have |

### HTTP Gateway

- `axum` server mapping HTTP/JSON to gRPC (same endpoints as etcd's grpc-gateway)
- `POST /v3/kv/range` → `KV.Range`, `POST /v3/kv/put` → `KV.Put`, etc.
- Supports JSON and protobuf content types

### Request Routing

- **Reads** → serve locally with linearizable read index, or forward to leader
- **Writes** → always forward to leader RaftNode as proposals
- **Streams** → handled by WatchHub / LeaseManager directly

### Ports (matching etcd defaults)

- 2379 — client gRPC + HTTP gateway
- 2380 — peer communication (Rebar QUIC/TCP transport)

## Cluster Management

### Node Lifecycle

1. New node starts, joins SWIM cluster via `--initial-cluster` peers
2. SWIM discovers existing nodes → ClusterManager learns about peers
3. ClusterManager sends `MemberAdd` proposal to Raft leader
4. Leader commits membership change → all nodes update Raft peer list
5. New node starts as Raft learner, catches up via `InstallSnapshot` + log replication
6. Once caught up, leader promotes learner to voting member

### Graceful Shutdown (via Rebar drain)

1. If leader → transfer leadership via MoveLeader
2. Stop accepting new client connections
3. Finish in-flight proposals
4. Leave Raft group cleanly
5. SWIM Leave announcement

### CLI Flags (matching etcd)

- `--name` — node name
- `--data-dir` — redb storage directory
- `--listen-client-urls` — gRPC/HTTP listen address (default `http://localhost:2379`)
- `--initial-cluster` — comma-separated `name=peer_addr` list
- `--initial-cluster-state` — `new` or `existing`

## Project Structure

```
barkeeper/
├── Cargo.toml
├── LICENSE                    # Apache 2.0
├── src/
│   ├── main.rs               # CLI, config parsing, node startup
│   ├── config.rs             # etcd-compatible CLI flags
│   ├── raft/
│   │   ├── mod.rs
│   │   ├── node.rs           # RaftNode actor
│   │   ├── log.rs            # LogStore actor (redb WAL)
│   │   ├── state_machine.rs  # StateMachine actor (KV apply)
│   │   ├── messages.rs       # AppendEntries, RequestVote, etc.
│   │   └── election.rs       # Election timer helper
│   ├── kv/
│   │   ├── mod.rs
│   │   ├── store.rs          # MVCC KV store (redb tables)
│   │   ├── txn.rs            # Transaction (compare-and-swap)
│   │   └── compact.rs        # Compaction logic
│   ├── watch/
│   │   ├── mod.rs
│   │   └── hub.rs            # WatchHub actor
│   ├── lease/
│   │   ├── mod.rs
│   │   └── manager.rs        # LeaseManager actor
│   ├── auth/
│   │   ├── mod.rs
│   │   └── manager.rs        # AuthManager actor
│   ├── cluster/
│   │   ├── mod.rs
│   │   └── manager.rs        # ClusterManager (SWIM → Raft bridge)
│   └── api/
│       ├── mod.rs
│       ├── grpc.rs           # tonic gRPC services
│       └── gateway.rs        # axum HTTP/JSON gateway
├── proto/
│   └── etcdserverpb/         # Vendored etcd v3 protobuf definitions (Apache 2.0)
├── tests/
│   ├── raft_test.rs
│   ├── kv_test.rs
│   ├── watch_test.rs
│   ├── integration_test.rs
│   └── compat_test.rs        # etcd client compatibility tests
└── README.md
```

## Dependencies

| Crate | Purpose |
|-------|---------|
| `rebar` | Actor runtime, DistributedRouter, SWIM, QUIC transport |
| `redb` | Persistent storage (WAL + KV) |
| `tonic` | gRPC server |
| `prost` | Protobuf codegen from etcd .proto files |
| `axum` | HTTP gateway |
| `clap` | CLI argument parsing |
| `tokio` | Async runtime (required by rebar) |
| `tracing` | Structured logging |

## Licensing

barkeeper is licensed under Apache 2.0, matching etcd. The vendored etcd `.proto` files from `etcd-io/etcd/api/etcdserverpb/` are Apache 2.0 licensed and include proper attribution in `proto/LICENSE`.
