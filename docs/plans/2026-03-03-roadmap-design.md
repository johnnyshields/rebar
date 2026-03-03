# Rebar Roadmap Features Design

**Date:** 2026-03-03
**Features:** QUIC Transport, Distribution Layer Integration, Graceful Node Drain
**Approach:** TDD — tests written before implementation

---

## 1. QUIC Transport

### Overview

Replace TCP as the production transport using `quinn 0.11` (already a dependency). Uses a stream-per-frame model: each `send()` opens a new unidirectional QUIC stream, writes one encoded frame, and finishes the stream. Each `recv()` accepts an incoming stream and reads the frame. This eliminates length-prefixed framing and leverages QUIC's native multiplexing (no head-of-line blocking).

### Transparent Certificate Exchange

Nodes generate ephemeral self-signed certificates at startup. Certificate fingerprints are exchanged via SWIM gossip, eliminating manual cert management.

**Flow:**
1. Node starts → generates self-signed cert + keypair in memory
2. Computes `cert_hash: [u8; 32]` (SHA-256 of DER-encoded cert)
3. Includes `cert_hash` in SWIM `Alive` gossip updates
4. When connecting to a peer, looks up their `cert_hash` from the membership list
5. Custom `rustls::client::danger::ServerCertVerifier` accepts the cert only if its hash matches the SWIM-advertised hash
6. Both sides verify → mutual authentication without any CA

### Types

```
QuicTransport
├── cert: rustls::pki_types::CertificateDer
├── key: rustls::pki_types::PrivateKeyDer
├── cert_hash: [u8; 32]
├── listen(addr) → QuicListener
└── connect(addr, expected_cert_hash) → QuicConnection

QuicListener (impl TransportListener)
├── endpoint: quinn::Endpoint
├── local_addr() → SocketAddr
└── accept() → QuicConnection

QuicConnection (impl TransportConnection)
├── connection: quinn::Connection
├── send(frame) → open uni stream, write frame.encode(), finish
├── recv() → accept uni stream, read to end, Frame::decode()
└── close() → connection.close()

QuicTransportConnector (impl TransportConnector)
├── endpoint: quinn::Endpoint (client mode)
└── connect(addr) → QuicConnection
```

### SWIM Gossip Changes

`GossipUpdate::Alive` gains a `cert_hash: [u8; 32]` field. `Member` struct gains the same field. The membership list becomes the trust store.

### Tests (write first)

- `quic_send_recv_single_frame` — send one frame, receive it
- `quic_send_recv_multiple_frames` — interleaved sends, all arrive
- `quic_large_payload` — frame exceeding typical MTU
- `quic_connection_close` — clean shutdown
- `quic_listener_accept` — accept multiple connections
- `quic_connector_implements_trait` — ConnectionManager can use it
- `quic_concurrent_streams` — parallel sends don't block each other
- `quic_cert_fingerprint_verification` — connection succeeds with matching hash
- `quic_cert_fingerprint_mismatch` — connection rejected with wrong hash
- `quic_self_signed_cert_generation` — cert is valid, hash is deterministic for same cert

---

## 2. Distribution Layer Integration

### Overview

Bridge `rebar-core` (local processes) and `rebar-cluster` (networking) with a `MessageRouter` trait. When a process calls `send(pid, msg)`, the router checks if `pid.node_id() == self_node_id`. Local sends go to ProcessTable. Remote sends encode a `Frame(MsgType::Send)` and route through ConnectionManager. The crates stay decoupled — `rebar-core` defines the trait, `rebar-cluster` implements it.

### Types in rebar-core

```
trait MessageRouter: Send + Sync {
    fn route(&self, from: ProcessId, to: ProcessId, payload: Value)
        -> Result<(), SendError>;
}

LocalRouter (default impl)
├── table: Arc<ProcessTable>
└── route() → table.send()

SendError (new variant)
└── NodeUnreachable(u64)
```

### Changes to ProcessContext and Runtime

- `ProcessContext` holds `Arc<dyn MessageRouter>` instead of `Arc<ProcessTable>` directly
- `ProcessContext::send()` delegates to `router.route()`
- `Runtime::new()` accepts optional `Arc<dyn MessageRouter>`; defaults to `LocalRouter`
- Zero overhead for local-only users — `LocalRouter` is a thin wrapper

### Types in rebar-cluster

