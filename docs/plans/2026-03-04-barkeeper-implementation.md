# barkeeper Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build `barkeeper`, an etcd-compatible distributed KV store implemented as Rebar actors with Raft consensus and redb storage.

**Architecture:** Actor-Per-Responsibility — RaftNode, LogStore, StateMachine, WatchHub, LeaseManager, ClusterManager, GrpcFrontend. Raft RPCs flow over Rebar's DistributedRouter. SWIM provides node discovery. redb provides durable storage (WAL + KV MVCC).

**Tech Stack:** Rust, rebar (actor runtime), redb (storage), tonic (gRPC), prost (protobuf), axum (HTTP gateway), clap (CLI).

---

## Phase 1: Foundation

### Task 1: Create repository and project scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `build.rs`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `LICENSE`
- Create: `README.md`
- Create: `.gitignore`
- Create: `proto/etcdserverpb/rpc.proto` (vendored)
- Create: `proto/etcdserverpb/kv.proto` (vendored)
- Create: `proto/authpb/auth.proto` (vendored)
- Create: `proto/LICENSE` (Apache 2.0 attribution)

**Step 1: Initialize git repo**

```bash
mkdir -p /home/alexandernicholson/.pxycrab/workspace/barkeeper
cd /home/alexandernicholson/.pxycrab/workspace/barkeeper
git init
```

**Step 2: Create Cargo.toml**

```toml
[package]
name = "barkeeper"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
description = "An etcd-compatible distributed KV store built on the Rebar actor runtime"

[dependencies]
# Actor runtime
rebar = { git = "https://github.com/alexandernicholson/rebar.git", branch = "main" }
rebar-core = { git = "https://github.com/alexandernicholson/rebar.git", branch = "main" }
rebar-cluster = { git = "https://github.com/alexandernicholson/rebar.git", branch = "main" }

# Storage
redb = "2"

# gRPC
tonic = "0.12"
prost = "0.13"
prost-types = "0.13"

# HTTP gateway
axum = "0.7"
tower = "0.4"
tower-http = { version = "0.5", features = ["cors"] }

# Async runtime
tokio = { version = "1", features = ["full"] }

# Serialization
rmpv = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Utilities
rand = "0.8"
uuid = { version = "1", features = ["v4"] }
bytes = "1"

[build-dependencies]
tonic-build = "0.12"
```

**Step 3: Create build.rs for protobuf codegen**

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[
                "proto/etcdserverpb/rpc.proto",
                "proto/etcdserverpb/kv.proto",
                "proto/authpb/auth.proto",
            ],
            &["proto/"],
        )?;
    Ok(())
}
```

**Step 4: Vendor etcd protobuf files**

Download from https://github.com/etcd-io/etcd/tree/main/api/:
- `etcdserverpb/rpc.proto` — all service definitions (KV, Watch, Lease, Cluster, Maintenance, Auth)
- `mvccpb/kv.proto` → save as `etcdserverpb/kv.proto` — KeyValue, Event types
- `authpb/auth.proto` — User, Permission, Role types

Adjust import paths so they compile with tonic-build. The proto files should use `package etcdserverpb;` and `package authpb;`.

Add `proto/LICENSE` with Apache 2.0 text and attribution:
```
The protobuf definitions in this directory are vendored from
https://github.com/etcd-io/etcd and are licensed under the Apache License 2.0.

Copyright 2015 The etcd Authors
```

**Step 5: Create src/lib.rs**

```rust
pub mod raft;
pub mod kv;
pub mod watch;
pub mod lease;
pub mod cluster;
pub mod auth;
pub mod api;
pub mod config;

pub mod proto {
    pub mod etcdserverpb {
        tonic::include_proto!("etcdserverpb");
    }
    pub mod authpb {
        tonic::include_proto!("authpb");
    }
}
```

**Step 6: Create stub modules**

Create empty `mod.rs` files for: `src/raft/mod.rs`, `src/kv/mod.rs`, `src/watch/mod.rs`, `src/lease/mod.rs`, `src/cluster/mod.rs`, `src/auth/mod.rs`, `src/api/mod.rs`, `src/config.rs`.

**Step 7: Create src/main.rs**

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "barkeeper", about = "etcd-compatible distributed KV store on Rebar")]
struct Cli {
    #[arg(long, default_value = "default")]
    name: String,

    #[arg(long, default_value = "data.barkeeper")]
    data_dir: String,

    #[arg(long, default_value = "http://localhost:2379")]
    listen_client_urls: String,

    #[arg(long)]
    initial_cluster: Option<String>,

    #[arg(long, default_value = "new")]
    initial_cluster_state: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    tracing::info!(name = %cli.name, "barkeeper starting");
}
```

**Step 8: Create .gitignore, LICENSE, README.md**

Standard Rust .gitignore. Apache 2.0 LICENSE. README with project description.

**Step 9: Verify build**

Run: `cargo build`
Expected: Compiles successfully (proto codegen runs, stub modules exist)

**Step 10: Commit**

```bash
git add -A
git commit -m "feat: initial project scaffold with etcd protobuf codegen"
```

---

### Task 2: Raft message types and state definitions

**Files:**
- Create: `src/raft/messages.rs`
- Create: `src/raft/state.rs`
- Modify: `src/raft/mod.rs`

**Step 1: Create src/raft/messages.rs — all Raft RPC types**

```rust
use rmpv::Value;
use serde::{Deserialize, Serialize};

/// A single entry in the Raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub term: u64,
    pub index: u64,
    pub data: LogEntryData,
}

/// What a log entry contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogEntryData {
    /// A client KV operation to apply to the state machine.
    Command(Vec<u8>),
    /// A cluster membership change.
    ConfigChange(ConfigChange),
    /// No-op entry (used after leader election).
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChange {
    pub change_type: ConfigChangeType,
    pub node_id: u64,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigChangeType {
    AddLearner,
    PromoteVoter,
    RemoveNode,
}

// --- Raft RPCs ---

/// AppendEntries RPC (leader → followers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesRequest {
    pub term: u64,
    pub leader_id: u64,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub entries: Vec<LogEntry>,
    pub leader_commit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesResponse {
    pub term: u64,
    pub success: bool,
    /// Optimization: if rejected, the follower's last log index for faster backtracking.
    pub last_log_index: u64,
}

/// RequestVote RPC (candidate → all nodes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteRequest {
    pub term: u64,
    pub candidate_id: u64,
    pub last_log_index: u64,
    pub last_log_term: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteResponse {
    pub term: u64,
    pub vote_granted: bool,
}

/// InstallSnapshot RPC (leader → lagging follower).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallSnapshotRequest {
    pub term: u64,
    pub leader_id: u64,
    pub last_included_index: u64,
    pub last_included_term: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallSnapshotResponse {
    pub term: u64,
}

// --- Internal messages (within a node) ---

/// A client proposal to be replicated through Raft.
#[derive(Debug, Clone)]
pub struct ClientProposal {
    pub id: u64,
    pub data: Vec<u8>,
    pub response_tx: tokio::sync::oneshot::Sender<ClientProposalResult>,
}

#[derive(Debug)]
pub enum ClientProposalResult {
    Success { index: u64, revision: i64 },
    NotLeader { leader_id: Option<u64> },
    Error(String),
}

/// Encodes a Raft message into rmpv::Value for Rebar messaging.
pub fn encode_raft_message(msg: &RaftMessage) -> Value {
    let bytes = serde_json::to_vec(msg).expect("raft message serialization");
    Value::Binary(bytes)
}

/// Decodes a Raft message from rmpv::Value.
pub fn decode_raft_message(val: &Value) -> Option<RaftMessage> {
    match val {
        Value::Binary(bytes) => serde_json::from_slice(bytes).ok(),
        _ => None,
    }
}

/// Wrapper enum for all Raft RPC messages sent between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RaftMessage {
    AppendEntriesReq(AppendEntriesRequest),
    AppendEntriesResp(AppendEntriesResponse),
    RequestVoteReq(RequestVoteRequest),
    RequestVoteResp(RequestVoteResponse),
    InstallSnapshotReq(InstallSnapshotRequest),
    InstallSnapshotResp(InstallSnapshotResponse),
}
```

**Step 2: Create src/raft/state.rs — Raft node state**

```rust
use std::collections::{HashMap, HashSet};

/// The three Raft states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaftRole {
    Follower,
    Candidate,
    Leader,
}

/// Persistent state (survives restarts — stored in redb).
#[derive(Debug, Clone)]
pub struct PersistentState {
    pub current_term: u64,
    pub voted_for: Option<u64>,
}

/// Volatile state (all servers).
#[derive(Debug, Clone)]
pub struct VolatileState {
    pub commit_index: u64,
    pub last_applied: u64,
}

/// Volatile state (leader only, reinitialized after election).
#[derive(Debug, Clone)]
pub struct LeaderState {
    /// For each follower: next log index to send.
    pub next_index: HashMap<u64, u64>,
    /// For each follower: highest log index known to be replicated.
    pub match_index: HashMap<u64, u64>,
}

impl LeaderState {
    pub fn new(peers: &[u64], last_log_index: u64) -> Self {
        let mut next_index = HashMap::new();
        let mut match_index = HashMap::new();
        for &peer in peers {
            next_index.insert(peer, last_log_index + 1);
            match_index.insert(peer, 0);
        }
        LeaderState { next_index, match_index }
    }
}

/// The full Raft state for a node.
#[derive(Debug)]
pub struct RaftState {
    pub node_id: u64,
    pub role: RaftRole,
    pub persistent: PersistentState,
    pub volatile: VolatileState,
    pub leader_state: Option<LeaderState>,
    pub leader_id: Option<u64>,
    /// Set of voters and learners.
    pub voters: HashSet<u64>,
    pub learners: HashSet<u64>,
    /// Votes received in current election (candidate only).
    pub votes_received: HashSet<u64>,
}

impl RaftState {
    pub fn new(node_id: u64) -> Self {
        let mut voters = HashSet::new();
        voters.insert(node_id);
        RaftState {
            node_id,
            role: RaftRole::Follower,
            persistent: PersistentState {
                current_term: 0,
                voted_for: None,
            },
            volatile: VolatileState {
                commit_index: 0,
                last_applied: 0,
            },
            leader_state: None,
            leader_id: None,
            voters,
            learners: HashSet::new(),
            votes_received: HashSet::new(),
        }
    }

    pub fn quorum_size(&self) -> usize {
        self.voters.len() / 2 + 1
    }

    pub fn is_leader(&self) -> bool {
        self.role == RaftRole::Leader
    }

    pub fn peers(&self) -> Vec<u64> {
        self.voters.iter()
            .chain(self.learners.iter())
            .filter(|&&id| id != self.node_id)
            .copied()
            .collect()
    }
}
```

**Step 3: Update src/raft/mod.rs**

```rust
pub mod messages;
pub mod state;
```