```
DistributedRouter (impl MessageRouter)
├── node_id: u64
├── table: Arc<ProcessTable>
├── connection_tx: mpsc::Sender<RouterCommand>
└── route()
    ├── if to.node_id() == self.node_id → table.send()
    └── else → encode Frame, send via connection_tx

RouterCommand
├── Send { node_id: u64, frame: Frame }
└── (future: Monitor, Link, etc.)
```

### Inbound Path

ConnectionManager receives `MsgType::Send` frames from remote nodes → extracts destination PID and payload from frame header → delivers to local ProcessTable via `table.send()`.

### Top-level rebar Crate

```
DistributedRuntime
├── runtime: Runtime (with DistributedRouter)
├── connection_manager: ConnectionManager
├── swim: SwimProtocol
├── registry: Registry
├── spawn() → runtime.spawn()
├── send() → runtime.send()
└── (wires inbound frames to ProcessTable)
```

### Tests (write first)

- `local_router_delivers_locally` — same behavior as current ProcessTable path
- `local_router_rejects_unknown_pid` — SendError::ProcessDead
- `distributed_router_routes_local` — local node_id goes to ProcessTable
- `distributed_router_routes_remote` — different node_id encodes Frame and sends
- `distributed_router_node_unreachable` — returns SendError::NodeUnreachable
- `remote_send_encodes_correct_frame` — MsgType::Send, dest PID in header, payload correct
- `inbound_frame_delivers_to_local_process` — receive Send frame, message arrives in mailbox
- `end_to_end_cross_node_send` — two runtimes, message from node 1 arrives at node 2

---

## 3. Graceful Node Drain

### Overview

Three-phase shutdown protocol: Announce → Drain → Shutdown. Mirrors BEAM's `init:stop()` behavior.

### Phase 1 — Announce (immediate)

- Broadcast `GossipUpdate::Leave { node_id, addr }` via SWIM gossip
- Unregister all names from Registry (`remove_by_node(self_node_id)`)
- Set node state to `Draining` — reject new incoming connections
- Other nodes receive Leave, remove this node from routing tables

### Phase 2 — Drain (configurable timeout, default 30s)

- Stop spawning new processes (Runtime rejects spawn requests)
- Wait for in-flight outbound messages to flush through ConnectionManager
- Wait for process mailboxes to empty, or until timeout expires
- Timeout is a hard cap — drain proceeds to phase 3 even if messages remain

### Phase 3 — Shutdown (ordered)

- Shut down supervisor trees using existing `ShutdownStrategy`
- Close all transport connections
- Stop SWIM protocol loop
- Drop runtime, deallocate all process handles

### Types

```
DrainConfig
├── announce_timeout: Duration  (default 5s — time to propagate Leave)
├── drain_timeout: Duration     (default 30s — time for in-flight messages)
└── shutdown_timeout: Duration  (default 10s — time for supervisor shutdown)

DrainResult
├── processes_stopped: usize
├── messages_drained: usize
├── phase_durations: [Duration; 3]
└── timed_out: bool

NodeDrain
├── config: DrainConfig
├── drain(runtime, connection_manager, swim, registry) → DrainResult
```

### Integration Points

- `DistributedRuntime::drain(config) → DrainResult` — public API
- ConnectionManager gets `drain()` method — stop accepting, flush pending, close all
- Runtime gets `set_draining()` — rejects new `spawn()` calls
- SWIM `Leave` already exists in `GossipUpdate` enum

### Tests (write first)

- `drain_broadcasts_leave` — SWIM gossip contains Leave update
- `drain_unregisters_names` — registry empty after phase 1
- `drain_rejects_new_connections` — incoming connects fail after announce
- `drain_rejects_new_spawns` — runtime.spawn() returns error after drain starts
- `drain_waits_for_inflight` — messages sent before drain are delivered
- `drain_respects_timeout` — proceeds to shutdown even if drain incomplete
- `drain_shuts_down_supervisors` — supervisor trees stop cleanly
- `drain_returns_stats` — DrainResult has accurate counts
- `end_to_end_drain` — node drains, other nodes see Leave, remote processes get NodeDown

---

## Implementation Order

1. **QUIC Transport** — standalone, no dependency on other features
2. **Distribution Layer** — needs a working transport (TCP suffices for testing, QUIC for production)
3. **Graceful Node Drain** — builds on distribution layer

All three use TDD: tests written and failing first, then implementation to make them pass.