**Step 4: Write tests**

Create `tests/raft_types_test.rs`:

```rust
use barkeeper::raft::messages::*;
use barkeeper::raft::state::*;

#[test]
fn test_raft_state_new() {
    let state = RaftState::new(1);
    assert_eq!(state.node_id, 1);
    assert_eq!(state.role, RaftRole::Follower);
    assert_eq!(state.persistent.current_term, 0);
    assert_eq!(state.persistent.voted_for, None);
    assert!(state.voters.contains(&1));
    assert_eq!(state.quorum_size(), 1);
}

#[test]
fn test_quorum_size() {
    let mut state = RaftState::new(1);
    state.voters.insert(2);
    state.voters.insert(3);
    assert_eq!(state.quorum_size(), 2); // majority of 3
    state.voters.insert(4);
    state.voters.insert(5);
    assert_eq!(state.quorum_size(), 3); // majority of 5
}

#[test]
fn test_leader_state_initialization() {
    let peers = vec![2, 3, 4];
    let leader_state = LeaderState::new(&peers, 10);
    assert_eq!(leader_state.next_index[&2], 11);
    assert_eq!(leader_state.next_index[&3], 11);
    assert_eq!(leader_state.match_index[&2], 0);
}

#[test]
fn test_raft_message_encode_decode() {
    let msg = RaftMessage::RequestVoteReq(RequestVoteRequest {
        term: 5,
        candidate_id: 2,
        last_log_index: 10,
        last_log_term: 4,
    });
    let encoded = encode_raft_message(&msg);
    let decoded = decode_raft_message(&encoded).unwrap();
    match decoded {
        RaftMessage::RequestVoteReq(req) => {
            assert_eq!(req.term, 5);
            assert_eq!(req.candidate_id, 2);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_log_entry_variants() {
    let cmd = LogEntry {
        term: 1,
        index: 1,
        data: LogEntryData::Command(b"put key value".to_vec()),
    };
    assert_eq!(cmd.term, 1);

    let noop = LogEntry {
        term: 2,
        index: 2,
        data: LogEntryData::Noop,
    };
    assert_eq!(noop.index, 2);
}

#[test]
fn test_peers_excludes_self() {
    let mut state = RaftState::new(1);
    state.voters.insert(2);
    state.voters.insert(3);
    state.learners.insert(4);
    let peers = state.peers();
    assert_eq!(peers.len(), 3);
    assert!(!peers.contains(&1));
}
```

**Step 5: Run tests**

Run: `cargo test --test raft_types_test`
Expected: All tests PASS

**Step 6: Commit**

```bash
git add src/raft/ tests/raft_types_test.rs
git commit -m "feat(raft): add message types and state definitions"
```

---

### Task 3: LogStore — redb-backed Raft log persistence

**Files:**
- Create: `src/raft/log_store.rs`
- Create: `tests/log_store_test.rs`
- Modify: `src/raft/mod.rs`

The LogStore is a standalone struct (not an actor) that wraps redb for persisting Raft log entries and hard state (term, voted_for). It will be owned by the RaftNode actor.

**Step 1: Create src/raft/log_store.rs**

```rust
use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;

use super::messages::LogEntry;
use super::state::PersistentState;

/// redb table: log_index (u64) → serialized LogEntry
const LOG_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("raft_log");

/// redb table: meta key (string) → value (bytes)
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("raft_meta");

/// Durable storage for Raft log entries and hard state.
pub struct LogStore {
    db: Database,
}

impl LogStore {
    /// Open or create a LogStore at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, redb::Error> {
        let db = Database::create(path)?;
        // Ensure tables exist
        let txn = db.begin_write()?;
        {
            txn.open_table(LOG_TABLE)?;
            txn.open_table(META_TABLE)?;
        }
        txn.commit()?;
        Ok(LogStore { db })
    }

    /// Append entries to the log. Overwrites any existing entries at the same indices.
    pub fn append(&self, entries: &[LogEntry]) -> Result<(), redb::Error> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOG_TABLE)?;
            for entry in entries {
                let bytes = serde_json::to_vec(entry).expect("serialize log entry");
                table.insert(entry.index, bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Get a single log entry by index.
    pub fn get(&self, index: u64) -> Result<Option<LogEntry>, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LOG_TABLE)?;
        match table.get(index)? {
            Some(val) => {
                let entry: LogEntry = serde_json::from_slice(val.value()).expect("deserialize");
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    /// Get entries in range [start, end] inclusive.
    pub fn get_range(&self, start: u64, end: u64) -> Result<Vec<LogEntry>, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LOG_TABLE)?;
        let mut entries = Vec::new();
        for result in table.range(start..=end)? {
            let (_, val) = result?;
            let entry: LogEntry = serde_json::from_slice(val.value()).expect("deserialize");
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Truncate all entries after the given index (exclusive).
    /// Keeps entries at index and before.
    pub fn truncate_after(&self, after_index: u64) -> Result<(), redb::Error> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOG_TABLE)?;
            // Collect keys to remove
            let keys: Vec<u64> = table.range((after_index + 1)..)?
                .map(|r| r.map(|(k, _)| k.value()))
                .collect::<Result<_, _>>()?;
            for key in keys {
                table.remove(key)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Get the last log index (0 if empty).
    pub fn last_index(&self) -> Result<u64, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LOG_TABLE)?;
        match table.last()? {
            Some((k, _)) => Ok(k.value()),
            None => Ok(0),
        }
    }

    /// Get the term of the last log entry (0 if empty).
    pub fn last_term(&self) -> Result<u64, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LOG_TABLE)?;
        match table.last()? {
            Some((_, v)) => {
                let entry: LogEntry = serde_json::from_slice(v.value()).expect("deserialize");
                Ok(entry.term)
            }
            None => Ok(0),
        }
    }

    /// Get the term for a specific log index.
    pub fn term_at(&self, index: u64) -> Result<Option<u64>, redb::Error> {
        self.get(index).map(|opt| opt.map(|e| e.term))
    }

    /// Save persistent Raft state (current_term, voted_for).
    pub fn save_hard_state(&self, state: &PersistentState) -> Result<(), redb::Error> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(META_TABLE)?;
            let bytes = serde_json::to_vec(state).expect("serialize hard state");
            table.insert("hard_state", bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Load persistent Raft state.
    pub fn load_hard_state(&self) -> Result<Option<PersistentState>, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META_TABLE)?;
        match table.get("hard_state")? {
            Some(val) => {
                let state: PersistentState = serde_json::from_slice(val.value()).expect("deserialize");
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    /// Get total number of log entries.
    pub fn len(&self) -> Result<u64, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LOG_TABLE)?;
        Ok(table.len()?)
    }

    pub fn is_empty(&self) -> Result<bool, redb::Error> {
        Ok(self.len()? == 0)
    }
}
```

**Step 2: Add `Serialize, Deserialize` to PersistentState in state.rs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentState {
    pub current_term: u64,
    pub voted_for: Option<u64>,
}
```

**Step 3: Write tests in tests/log_store_test.rs**

```rust
use barkeeper::raft::log_store::LogStore;
use barkeeper::raft::messages::{LogEntry, LogEntryData};
use barkeeper::raft::state::PersistentState;
use tempfile::tempdir;

fn make_entry(term: u64, index: u64, data: &str) -> LogEntry {
    LogEntry {
        term,
        index,
        data: LogEntryData::Command(data.as_bytes().to_vec()),
    }
}

#[test]
fn test_open_and_empty() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();
    assert!(store.is_empty().unwrap());
    assert_eq!(store.last_index().unwrap(), 0);
    assert_eq!(store.last_term().unwrap(), 0);
}

#[test]
fn test_append_and_get() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();

    let entries = vec![
        make_entry(1, 1, "put a 1"),
        make_entry(1, 2, "put b 2"),
        make_entry(2, 3, "put c 3"),
    ];
    store.append(&entries).unwrap();

    assert_eq!(store.len().unwrap(), 3);
    assert_eq!(store.last_index().unwrap(), 3);
    assert_eq!(store.last_term().unwrap(), 2);

    let entry = store.get(2).unwrap().unwrap();
    assert_eq!(entry.term, 1);
    assert_eq!(entry.index, 2);
}

#[test]
fn test_get_range() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();

    let entries = vec![
        make_entry(1, 1, "a"),
        make_entry(1, 2, "b"),
        make_entry(2, 3, "c"),
        make_entry(2, 4, "d"),
    ];
    store.append(&entries).unwrap();

    let range = store.get_range(2, 3).unwrap();
    assert_eq!(range.len(), 2);
    assert_eq!(range[0].index, 2);
    assert_eq!(range[1].index, 3);
}

#[test]
fn test_truncate_after() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();

    let entries = vec![
        make_entry(1, 1, "a"),
        make_entry(1, 2, "b"),
        make_entry(2, 3, "c"),
        make_entry(2, 4, "d"),
    ];
    store.append(&entries).unwrap();

    store.truncate_after(2).unwrap();
    assert_eq!(store.len().unwrap(), 2);
    assert_eq!(store.last_index().unwrap(), 2);
    assert!(store.get(3).unwrap().is_none());
}

#[test]
fn test_term_at() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();

    store.append(&[make_entry(5, 1, "x")]).unwrap();
    assert_eq!(store.term_at(1).unwrap(), Some(5));
    assert_eq!(store.term_at(99).unwrap(), None);
}

#[test]
fn test_hard_state_persistence() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.redb");

    {
        let store = LogStore::open(&path).unwrap();
        assert!(store.load_hard_state().unwrap().is_none());

        let state = PersistentState { current_term: 5, voted_for: Some(3) };
        store.save_hard_state(&state).unwrap();
    }

    // Reopen and verify persistence
    {
        let store = LogStore::open(&path).unwrap();
        let state = store.load_hard_state().unwrap().unwrap();
        assert_eq!(state.current_term, 5);
        assert_eq!(state.voted_for, Some(3));
    }
}

#[test]
fn test_overwrite_entries() {
    let dir = tempdir().unwrap();
    let store = LogStore::open(dir.path().join("test.redb")).unwrap();

    store.append(&[make_entry(1, 1, "original")]).unwrap();
    store.append(&[make_entry(2, 1, "overwritten")]).unwrap();

    let entry = store.get(1).unwrap().unwrap();
    assert_eq!(entry.term, 2);
}
```

Add `tempfile = "3"` to `[dev-dependencies]` in Cargo.toml.

**Step 4: Run tests**

Run: `cargo test --test log_store_test`
Expected: All tests PASS

**Step 5: Commit**

```bash
git add src/raft/log_store.rs src/raft/mod.rs tests/log_store_test.rs Cargo.toml
git commit -m "feat(raft): add redb-backed LogStore with persistence tests"
```

---

## Phase 2: Raft Consensus

### Task 4: Raft core logic — election and vote handling

**Files:**
- Create: `src/raft/core.rs`
- Create: `tests/raft_election_test.rs`
- Modify: `src/raft/mod.rs`

Implement the Raft core as a pure state machine struct `RaftCore` that processes inputs and produces outputs (actions). No I/O, no actors — just logic. This makes it highly testable.

**Step 1: Create src/raft/core.rs**

The `RaftCore` struct processes events and returns `Vec<Action>` describing what should happen (send messages, persist state, apply entries, etc.).

```rust
use std::collections::HashSet;
use std::time::Duration;

use super::log_store::LogStore;
use super::messages::*;
use super::state::*;

/// Actions the RaftCore wants the actor to perform.
#[derive(Debug)]
pub enum Action {
    /// Send a Raft message to a peer node.
    SendMessage { to: u64, message: RaftMessage },
    /// Persist hard state to durable storage.
    PersistHardState(PersistentState),
    /// Append entries to log store.
    AppendToLog(Vec<LogEntry>),
    /// Truncate log after given index.
    TruncateLogAfter(u64),
    /// Apply committed entries [last_applied+1..=commit_index] to state machine.
    ApplyEntries { from: u64, to: u64 },
    /// Reset election timer (randomized).
    ResetElectionTimer,
    /// Start sending heartbeats (became leader).
    StartHeartbeatTimer,
    /// Stop heartbeat timer (stepped down).
    StopHeartbeatTimer,
    /// Respond to a client proposal.
    RespondToProposal { id: u64, result: ClientProposalResult },
}

/// Events that drive the Raft state machine.
#[derive(Debug)]
pub enum Event {
    /// Raft message received from a peer.
    Message { from: u64, message: RaftMessage },
    /// Election timeout fired.
    ElectionTimeout,
    /// Heartbeat timer fired (leader only).
    HeartbeatTimeout,
    /// Client submitted a proposal.
    Proposal { id: u64, data: Vec<u8> },
    /// Node is starting up with restored state.
    Initialize {
        peers: Vec<u64>,
        hard_state: Option<PersistentState>,
        last_log_index: u64,
        last_log_term: u64,
    },
}

pub struct RaftCore {
    pub state: RaftState,
    last_log_index: u64,
    last_log_term: u64,
    /// Pending client proposals awaiting commit (leader only).
    pending_proposals: Vec<(u64, u64)>, // (proposal_id, log_index)
}

impl RaftCore {
    pub fn new(node_id: u64) -> Self {
        RaftCore {
            state: RaftState::new(node_id),
            last_log_index: 0,
            last_log_term: 0,
            pending_proposals: Vec::new(),
        }
    }

    /// Process an event and return actions to perform.
    pub fn step(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Initialize { peers, hard_state, last_log_index, last_log_term } => {
                self.handle_initialize(peers, hard_state, last_log_index, last_log_term)
            }
            Event::ElectionTimeout => self.handle_election_timeout(),
            Event::HeartbeatTimeout => self.handle_heartbeat_timeout(),
            Event::Proposal { id, data } => self.handle_proposal(id, data),
            Event::Message { from, message } => self.handle_message(from, message),
        }
    }

    fn handle_initialize(
        &mut self,
        peers: Vec<u64>,
        hard_state: Option<PersistentState>,
        last_log_index: u64,
        last_log_term: u64,
    ) -> Vec<Action> {
        if let Some(hs) = hard_state {
            self.state.persistent = hs;
        }
        for peer in peers {
            self.state.voters.insert(peer);
        }
        self.last_log_index = last_log_index;
        self.last_log_term = last_log_term;
        vec![Action::ResetElectionTimer]
    }

    fn handle_election_timeout(&mut self) -> Vec<Action> {
        if self.state.is_leader() {
            return vec![];
        }
        self.start_election()
    }

    fn start_election(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();

        self.state.persistent.current_term += 1;
        self.state.role = RaftRole::Candidate;
        self.state.persistent.voted_for = Some(self.state.node_id);
        self.state.votes_received.clear();
        self.state.votes_received.insert(self.state.node_id);
        self.state.leader_id = None;

        actions.push(Action::PersistHardState(self.state.persistent.clone()));
        actions.push(Action::ResetElectionTimer);

        // Single-node cluster: immediately become leader
        if self.state.quorum_size() <= 1 {
            return self.become_leader(actions);
        }

        let req = RequestVoteRequest {
            term: self.state.persistent.current_term,
            candidate_id: self.state.node_id,
            last_log_index: self.last_log_index,
            last_log_term: self.last_log_term,
        };

        for &peer in &self.state.voters.clone() {
            if peer != self.state.node_id {
                actions.push(Action::SendMessage {
                    to: peer,
                    message: RaftMessage::RequestVoteReq(req.clone()),
                });
            }
        }

        actions
    }

    fn become_leader(&mut self, mut actions: Vec<Action>) -> Vec<Action> {
        self.state.role = RaftRole::Leader;
        self.state.leader_id = Some(self.state.node_id);

        let peers: Vec<u64> = self.state.peers();
        self.state.leader_state = Some(LeaderState::new(&peers, self.last_log_index));

        actions.push(Action::StartHeartbeatTimer);

        // Append no-op entry to commit entries from previous terms
        let noop = LogEntry {
            term: self.state.persistent.current_term,
            index: self.last_log_index + 1,
            data: LogEntryData::Noop,
        };
        self.last_log_index = noop.index;
        self.last_log_term = noop.term;
        actions.push(Action::AppendToLog(vec![noop]));

        // Send initial empty AppendEntries (heartbeat) to all peers
        actions.extend(self.send_append_entries_to_all());

        actions
    }

    fn handle_heartbeat_timeout(&mut self) -> Vec<Action> {
        if !self.state.is_leader() {
            return vec![];
        }
        self.send_append_entries_to_all()
    }

    fn handle_proposal(&mut self, id: u64, data: Vec<u8>) -> Vec<Action> {
        if !self.state.is_leader() {
            return vec![Action::RespondToProposal {
                id,
                result: ClientProposalResult::NotLeader {
                    leader_id: self.state.leader_id,
                },
            }];
        }

        let entry = LogEntry {
            term: self.state.persistent.current_term,
            index: self.last_log_index + 1,
            data: LogEntryData::Command(data),
        };
        self.last_log_index = entry.index;
        self.last_log_term = entry.term;
        self.pending_proposals.push((id, entry.index));

        let mut actions = vec![Action::AppendToLog(vec![entry])];

        // Update own match_index
        if let Some(ref mut ls) = self.state.leader_state {
            ls.match_index.insert(self.state.node_id, self.last_log_index);
            ls.next_index.insert(self.state.node_id, self.last_log_index + 1);
        }

        // Replicate to followers
        actions.extend(self.send_append_entries_to_all());

        // Check if we can commit (single-node cluster)
        actions.extend(self.advance_commit_index());

        actions
    }

    fn handle_message(&mut self, from: u64, message: RaftMessage) -> Vec<Action> {
        match message {
            RaftMessage::RequestVoteReq(req) => self.handle_request_vote(from, req),
            RaftMessage::RequestVoteResp(resp) => self.handle_request_vote_response(from, resp),
            RaftMessage::AppendEntriesReq(req) => self.handle_append_entries(from, req),
            RaftMessage::AppendEntriesResp(resp) => self.handle_append_entries_response(from, resp),
            RaftMessage::InstallSnapshotReq(req) => self.handle_install_snapshot(from, req),
            RaftMessage::InstallSnapshotResp(resp) => vec![], // handled in Task 16
        }
    }

    fn handle_request_vote(&mut self, from: u64, req: RequestVoteRequest) -> Vec<Action> {
        let mut actions = Vec::new();

        // If request term > our term, step down
        if req.term > self.state.persistent.current_term {
            self.step_down(req.term, &mut actions);
        }

        let vote_granted = req.term == self.state.persistent.current_term
            && (self.state.persistent.voted_for.is_none()
                || self.state.persistent.voted_for == Some(req.candidate_id))
            && self.is_log_up_to_date(req.last_log_index, req.last_log_term);

        if vote_granted {
            self.state.persistent.voted_for = Some(req.candidate_id);
            actions.push(Action::PersistHardState(self.state.persistent.clone()));
            actions.push(Action::ResetElectionTimer);
        }

        actions.push(Action::SendMessage {
            to: from,
            message: RaftMessage::RequestVoteResp(RequestVoteResponse {
                term: self.state.persistent.current_term,
                vote_granted,
            }),
        });

        actions
    }

    fn handle_request_vote_response(&mut self, from: u64, resp: RequestVoteResponse) -> Vec<Action> {
        let mut actions = Vec::new();

        if resp.term > self.state.persistent.current_term {
            self.step_down(resp.term, &mut actions);
            return actions;
        }

        if self.state.role != RaftRole::Candidate || resp.term != self.state.persistent.current_term {
            return actions;
        }

        if resp.vote_granted {
            self.state.votes_received.insert(from);
            if self.state.votes_received.len() >= self.state.quorum_size() {
                return self.become_leader(actions);
            }
        }

        actions
    }

    fn handle_append_entries(&mut self, from: u64, req: AppendEntriesRequest) -> Vec<Action> {
        let mut actions = Vec::new();

        if req.term < self.state.persistent.current_term {
            actions.push(Action::SendMessage {
                to: from,
                message: RaftMessage::AppendEntriesResp(AppendEntriesResponse {
                    term: self.state.persistent.current_term,
                    success: false,
                    last_log_index: self.last_log_index,
                }),
            });
            return actions;
        }

        if req.term > self.state.persistent.current_term {
            self.step_down(req.term, &mut actions);
        } else if self.state.role == RaftRole::Candidate {
            self.state.role = RaftRole::Follower;
            actions.push(Action::StopHeartbeatTimer);
        }

        self.state.leader_id = Some(from);
        actions.push(Action::ResetElectionTimer);

        // Log consistency check: we need the entry at prev_log_index to match prev_log_term
        // This is checked by the actor against the LogStore, so we trust the caller here.
        // For now, we accept and let the actor handle the consistency check.
        // The actor will call step() with the appropriate response.

        if !req.entries.is_empty() {
            // Truncate conflicting entries and append new ones
            let first_new_index = req.entries[0].index;
            actions.push(Action::TruncateLogAfter(first_new_index - 1));
            self.last_log_index = req.entries.last().unwrap().index;
            self.last_log_term = req.entries.last().unwrap().term;
            actions.push(Action::AppendToLog(req.entries));
        }

        // Update commit index
        if req.leader_commit > self.state.volatile.commit_index {
            let new_commit = std::cmp::min(req.leader_commit, self.last_log_index);
            if new_commit > self.state.volatile.commit_index {
                let old_commit = self.state.volatile.commit_index;
                self.state.volatile.commit_index = new_commit;
                actions.push(Action::ApplyEntries {
                    from: old_commit + 1,
                    to: new_commit,
                });
            }
        }

        actions.push(Action::SendMessage {
            to: from,
            message: RaftMessage::AppendEntriesResp(AppendEntriesResponse {
                term: self.state.persistent.current_term,
                success: true,
                last_log_index: self.last_log_index,
            }),
        });

        actions
    }

    fn handle_append_entries_response(&mut self, from: u64, resp: AppendEntriesResponse) -> Vec<Action> {
        let mut actions = Vec::new();

        if resp.term > self.state.persistent.current_term {
            self.step_down(resp.term, &mut actions);
            return actions;
        }

        if !self.state.is_leader() || resp.term != self.state.persistent.current_term {
            return actions;
        }

        let leader_state = self.state.leader_state.as_mut().unwrap();

        if resp.success {
            leader_state.match_index.insert(from, resp.last_log_index);
            leader_state.next_index.insert(from, resp.last_log_index + 1);
            actions.extend(self.advance_commit_index());
        } else {
            // Decrement next_index and retry (log inconsistency)
            let next = leader_state.next_index.get(&from).copied().unwrap_or(1);
            let new_next = std::cmp::min(next.saturating_sub(1).max(1), resp.last_log_index + 1);
            leader_state.next_index.insert(from, new_next);
            // The actor will send AppendEntries on the next heartbeat or immediately
        }

        actions
    }

    fn handle_install_snapshot(&mut self, from: u64, req: InstallSnapshotRequest) -> Vec<Action> {
        // Placeholder — implemented in Task 16
        vec![]
    }

    // --- Helper methods ---

    fn step_down(&mut self, new_term: u64, actions: &mut Vec<Action>) {
        self.state.persistent.current_term = new_term;
        self.state.persistent.voted_for = None;
        self.state.role = RaftRole::Follower;
        self.state.leader_state = None;
        self.state.votes_received.clear();
        actions.push(Action::PersistHardState(self.state.persistent.clone()));
        actions.push(Action::StopHeartbeatTimer);
    }

    fn is_log_up_to_date(&self, last_log_index: u64, last_log_term: u64) -> bool {
        if last_log_term != self.last_log_term {
            last_log_term > self.last_log_term
        } else {
            last_log_index >= self.last_log_index
        }
    }

    fn send_append_entries_to_all(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        let leader_state = match &self.state.leader_state {
            Some(ls) => ls,
            None => return actions,
        };

        for &peer in &self.state.voters.clone() {
            if peer == self.state.node_id {
                continue;
            }
            let next = leader_state.next_index.get(&peer).copied().unwrap_or(1);
            let prev_index = next - 1;

            // The actor will fill in entries from LogStore based on next_index.
            // We send the request structure; the actor reads entries from storage.
            let req = AppendEntriesRequest {
                term: self.state.persistent.current_term,
                leader_id: self.state.node_id,
                prev_log_index: prev_index,
                prev_log_term: 0, // Actor fills this from LogStore
                entries: vec![],  // Actor fills this from LogStore
                leader_commit: self.state.volatile.commit_index,
            };
            actions.push(Action::SendMessage {
                to: peer,
                message: RaftMessage::AppendEntriesReq(req),
            });
        }

        actions
    }

    fn advance_commit_index(&mut self) -> Vec<Action> {
        if !self.state.is_leader() {
            return vec![];
        }

        let leader_state = self.state.leader_state.as_ref().unwrap();

        // Find the highest index replicated to a majority
        let mut match_indices: Vec<u64> = leader_state.match_index.values().copied().collect();
        match_indices.sort_unstable();
        let quorum_idx = match_indices.len() - self.state.quorum_size();
        let new_commit = match_indices[quorum_idx];

        if new_commit > self.state.volatile.commit_index {
            let old_commit = self.state.volatile.commit_index;
            self.state.volatile.commit_index = new_commit;

            let mut actions = vec![Action::ApplyEntries {
                from: old_commit + 1,
                to: new_commit,
            }];

            // Respond to any pending proposals that are now committed
            let committed: Vec<(u64, u64)> = self.pending_proposals
                .iter()
                .filter(|(_, idx)| *idx <= new_commit)
                .cloned()
                .collect();
            self.pending_proposals.retain(|(_, idx)| *idx > new_commit);
            for (id, _) in committed {
                actions.push(Action::RespondToProposal {
                    id,
                    result: ClientProposalResult::Success { index: new_commit, revision: 0 },
                });
            }

            actions
        } else {
            vec![]
        }
    }
}
```

**Step 2: Write tests in tests/raft_election_test.rs**

```rust
use barkeeper::raft::core::*;
use barkeeper::raft::messages::*;
use barkeeper::raft::state::*;

fn init_core(node_id: u64, peers: Vec<u64>) -> RaftCore {
    let mut core = RaftCore::new(node_id);
    core.step(Event::Initialize {
        peers,
        hard_state: None,
        last_log_index: 0,
        last_log_term: 0,
    });
    core
}

#[test]
fn test_single_node_election() {
    let mut core = init_core(1, vec![]);
    assert_eq!(core.state.role, RaftRole::Follower);

    let actions = core.step(Event::ElectionTimeout);
    assert_eq!(core.state.role, RaftRole::Leader);
    assert_eq!(core.state.persistent.current_term, 1);
    assert!(actions.iter().any(|a| matches!(a, Action::StartHeartbeatTimer)));
}

#[test]
fn test_three_node_election_starts() {
    let mut core = init_core(1, vec![2, 3]);
    let actions = core.step(Event::ElectionTimeout);
    assert_eq!(core.state.role, RaftRole::Candidate);
    assert_eq!(core.state.persistent.current_term, 1);
    assert_eq!(core.state.persistent.voted_for, Some(1));

    // Should send RequestVote to peers 2 and 3
    let vote_requests: Vec<_> = actions.iter().filter(|a| matches!(a, Action::SendMessage { .. })).collect();
    assert_eq!(vote_requests.len(), 2);
}

#[test]
fn test_wins_election_with_majority() {
    let mut core = init_core(1, vec![2, 3]);
    core.step(Event::ElectionTimeout);

    // Receive vote from node 2 (now have 2/3 = majority)
    let actions = core.step(Event::Message {
        from: 2,
        message: RaftMessage::RequestVoteResp(RequestVoteResponse {
            term: 1,
            vote_granted: true,
        }),
    });

    assert_eq!(core.state.role, RaftRole::Leader);
    assert!(actions.iter().any(|a| matches!(a, Action::StartHeartbeatTimer)));
}

#[test]
fn test_vote_rejected_higher_term() {
    let mut core = init_core(1, vec![2, 3]);
    core.step(Event::ElectionTimeout);

    let actions = core.step(Event::Message {
        from: 2,
        message: RaftMessage::RequestVoteResp(RequestVoteResponse {
            term: 5, // higher term
            vote_granted: false,
        }),
    });

    // Should step down to follower at term 5
    assert_eq!(core.state.role, RaftRole::Follower);
    assert_eq!(core.state.persistent.current_term, 5);
}

#[test]
fn test_grant_vote_if_not_voted() {
    let mut core = init_core(1, vec![2, 3]);

    let actions = core.step(Event::Message {
        from: 2,
        message: RaftMessage::RequestVoteReq(RequestVoteRequest {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        }),
    });

    // Should grant vote
    let resp = actions.iter().find_map(|a| match a {
        Action::SendMessage { to: 2, message: RaftMessage::RequestVoteResp(resp) } => Some(resp),
        _ => None,
    });
    assert!(resp.unwrap().vote_granted);
    assert_eq!(core.state.persistent.voted_for, Some(2));
}

#[test]
fn test_deny_vote_if_already_voted() {
    let mut core = init_core(1, vec![2, 3]);

    // Vote for node 2
    core.step(Event::Message {
        from: 2,
        message: RaftMessage::RequestVoteReq(RequestVoteRequest {
            term: 1,
            candidate_id: 2,
            last_log_index: 0,
            last_log_term: 0,
        }),
    });

    // Node 3 also requests vote
    let actions = core.step(Event::Message {
        from: 3,
        message: RaftMessage::RequestVoteReq(RequestVoteRequest {
            term: 1,
            candidate_id: 3,
            last_log_index: 0,
            last_log_term: 0,
        }),
    });

    let resp = actions.iter().find_map(|a| match a {
        Action::SendMessage { to: 3, message: RaftMessage::RequestVoteResp(resp) } => Some(resp),
        _ => None,
    });
    assert!(!resp.unwrap().vote_granted);
}

#[test]
fn test_leader_steps_down_on_higher_term() {
    let mut core = init_core(1, vec![]);
    core.step(Event::ElectionTimeout); // become leader
    assert!(core.state.is_leader());

    // Receive AppendEntries from a node with higher term
    core.step(Event::Message {
        from: 2,
        message: RaftMessage::AppendEntriesReq(AppendEntriesRequest {
            term: 5,
            leader_id: 2,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![],
            leader_commit: 0,
        }),
    });

    assert_eq!(core.state.role, RaftRole::Follower);
    assert_eq!(core.state.persistent.current_term, 5);
    assert_eq!(core.state.leader_id, Some(2));
}

#[test]
fn test_proposal_not_leader() {
    let mut core = init_core(1, vec![2, 3]);

    let actions = core.step(Event::Proposal {
        id: 1,
        data: b"test".to_vec(),
    });

    assert!(actions.iter().any(|a| matches!(
        a, Action::RespondToProposal { result: ClientProposalResult::NotLeader { .. }, .. }
    )));
}

#[test]
fn test_proposal_as_leader_single_node() {
    let mut core = init_core(1, vec![]);
    core.step(Event::ElectionTimeout);

    let actions = core.step(Event::Proposal {
        id: 42,
        data: b"put x 1".to_vec(),
    });

    // Should append to log and commit immediately (single-node)
    assert!(actions.iter().any(|a| matches!(a, Action::AppendToLog(_))));
    assert!(actions.iter().any(|a| matches!(a, Action::ApplyEntries { .. })));
    assert!(actions.iter().any(|a| matches!(
        a, Action::RespondToProposal { id: 42, result: ClientProposalResult::Success { .. } }
    )));
}
```

**Step 3: Run tests**

Run: `cargo test --test raft_election_test`
Expected: All tests PASS

**Step 4: Commit**

```bash
git add src/raft/core.rs src/raft/mod.rs tests/raft_election_test.rs
git commit -m "feat(raft): add core state machine with election and replication logic"
```

---

### Task 5: RaftNode actor — wiring core logic to Rebar

**Files:**
- Create: `src/raft/node.rs`
- Modify: `src/raft/mod.rs`

The RaftNode actor owns a `RaftCore` and a `LogStore`, receives Rebar messages, drives the core, and executes actions.

**Step 1: Create src/raft/node.rs**

```rust
use std::sync::Arc;
use std::time::Duration;

use rebar_core::Runtime;
use rmpv::Value;
use tokio::sync::{mpsc, oneshot};
use rand::Rng;

use super::core::{Action, Event, RaftCore};
use super::log_store::LogStore;
use super::messages::*;

/// Configuration for the RaftNode.
#[derive(Debug, Clone)]
pub struct RaftConfig {
    pub node_id: u64,
    pub data_dir: String,
    pub election_timeout_min: Duration,
    pub election_timeout_max: Duration,
    pub heartbeat_interval: Duration,
    pub peers: Vec<u64>,
}

impl Default for RaftConfig {
    fn default() -> Self {
        RaftConfig {
            node_id: 1,
            data_dir: "data.barkeeper".into(),
            election_timeout_min: Duration::from_millis(150),
            election_timeout_max: Duration::from_millis(300),
            heartbeat_interval: Duration::from_millis(50),
            peers: vec![],
        }
    }
}

/// Handle to communicate with the RaftNode actor.
#[derive(Clone)]
pub struct RaftHandle {
    pub proposal_tx: mpsc::Sender<ClientProposal>,
}

impl RaftHandle {
    /// Submit a proposal and wait for the result.
    pub async fn propose(&self, data: Vec<u8>) -> Result<ClientProposalResult, String> {
        let (tx, rx) = oneshot::channel();
        let proposal = ClientProposal {
            id: rand::random(),
            data,
            response_tx: tx,
        };
        self.proposal_tx.send(proposal).await.map_err(|_| "raft node stopped".to_string())?;
        rx.await.map_err(|_| "proposal dropped".to_string())
    }
}

/// Spawn the RaftNode actor. Returns a handle for submitting proposals.
///
/// The RaftNode runs as a Rebar process that:
/// 1. Loads persistent state from LogStore
/// 2. Runs election/heartbeat timers
/// 3. Processes inbound Raft messages
/// 4. Executes actions from RaftCore
pub async fn spawn_raft_node(
    runtime: Arc<Runtime>,
    config: RaftConfig,
    apply_tx: mpsc::Sender<Vec<LogEntry>>,
) -> RaftHandle {
    let (proposal_tx, proposal_rx) = mpsc::channel::<ClientProposal>(256);
    let handle = RaftHandle { proposal_tx };

    let log_store = LogStore::open(format!("{}/raft.redb", config.data_dir))
        .expect("failed to open LogStore");

    let mut core = RaftCore::new(config.node_id);

    // Initialize from persistent state
    let hard_state = log_store.load_hard_state().unwrap();
    let last_index = log_store.last_index().unwrap();
    let last_term = log_store.last_term().unwrap();

    let init_actions = core.step(Event::Initialize {
        peers: config.peers.clone(),
        hard_state,
        last_log_index: last_index,
        last_log_term: last_term,
    });

    // Spawn the actor as a tokio task (will be migrated to Rebar process later)
    tokio::spawn(async move {
        let mut proposal_rx = proposal_rx;
        let mut election_timer = random_election_timeout(&config);
        let mut heartbeat_timer: Option<tokio::time::Interval> = None;
        let mut pending_responses: std::collections::HashMap<u64, oneshot::Sender<ClientProposalResult>> = Default::default();

        // Process init actions
        // (simplified — full action executor below)

        loop {
            tokio::select! {
                // Election timeout
                _ = &mut election_timer => {
                    let actions = core.step(Event::ElectionTimeout);
                    execute_actions(&actions, &log_store, &apply_tx, &mut pending_responses, &mut heartbeat_timer, &config).await;
                    election_timer = random_election_timeout(&config);
                }

                // Heartbeat (leader only)
                _ = async {
                    if let Some(ref mut hb) = heartbeat_timer {
                        hb.tick().await
                    } else {
                        std::future::pending::<tokio::time::Instant>().await
                    }
                } => {
                    let actions = core.step(Event::HeartbeatTimeout);
                    execute_actions(&actions, &log_store, &apply_tx, &mut pending_responses, &mut heartbeat_timer, &config).await;
                }

                // Client proposals
                Some(proposal) = proposal_rx.recv() => {
                    let id = proposal.id;
                    pending_responses.insert(id, proposal.response_tx);
                    let actions = core.step(Event::Proposal { id, data: proposal.data });
                    execute_actions(&actions, &log_store, &apply_tx, &mut pending_responses, &mut heartbeat_timer, &config).await;
                }
            }
        }
    });

    handle
}

fn random_election_timeout(config: &RaftConfig) -> tokio::time::Sleep {
    let mut rng = rand::thread_rng();
    let ms = rng.gen_range(
        config.election_timeout_min.as_millis()..=config.election_timeout_max.as_millis()
    );
    tokio::time::sleep(Duration::from_millis(ms as u64))
}

async fn execute_actions(
    actions: &[Action],
    log_store: &LogStore,
    apply_tx: &mpsc::Sender<Vec<LogEntry>>,
    pending_responses: &mut std::collections::HashMap<u64, oneshot::Sender<ClientProposalResult>>,
    heartbeat_timer: &mut Option<tokio::time::Interval>,
    config: &RaftConfig,
) {
    for action in actions {
        match action {
            Action::PersistHardState(state) => {
                log_store.save_hard_state(state).unwrap();
            }
            Action::AppendToLog(entries) => {
                log_store.append(entries).unwrap();
            }
            Action::TruncateLogAfter(index) => {
                log_store.truncate_after(*index).unwrap();
            }
            Action::ApplyEntries { from, to } => {
                let entries = log_store.get_range(*from, *to).unwrap();
                apply_tx.send(entries).await.ok();
            }
            Action::RespondToProposal { id, result } => {
                if let Some(tx) = pending_responses.remove(id) {
                    // We need to clone the result or restructure — for now, create a new one
                    let _ = tx.send(match result {
                        ClientProposalResult::Success { index, revision } => {
                            ClientProposalResult::Success { index: *index, revision: *revision }
                        }
                        ClientProposalResult::NotLeader { leader_id } => {
                            ClientProposalResult::NotLeader { leader_id: *leader_id }
                        }
                        ClientProposalResult::Error(e) => {
                            ClientProposalResult::Error(e.clone())
                        }
                    });
                }
            }
            Action::ResetElectionTimer => {
                // Timer reset handled by the select loop
            }
            Action::StartHeartbeatTimer => {
                *heartbeat_timer = Some(tokio::time::interval(config.heartbeat_interval));
            }
            Action::StopHeartbeatTimer => {
                *heartbeat_timer = None;
            }
            Action::SendMessage { to, message } => {
                // TODO: send via Rebar DistributedRouter in multi-node mode
                // For single-node, no peers to send to
                tracing::debug!(to = to, "would send raft message (no network yet)");
            }
        }
    }
}
```

**Step 2: Commit**

```bash
git add src/raft/node.rs src/raft/mod.rs
git commit -m "feat(raft): add RaftNode actor with timer-driven event loop"
```

---

## Phase 3: KV Store

### Task 6: MVCC KV store with redb

**Files:**
- Create: `src/kv/store.rs`
- Create: `tests/kv_store_test.rs`
- Modify: `src/kv/mod.rs`

The MVCC store is the core data structure — every mutation creates a new revision.

**Step 1: Create src/kv/store.rs**

```rust
use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;

use crate::proto::etcdserverpb::{KeyValue, Event as EtcdEvent, event::EventType};

/// KV table: (key, revision) → serialized KeyValue
/// We use a compound key encoded as: key_bytes + 8-byte big-endian revision
const KV_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

/// Revision index: revision (u64) → serialized list of (key, event_type) pairs
const REV_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("revisions");

/// Meta table: string key → bytes
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kv_meta");

/// MVCC key-value store backed by redb.
pub struct KvStore {
    db: Database,
}

/// Result of a Put operation.
#[derive(Debug, Clone)]
pub struct PutResult {
    pub revision: i64,
    pub prev_kv: Option<KeyValue>,
}

/// Result of a DeleteRange operation.
#[derive(Debug, Clone)]
pub struct DeleteResult {
    pub revision: i64,
    pub deleted: i64,
    pub prev_kvs: Vec<KeyValue>,
}

/// Result of a Range query.
#[derive(Debug, Clone)]
pub struct RangeResult {
    pub kvs: Vec<KeyValue>,
    pub count: i64,
    pub more: bool,
}

/// A mutation event for the watch system.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub revision: i64,
    pub events: Vec<EtcdEvent>,
}

impl KvStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, redb::Error> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        {
            txn.open_table(KV_TABLE)?;
            txn.open_table(REV_TABLE)?;
            txn.open_table(META_TABLE)?;
        }
        txn.commit()?;
        Ok(KvStore { db })
    }

    /// Get the current revision.
    pub fn current_revision(&self) -> Result<i64, redb::Error> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META_TABLE)?;
        match table.get("revision")? {
            Some(val) => {
                let rev: i64 = serde_json::from_slice(val.value()).unwrap();
                Ok(rev)
            }
            None => Ok(0),
        }
    }

    /// Put a key-value pair. Returns the new revision and optional previous value.
    pub fn put(&self, key: Vec<u8>, value: Vec<u8>, lease_id: i64) -> Result<PutResult, redb::Error> {
        let txn = self.db.begin_write()?;
        let result;
        {
            let mut kv_table = txn.open_table(KV_TABLE)?;
            let mut rev_table = txn.open_table(REV_TABLE)?;
            let mut meta_table = txn.open_table(META_TABLE)?;

            let prev_rev = self.read_revision(&meta_table)?;
            let new_rev = prev_rev + 1;

            // Find previous value for this key
            let prev_kv = self.get_latest_kv(&kv_table, &key)?;

            let (create_revision, version) = match &prev_kv {
                Some(prev) => (prev.create_revision, prev.version + 1),
                None => (new_rev, 1),
            };

            let kv = KeyValue {
                key: key.clone(),
                value: value.clone(),
                create_revision,
                mod_revision: new_rev,
                version,
                lease: lease_id,
            };

            let compound_key = make_compound_key(&key, new_rev);
            let kv_bytes = serde_json::to_vec(&kv).unwrap();
            kv_table.insert(compound_key.as_slice(), kv_bytes.as_slice())?;

            // Record in revision index
            let rev_entry = serde_json::to_vec(&vec![RevEntry { key: key.clone(), event_type: 0 }]).unwrap();
            rev_table.insert(new_rev as u64, rev_entry.as_slice())?;

            // Update revision
            let rev_bytes = serde_json::to_vec(&new_rev).unwrap();
            meta_table.insert("revision", rev_bytes.as_slice())?;

            result = PutResult { revision: new_rev, prev_kv };
        }
        txn.commit()?;
        Ok(result)
    }

    /// Range query: get key or key range.
    pub fn range(&self, key: &[u8], range_end: &[u8], limit: i64, revision: i64) -> Result<RangeResult, redb::Error> {
        let txn = self.db.begin_read()?;
        let kv_table = txn.open_table(KV_TABLE)?;

        let target_rev = if revision > 0 {
            revision
        } else {
            let meta_table = txn.open_table(META_TABLE)?;
            self.read_revision(&meta_table)?
        };

        let mut kvs = Vec::new();

        if range_end.is_empty() {
            // Single key lookup
            if let Some(kv) = self.get_kv_at_revision(&kv_table, key, target_rev)? {
                kvs.push(kv);
            }
        } else {
            // Range scan — find all unique keys in range, get latest value at target_rev
            let all_keys = self.scan_keys_in_range(&kv_table, key, range_end, target_rev)?;
            kvs = all_keys;
        }

        let count = kvs.len() as i64;
        let more = if limit > 0 && count > limit {
            kvs.truncate(limit as usize);
            true
        } else {
            false
        };

        Ok(RangeResult { kvs, count, more })
    }

    /// Delete a key or range of keys.
    pub fn delete_range(&self, key: &[u8], range_end: &[u8]) -> Result<DeleteResult, redb::Error> {
        let txn = self.db.begin_write()?;
        let result;
        {
            let mut kv_table = txn.open_table(KV_TABLE)?;
            let mut rev_table = txn.open_table(REV_TABLE)?;
            let mut meta_table = txn.open_table(META_TABLE)?;

            let prev_rev = self.read_revision(&meta_table)?;
            let new_rev = prev_rev + 1;

            // Find keys to delete
            let keys_to_delete = if range_end.is_empty() {
                match self.get_latest_kv_write(&kv_table, key)? {
                    Some(kv) => vec![kv],
                    None => vec![],
                }
            } else {
                self.scan_keys_in_range_write(&kv_table, key, range_end)?
            };

            let deleted = keys_to_delete.len() as i64;
            let prev_kvs = keys_to_delete.clone();

            // Create tombstone entries
            let mut rev_entries = Vec::new();
            for kv in &keys_to_delete {
                let tombstone = KeyValue {
                    key: kv.key.clone(),
                    value: vec![],
                    create_revision: 0,
                    mod_revision: new_rev,
                    version: 0,
                    lease: 0,
                };
                let compound_key = make_compound_key(&kv.key, new_rev);
                let kv_bytes = serde_json::to_vec(&tombstone).unwrap();
                kv_table.insert(compound_key.as_slice(), kv_bytes.as_slice())?;
                rev_entries.push(RevEntry { key: kv.key.clone(), event_type: 1 });
            }

            if deleted > 0 {
                let rev_bytes = serde_json::to_vec(&rev_entries).unwrap();
                rev_table.insert(new_rev as u64, rev_bytes.as_slice())?;
                let rev_bytes = serde_json::to_vec(&new_rev).unwrap();
                meta_table.insert("revision", rev_bytes.as_slice())?;
            }

            result = DeleteResult { revision: new_rev, deleted, prev_kvs };
        }
        txn.commit()?;
        Ok(result)
    }

    // --- Internal helpers ---

    fn read_revision<T: ReadableTable<&'static str, &'static [u8]>>(&self, table: &T) -> Result<i64, redb::Error> {
        match table.get("revision")? {
            Some(val) => Ok(serde_json::from_slice(val.value()).unwrap()),
            None => Ok(0),
        }
    }

    fn get_latest_kv<T: ReadableTable<&'static [u8], &'static [u8]>>(&self, table: &T, key: &[u8]) -> Result<Option<KeyValue>, redb::Error> {
        // Scan for all entries with this key prefix and find the latest
        let start = make_compound_key(key, 0);
        let end = make_compound_key(key, u64::MAX as i64);

        let mut latest: Option<KeyValue> = None;
        for result in table.range(start.as_slice()..=end.as_slice())? {
            let (k, v) = result?;
            let (entry_key, _rev) = split_compound_key(k.value());
            if entry_key == key {
                let kv: KeyValue = serde_json::from_slice(v.value()).unwrap();
                // Skip tombstones (version == 0)
                if kv.version > 0 {
                    latest = Some(kv);
                } else {
                    latest = None; // key was deleted
                }
            }
        }
        Ok(latest)
    }

    fn get_latest_kv_write(&self, table: &redb::Table<&[u8], &[u8]>, key: &[u8]) -> Result<Option<KeyValue>, redb::Error> {
        let start = make_compound_key(key, 0);
        let end = make_compound_key(key, u64::MAX as i64);

        let mut latest: Option<KeyValue> = None;
        for result in table.range(start.as_slice()..=end.as_slice())? {
            let (k, v) = result?;
            let (entry_key, _rev) = split_compound_key(k.value());
            if entry_key == key {
                let kv: KeyValue = serde_json::from_slice(v.value()).unwrap();
                if kv.version > 0 {
                    latest = Some(kv);
                } else {
                    latest = None;
                }
            }
        }
        Ok(latest)
    }

    fn get_kv_at_revision<T: ReadableTable<&'static [u8], &'static [u8]>>(&self, table: &T, key: &[u8], max_rev: i64) -> Result<Option<KeyValue>, redb::Error> {
        let start = make_compound_key(key, 0);
        let end = make_compound_key(key, max_rev);

        let mut latest: Option<KeyValue> = None;
        for result in table.range(start.as_slice()..=end.as_slice())? {
            let (k, v) = result?;
            let (entry_key, _rev) = split_compound_key(k.value());
            if entry_key == key {
                let kv: KeyValue = serde_json::from_slice(v.value()).unwrap();
                if kv.version > 0 {
                    latest = Some(kv);
                } else {
                    latest = None;
                }
            }
        }
        Ok(latest)
    }

    fn scan_keys_in_range<T: ReadableTable<&'static [u8], &'static [u8]>>(&self, table: &T, start_key: &[u8], end_key: &[u8], max_rev: i64) -> Result<Vec<KeyValue>, redb::Error> {
        // This is simplified — a production implementation would use a key index
        let mut seen_keys: std::collections::HashMap<Vec<u8>, KeyValue> = std::collections::HashMap::new();

        for result in table.iter()? {
            let (k, v) = result?;
            let (key, rev) = split_compound_key(k.value());
            if key >= start_key && key < end_key && rev <= max_rev as u64 {
                let kv: KeyValue = serde_json::from_slice(v.value()).unwrap();
                if kv.version > 0 {
                    seen_keys.insert(key.to_vec(), kv);
                } else {
                    seen_keys.remove(key);
                }
            }
        }

        let mut kvs: Vec<KeyValue> = seen_keys.into_values().collect();
        kvs.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(kvs)
    }

    fn scan_keys_in_range_write(&self, table: &redb::Table<&[u8], &[u8]>, start_key: &[u8], end_key: &[u8]) -> Result<Vec<KeyValue>, redb::Error> {
        let mut seen_keys: std::collections::HashMap<Vec<u8>, KeyValue> = std::collections::HashMap::new();

        for result in table.iter()? {
            let (k, v) = result?;
            let (key, _rev) = split_compound_key(k.value());
            if key >= start_key && key < end_key {
                let kv: KeyValue = serde_json::from_slice(v.value()).unwrap();
                if kv.version > 0 {
                    seen_keys.insert(key.to_vec(), kv);
                } else {
                    seen_keys.remove(key);
                }
            }
        }

        Ok(seen_keys.into_values().collect())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RevEntry {
    key: Vec<u8>,
    event_type: i32, // 0 = PUT, 1 = DELETE
}

/// Encode (key, revision) as a compound key for lexicographic ordering.
/// Format: key_bytes + \x00 + 8-byte big-endian revision
fn make_compound_key(key: &[u8], revision: i64) -> Vec<u8> {
    let mut compound = Vec::with_capacity(key.len() + 1 + 8);
    compound.extend_from_slice(key);
    compound.push(0x00);
    compound.extend_from_slice(&(revision as u64).to_be_bytes());
    compound
}

/// Split a compound key back into (key, revision).
fn split_compound_key(compound: &[u8]) -> (&[u8], u64) {
    let sep_pos = compound.len() - 9; // last 9 bytes = \x00 + 8 bytes
    let key = &compound[..sep_pos];
    let rev_bytes: [u8; 8] = compound[sep_pos + 1..].try_into().unwrap();
    (key, u64::from_be_bytes(rev_bytes))
}
```

**Step 2: Write tests in tests/kv_store_test.rs**

```rust
use barkeeper::kv::store::KvStore;
use tempfile::tempdir;

#[test]
fn test_empty_store() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();
    assert_eq!(store.current_revision().unwrap(), 0);
}

#[test]
fn test_put_and_range() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    let result = store.put(b"hello".to_vec(), b"world".to_vec(), 0).unwrap();
    assert_eq!(result.revision, 1);
    assert!(result.prev_kv.is_none());

    let range = store.range(b"hello", b"", 0, 0).unwrap();
    assert_eq!(range.kvs.len(), 1);
    assert_eq!(range.kvs[0].key, b"hello");
    assert_eq!(range.kvs[0].value, b"world");
    assert_eq!(range.kvs[0].create_revision, 1);
    assert_eq!(range.kvs[0].mod_revision, 1);
    assert_eq!(range.kvs[0].version, 1);
}

#[test]
fn test_put_overwrite() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    store.put(b"key".to_vec(), b"v1".to_vec(), 0).unwrap();
    let result = store.put(b"key".to_vec(), b"v2".to_vec(), 0).unwrap();
    assert_eq!(result.revision, 2);
    assert!(result.prev_kv.is_some());
    assert_eq!(result.prev_kv.unwrap().value, b"v1");

    let range = store.range(b"key", b"", 0, 0).unwrap();
    assert_eq!(range.kvs[0].value, b"v2");
    assert_eq!(range.kvs[0].create_revision, 1); // same create revision
    assert_eq!(range.kvs[0].mod_revision, 2);
    assert_eq!(range.kvs[0].version, 2);
}

#[test]
fn test_delete_range_single() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    store.put(b"key".to_vec(), b"val".to_vec(), 0).unwrap();
    let result = store.delete_range(b"key", b"").unwrap();
    assert_eq!(result.deleted, 1);

    let range = store.range(b"key", b"", 0, 0).unwrap();
    assert_eq!(range.kvs.len(), 0);
}

#[test]
fn test_delete_nonexistent() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    let result = store.delete_range(b"nope", b"").unwrap();
    assert_eq!(result.deleted, 0);
}

#[test]
fn test_range_prefix() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    store.put(b"aa".to_vec(), b"1".to_vec(), 0).unwrap();
    store.put(b"ab".to_vec(), b"2".to_vec(), 0).unwrap();
    store.put(b"b".to_vec(), b"3".to_vec(), 0).unwrap();

    // Range [a, b) should return aa, ab
    let range = store.range(b"a", b"b", 0, 0).unwrap();
    assert_eq!(range.kvs.len(), 2);
    assert_eq!(range.kvs[0].key, b"aa");
    assert_eq!(range.kvs[1].key, b"ab");
}

#[test]
fn test_revision_query() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    store.put(b"key".to_vec(), b"v1".to_vec(), 0).unwrap(); // rev 1
    store.put(b"key".to_vec(), b"v2".to_vec(), 0).unwrap(); // rev 2

    // Query at revision 1
    let range = store.range(b"key", b"", 0, 1).unwrap();
    assert_eq!(range.kvs[0].value, b"v1");

    // Query at latest
    let range = store.range(b"key", b"", 0, 0).unwrap();
    assert_eq!(range.kvs[0].value, b"v2");
}

#[test]
fn test_range_limit() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();

    store.put(b"a".to_vec(), b"1".to_vec(), 0).unwrap();
    store.put(b"b".to_vec(), b"2".to_vec(), 0).unwrap();
    store.put(b"c".to_vec(), b"3".to_vec(), 0).unwrap();

    let range = store.range(b"a", b"z", 2, 0).unwrap();
    assert_eq!(range.kvs.len(), 2);
    assert!(range.more);
    assert_eq!(range.count, 3);
}
```

**Step 3: Run tests**

Run: `cargo test --test kv_store_test`
Expected: All tests PASS

**Step 4: Commit**

```bash
git add src/kv/ tests/kv_store_test.rs
git commit -m "feat(kv): add MVCC key-value store with redb backend"
```

---

### Task 7: StateMachine actor — applies committed entries to KV store

**Files:**
- Create: `src/kv/state_machine.rs`
- Modify: `src/kv/mod.rs`

The StateMachine receives committed log entries from RaftNode and applies them to the KvStore. It also notifies the WatchHub of mutations.

**Step 1: Create src/kv/state_machine.rs**

The StateMachine actor receives `Vec<LogEntry>` from the Raft apply channel, decodes the command, and applies it to the KvStore.

Define a `KvCommand` enum that represents all KV operations:

```rust
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::store::{KvStore, PutResult, DeleteResult, RangeResult, WatchEvent};
use crate::raft::messages::{LogEntry, LogEntryData};

/// Commands that can be applied to the KV store via Raft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KvCommand {
    Put { key: Vec<u8>, value: Vec<u8>, lease_id: i64 },
    DeleteRange { key: Vec<u8>, range_end: Vec<u8> },
    // Txn and Compact added in later tasks
}

/// Applies committed Raft entries to the KV store.
pub struct StateMachine {
    store: KvStore,
    watch_tx: Option<mpsc::Sender<WatchEvent>>,
}

impl StateMachine {
    pub fn new(store: KvStore, watch_tx: Option<mpsc::Sender<WatchEvent>>) -> Self {
        StateMachine { store, watch_tx }
    }

    /// Apply a batch of committed log entries.
    pub async fn apply(&self, entries: Vec<LogEntry>) {
        for entry in entries {
            match entry.data {
                LogEntryData::Command(data) => {
                    if let Ok(cmd) = serde_json::from_slice::<KvCommand>(&data) {
                        self.apply_command(cmd).await;
                    }
                }
                LogEntryData::Noop => {} // no-op, nothing to apply
                LogEntryData::ConfigChange(_) => {
                    // Handled by ClusterManager, not StateMachine
                }
            }
        }
    }

    async fn apply_command(&self, cmd: KvCommand) {
        match cmd {
            KvCommand::Put { key, value, lease_id } => {
                match self.store.put(key, value, lease_id) {
                    Ok(result) => {
                        tracing::debug!(revision = result.revision, "applied put");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to apply put");
                    }
                }
            }
            KvCommand::DeleteRange { key, range_end } => {
                match self.store.delete_range(&key, &range_end) {
                    Ok(result) => {
                        tracing::debug!(revision = result.revision, deleted = result.deleted, "applied delete");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to apply delete");
                    }
                }
            }
        }
    }

    pub fn store(&self) -> &KvStore {
        &self.store
    }
}

/// Spawn the state machine apply loop.
pub async fn spawn_state_machine(
    store: KvStore,
    mut apply_rx: mpsc::Receiver<Vec<LogEntry>>,
    watch_tx: Option<mpsc::Sender<WatchEvent>>,
) {
    let sm = StateMachine::new(store, watch_tx);
    tokio::spawn(async move {
        while let Some(entries) = apply_rx.recv().await {
            sm.apply(entries).await;
        }
    });
}
```

**Step 2: Commit**

```bash
git add src/kv/state_machine.rs src/kv/mod.rs
git commit -m "feat(kv): add StateMachine actor for applying committed entries"
```

---

## Phase 4: gRPC API

### Task 8: KV gRPC service

**Files:**
- Create: `src/api/kv_service.rs`
- Modify: `src/api/mod.rs`

Implement the etcd KV gRPC service (Range, Put, DeleteRange, Txn, Compact).

**Step 1: Create src/api/kv_service.rs**

```rust
use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::kv::store::KvStore;
use crate::kv::state_machine::KvCommand;
use crate::raft::node::RaftHandle;
use crate::raft::messages::ClientProposalResult;
use crate::proto::etcdserverpb::{
    kv_server::Kv,
    RangeRequest, RangeResponse,
    PutRequest, PutResponse,
    DeleteRangeRequest, DeleteRangeResponse,
    TxnRequest, TxnResponse,
    CompactionRequest, CompactionResponse,
    ResponseHeader, KeyValue,
};

pub struct KvService {
    raft: RaftHandle,
    store: Arc<KvStore>,
    cluster_id: u64,
    member_id: u64,
}

impl KvService {
    pub fn new(raft: RaftHandle, store: Arc<KvStore>, cluster_id: u64, member_id: u64) -> Self {
        KvService { raft, store, cluster_id, member_id }
    }

    fn make_header(&self, revision: i64) -> ResponseHeader {
        ResponseHeader {
            cluster_id: self.cluster_id,
            member_id: self.member_id,
            revision,
            raft_term: 0, // TODO: get from raft state
        }
    }
}

#[tonic::async_trait]
impl Kv for KvService {
    async fn range(&self, request: Request<RangeRequest>) -> Result<Response<RangeResponse>, Status> {
        let req = request.into_inner();

        let result = self.store.range(&req.key, &req.range_end, req.limit, req.revision)
            .map_err(|e| Status::internal(format!("storage error: {e}")))?;

        let revision = self.store.current_revision()
            .map_err(|e| Status::internal(format!("storage error: {e}")))?;

        Ok(Response::new(RangeResponse {
            header: Some(self.make_header(revision)),
            kvs: result.kvs,
            more: result.more,
            count: result.count,
        }))
    }

    async fn put(&self, request: Request<PutRequest>) -> Result<Response<PutResponse>, Status> {
        let req = request.into_inner();

        let cmd = KvCommand::Put {
            key: req.key.clone(),
            value: req.value.clone(),
            lease_id: req.lease,
        };

        let data = serde_json::to_vec(&cmd).unwrap();
        let result = self.raft.propose(data).await
            .map_err(|e| Status::unavailable(format!("raft error: {e}")))?;

        match result {
            ClientProposalResult::Success { revision, .. } => {
                // Optionally fetch prev_kv
                let prev_kv = if req.prev_kv {
                    self.store.range(&req.key, b"", 0, revision - 1)
                        .ok()
                        .and_then(|r| r.kvs.into_iter().next())
                } else {
                    None
                };

                Ok(Response::new(PutResponse {
                    header: Some(self.make_header(revision)),
                    prev_kv,
                }))
            }
            ClientProposalResult::NotLeader { leader_id } => {
                Err(Status::unavailable(format!("not leader, leader is {:?}", leader_id)))
            }
            ClientProposalResult::Error(e) => {
                Err(Status::internal(e))
            }
        }
    }

    async fn delete_range(&self, request: Request<DeleteRangeRequest>) -> Result<Response<DeleteRangeResponse>, Status> {
        let req = request.into_inner();

        let cmd = KvCommand::DeleteRange {
            key: req.key.clone(),
            range_end: req.range_end.clone(),
        };

        let data = serde_json::to_vec(&cmd).unwrap();
        let result = self.raft.propose(data).await
            .map_err(|e| Status::unavailable(format!("raft error: {e}")))?;

        match result {
            ClientProposalResult::Success { revision, .. } => {
                Ok(Response::new(DeleteRangeResponse {
                    header: Some(self.make_header(revision)),
                    deleted: 0, // TODO: get from apply result
                    prev_kvs: vec![],
                }))
            }
            ClientProposalResult::NotLeader { leader_id } => {
                Err(Status::unavailable(format!("not leader, leader is {:?}", leader_id)))
            }
            ClientProposalResult::Error(e) => {
                Err(Status::internal(e))
            }
        }
    }

    async fn txn(&self, request: Request<TxnRequest>) -> Result<Response<TxnResponse>, Status> {
        // TODO: implement in Task 9
        Err(Status::unimplemented("txn not yet implemented"))
    }

    async fn compact(&self, request: Request<CompactionRequest>) -> Result<Response<CompactionResponse>, Status> {
        // TODO: implement in Task 10
        Err(Status::unimplemented("compact not yet implemented"))
    }
}
```

**Step 2: Commit**

```bash
git add src/api/kv_service.rs src/api/mod.rs
git commit -m "feat(api): add KV gRPC service (Range, Put, DeleteRange)"
```

---

### Task 9: gRPC server startup and single-node integration

**Files:**
- Create: `src/api/server.rs`
- Modify: `src/main.rs`
- Create: `tests/integration_test.rs`

Wire everything together: start a single-node barkeeper that accepts gRPC clients.

**Step 1: Create src/api/server.rs**

Start a tonic gRPC server with the KV service, plus a basic HTTP gateway with axum.

```rust
use std::sync::Arc;
use std::net::SocketAddr;
use tonic::transport::Server;
use tokio::sync::mpsc;

use crate::api::kv_service::KvService;
use crate::kv::store::KvStore;
use crate::kv::state_machine::spawn_state_machine;
use crate::raft::node::{spawn_raft_node, RaftConfig, RaftHandle};
use crate::proto::etcdserverpb::kv_server::KvServer;

pub struct BarkeepServer {
    pub raft: RaftHandle,
    pub store: Arc<KvStore>,
}

impl BarkeepServer {
    pub async fn start(config: RaftConfig, listen_addr: SocketAddr) -> Result<Self, Box<dyn std::error::Error>> {
        // Create data directory
        std::fs::create_dir_all(&config.data_dir)?;

        // Open KV store
        let store = Arc::new(KvStore::open(format!("{}/kv.redb", config.data_dir))?);

        // Create apply channel
        let (apply_tx, apply_rx) = mpsc::channel(256);

        // Spawn state machine
        spawn_state_machine(
            KvStore::open(format!("{}/kv.redb", config.data_dir))?,
            apply_rx,
            None, // watch_tx added later
        ).await;

        // Spawn Raft node
        let raft = spawn_raft_node(Arc::new(rebar_core::Runtime::new(config.node_id)), config.clone(), apply_tx).await;

        // Create gRPC services
        let kv_service = KvService::new(raft.clone(), store.clone(), 1, config.node_id);

        // Start gRPC server
        let server = BarkeepServer {
            raft: raft.clone(),
            store: store.clone(),
        };

        tokio::spawn(async move {
            tracing::info!(%listen_addr, "barkeeper gRPC server starting");
            Server::builder()
                .add_service(KvServer::new(kv_service))
                .serve(listen_addr)
                .await
                .expect("gRPC server failed");
        });

        Ok(server)
    }
}
```

**Step 2: Update main.rs**

```rust
use barkeeper::api::server::BarkeepServer;
use barkeeper::raft::node::RaftConfig;
use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser)]
#[command(name = "barkeeper", about = "etcd-compatible distributed KV store on Rebar")]
struct Cli {
    #[arg(long, default_value = "default")]
    name: String,

    #[arg(long, default_value = "data.barkeeper")]
    data_dir: String,

    #[arg(long, default_value = "127.0.0.1:2379")]
    listen_client_urls: String,

    #[arg(long, default_value = "1")]
    node_id: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let config = RaftConfig {
        node_id: cli.node_id,
        data_dir: cli.data_dir,
        ..Default::default()
    };

    let addr: SocketAddr = cli.listen_client_urls.parse()?;
    let _server = BarkeepServer::start(config, addr).await?;

    tracing::info!(name = %cli.name, "barkeeper running");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
```

**Step 3: Write integration test**

```rust
// tests/integration_test.rs
// Tests a single-node barkeeper end-to-end

// This test requires the full server to be running.
// For now, test KvStore + StateMachine directly.

use barkeeper::kv::store::KvStore;
use barkeeper::kv::state_machine::{StateMachine, KvCommand};
use barkeeper::raft::messages::{LogEntry, LogEntryData};
use tempfile::tempdir;

#[tokio::test]
async fn test_state_machine_apply_put() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();
    let sm = StateMachine::new(store, None);

    let cmd = KvCommand::Put {
        key: b"hello".to_vec(),
        value: b"world".to_vec(),
        lease_id: 0,
    };

    let entry = LogEntry {
        term: 1,
        index: 1,
        data: LogEntryData::Command(serde_json::to_vec(&cmd).unwrap()),
    };

    sm.apply(vec![entry]).await;

    let result = sm.store().range(b"hello", b"", 0, 0).unwrap();
    assert_eq!(result.kvs.len(), 1);
    assert_eq!(result.kvs[0].value, b"world");
}

#[tokio::test]
async fn test_state_machine_apply_delete() {
    let dir = tempdir().unwrap();
    let store = KvStore::open(dir.path().join("test.redb")).unwrap();
    let sm = StateMachine::new(store, None);

    // Put then delete
    let put = LogEntry {
        term: 1,
        index: 1,
        data: LogEntryData::Command(serde_json::to_vec(&KvCommand::Put {
            key: b"key".to_vec(),
            value: b"val".to_vec(),
            lease_id: 0,
        }).unwrap()),
    };

    let delete = LogEntry {
        term: 1,
        index: 2,
        data: LogEntryData::Command(serde_json::to_vec(&KvCommand::DeleteRange {
            key: b"key".to_vec(),
            range_end: b"".to_vec(),
        }).unwrap()),
    };

    sm.apply(vec![put, delete]).await;

    let result = sm.store().range(b"key", b"", 0, 0).unwrap();
    assert_eq!(result.kvs.len(), 0);
}
```

**Step 4: Run tests**

Run: `cargo test --test integration_test`
Expected: All tests PASS

**Step 5: Commit**

```bash
git add src/api/ src/main.rs tests/integration_test.rs
git commit -m "feat: add gRPC server startup and single-node integration"
```

---

## Phase 5: Supporting Services

### Task 10: Watch service

**Files:**
- Create: `src/watch/hub.rs`
- Create: `src/api/watch_service.rs`

Implement the WatchHub actor and Watch gRPC streaming service.

The WatchHub maintains a set of watchers, receives events from the StateMachine, and fans them out to matching watch streams.

This task implements:
- WatchHub struct with `create_watch`, `cancel_watch`, `notify` methods
- Watch gRPC bidirectional streaming service
- Integration with StateMachine via mpsc channel

**Commit:** `feat(watch): add WatchHub actor and Watch gRPC service`

---

### Task 11: Lease service

**Files:**
- Create: `src/lease/manager.rs`
- Create: `src/api/lease_service.rs`

Implement the LeaseManager and Lease gRPC service.

The LeaseManager tracks active leases with TTLs. Lease operations go through Raft for consistency. The leader runs expiry timers.

This task implements:
- LeaseManager struct with `grant`, `revoke`, `keepalive`, `time_to_live`, `list` methods
- Lease gRPC service (LeaseGrant, LeaseRevoke, LeaseKeepAlive stream, LeaseTimeToLive, LeaseLeases)
- Expiry timer that submits revoke proposals through Raft

**Commit:** `feat(lease): add LeaseManager actor and Lease gRPC service`

---

### Task 12: Cluster service and SWIM bridge

**Files:**
- Create: `src/cluster/manager.rs`
- Create: `src/api/cluster_service.rs`

Implement the ClusterManager that bridges SWIM membership with Raft membership, plus the Cluster gRPC service.

This task implements:
- ClusterManager that listens for SWIM membership changes and proposes Raft config changes
- Cluster gRPC service (MemberAdd, MemberRemove, MemberUpdate, MemberList, MemberPromote)

**Commit:** `feat(cluster): add ClusterManager and Cluster gRPC service`

---

### Task 13: HTTP/JSON gateway

**Files:**
- Create: `src/api/gateway.rs`

Implement the axum-based HTTP gateway that maps JSON requests to gRPC service calls, matching etcd's grpc-gateway endpoints.

Endpoints:
- `POST /v3/kv/range` → KV.Range
- `POST /v3/kv/put` → KV.Put
- `POST /v3/kv/deleterange` → KV.DeleteRange
- `POST /v3/kv/txn` → KV.Txn
- `POST /v3/kv/compaction` → KV.Compact
- `POST /v3/lease/grant` → Lease.LeaseGrant
- `POST /v3/lease/revoke` → Lease.LeaseRevoke
- `POST /v3/lease/timetolive` → Lease.LeaseTimeToLive
- `POST /v3/lease/leases` → Lease.LeaseLeases
- `POST /v3/cluster/member/list` → Cluster.MemberList
- `POST /v3/maintenance/status` → Maintenance.Status

**Commit:** `feat(api): add HTTP/JSON gateway matching etcd grpc-gateway endpoints`

---

## Phase 6: Multi-Node and Polish

### Task 14: Multi-node Raft via Rebar DistributedRouter

**Files:**
- Modify: `src/raft/node.rs`
- Create: `src/raft/transport.rs`

Wire the RaftNode actor to send/receive Raft RPCs via Rebar's DistributedRouter. When the RaftCore emits `Action::SendMessage`, encode the RaftMessage as an rmpv::Value and send it to the peer's RaftNode process via `ctx.send()`.

On the receiving side, the RaftNode process receives Rebar messages, decodes them, and feeds them into `core.step(Event::Message { ... })`.

**Commit:** `feat(raft): wire multi-node Raft RPCs via Rebar DistributedRouter`

---

### Task 15: Raft snapshots

**Files:**
- Modify: `src/raft/core.rs`
- Create: `src/raft/snapshot.rs`

Implement InstallSnapshot RPC handling in the Raft core, plus snapshot creation from the KV store and restoration.

**Commit:** `feat(raft): add snapshot support for InstallSnapshot RPC`

---

### Task 16: Maintenance service

**Files:**
- Create: `src/api/maintenance_service.rs`

Implement the Maintenance gRPC service: Status, Alarm, Defragment, Hash, HashKV, Snapshot stream, MoveLeader.

**Commit:** `feat(api): add Maintenance gRPC service`

---

### Task 17: Auth service

**Files:**
- Create: `src/auth/manager.rs`
- Create: `src/api/auth_service.rs`

Implement the Auth gRPC service with user/role/permission RBAC.

**Commit:** `feat(auth): add Auth service with user/role/permission RBAC`

---

### Task 18: Txn (compare-and-swap) and Compact

**Files:**
- Modify: `src/kv/store.rs` (add `txn` and `compact` methods)
- Modify: `src/api/kv_service.rs` (implement Txn and Compact RPCs)
- Create: `tests/txn_test.rs`

Implement etcd's transaction primitive: `if (compare conditions) then (success_ops) else (failure_ops)`.

**Commit:** `feat(kv): add Txn compare-and-swap and Compact operations`

---

### Task 19: etcd compatibility tests

**Files:**
- Create: `tests/compat_test.rs`

Run etcdctl against a running barkeeper instance to verify wire compatibility:
- `etcdctl put foo bar`
- `etcdctl get foo`
- `etcdctl del foo`
- `etcdctl watch foo &` then `etcdctl put foo bar2`
- `etcdctl lease grant 60` then `etcdctl put foo bar --lease=<id>`
- `etcdctl member list`

**Commit:** `test: add etcd compatibility tests using etcdctl`

---

### Task 20: README, documentation, and push

**Files:**
- Modify: `README.md` (comprehensive documentation)

Write a full README covering:
- What barkeeper is (etcd-compatible KV store on Rebar)
- Quick start (single-node, multi-node)
- CLI flags (matching etcd conventions)
- Architecture overview
- Differences from etcd
- Building from source
- License (Apache 2.0)

**Commit:** `docs: add comprehensive README`

Then push: `git push origin main`

---

## Summary

| Task | Phase | Description |
|------|-------|-------------|
| 1 | Foundation | Repo scaffold, proto codegen |
| 2 | Foundation | Raft message types and state |
| 3 | Foundation | LogStore (redb WAL) |
| 4 | Raft | Core state machine (election + replication) |
| 5 | Raft | RaftNode actor (timers, event loop) |
| 6 | KV Store | MVCC KV store (redb) |
| 7 | KV Store | StateMachine actor (apply loop) |
| 8 | gRPC API | KV gRPC service |
| 9 | gRPC API | Server startup + integration |
| 10 | Services | Watch service |
| 11 | Services | Lease service |
| 12 | Services | Cluster + SWIM bridge |
| 13 | Services | HTTP/JSON gateway |
| 14 | Multi-Node | Raft RPCs via DistributedRouter |
| 15 | Multi-Node | Raft snapshots |
| 16 | Multi-Node | Maintenance service |
| 17 | Multi-Node | Auth service |
| 18 | Multi-Node | Txn + Compact |
| 19 | Polish | etcd compatibility tests |
| 20 | Polish | README + docs + push |
