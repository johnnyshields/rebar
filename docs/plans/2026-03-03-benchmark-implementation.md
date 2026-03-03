# HTTP Mesh Benchmark Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build identical 3-service HTTP microservice meshes in Rebar, Elixir/Phoenix, Go, and Actix, orchestrated via Docker Compose, with a shared benchmark harness measuring throughput, latency, fault tolerance, spawn rate, and cross-node messaging.

**Architecture:** Each stack has 3 services (Gateway, Compute, Store) in separate containers discovering each other via framework-native mechanisms. A shared oha-based harness runs 5 scenarios against each stack. Results are collected as JSON and rendered into comparison tables.

**Tech Stack:** Rust (axum, rebar-core, rebar-cluster), Elixir (Phoenix, libcluster), Go (net/http, gRPC, memberlist), Rust (actix-web, actix actors), Docker Compose, oha, Python (report generation)

**Design Doc:** `docs/plans/2026-03-03-benchmark-design.md`

---

## Phase 0: Prerequisites

### Task 1: Install Docker and oha

**Step 1: Install Docker Engine**

```bash
sudo apt-get update
sudo apt-get install -y docker.io docker-compose-v2
sudo usermod -aG docker $USER
# Log out and back in, or use newgrp docker
```

**Step 2: Install oha**

```bash
cargo install oha
```

**Step 3: Verify**

```bash
docker --version
docker compose version
oha --version
```

**Step 4: Commit nothing** (system-level installs)

---

## Phase 1: Rebar HTTP Mesh (the main implementation)

This is the most complex stack because we need to wire together all Rebar components into a working distributed system. The other stacks use established frameworks with built-in clustering.

### Task 2: Scaffold bench directory and Rebar workspace

**Files:**
- Create: `bench/rebar/Cargo.toml` (workspace)
- Create: `bench/rebar/gateway/Cargo.toml`
- Create: `bench/rebar/gateway/src/main.rs`
- Create: `bench/rebar/compute/Cargo.toml`
- Create: `bench/rebar/compute/src/main.rs`
- Create: `bench/rebar/store/Cargo.toml`
- Create: `bench/rebar/store/src/main.rs`
- Create: `bench/rebar/common/Cargo.toml`
- Create: `bench/rebar/common/src/lib.rs`

The `common` crate contains shared types (fib function, service discovery helpers, HTTP response types) used by all 3 services.

**Step 1: Create workspace Cargo.toml**

```toml
[workspace]
resolver = "2"
members = ["gateway", "compute", "store", "common"]

[workspace.package]
edition = "2024"

[workspace.dependencies]
rebar-core = { path = "../../crates/rebar-core" }
rebar-cluster = { path = "../../crates/rebar-cluster" }
tokio = { version = "1", features = ["full"] }
axum = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
rmpv = "1"
```

**Step 2: Create common crate**

`bench/rebar/common/Cargo.toml`:
```toml
[package]
name = "bench-common"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
```

`bench/rebar/common/src/lib.rs`:
```rust
use serde::{Deserialize, Serialize};

pub fn fib(n: u64) -> u64 {
    if n <= 1 {
        return n;
    }
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 2..=n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeRequest {
    pub n: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeResponse {
    pub result: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreValue {
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreResponse {
    pub key: String,
    pub value: String,
}

pub const GATEWAY_HTTP_PORT: u16 = 8080;
pub const COMPUTE_HTTP_PORT: u16 = 8081;
pub const STORE_HTTP_PORT: u16 = 8082;

pub const GATEWAY_REBAR_PORT: u16 = 9000;
pub const COMPUTE_REBAR_PORT: u16 = 9001;
pub const STORE_REBAR_PORT: u16 = 9002;
```

**Step 3: Create gateway/compute/store Cargo.toml stubs**

Each service Cargo.toml follows this pattern (substitute name):
```toml
[package]
name = "bench-gateway"
version = "0.1.0"
edition.workspace = true

[dependencies]
bench-common = { path = "../common" }
rebar-core.workspace = true
rebar-cluster.workspace = true
tokio.workspace = true
axum.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
rmpv.workspace = true
```

**Step 4: Create minimal main.rs stubs**

Each `src/main.rs`:
```rust
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("starting...");
}
```

**Step 5: Verify build**

```bash
cd bench/rebar && cargo build
```

**Step 6: Commit**

```bash
git add bench/rebar/ && git commit -m "bench: scaffold Rebar HTTP mesh workspace"
```

---

### Task 3: Implement Compute service

**Files:**
- Modify: `bench/rebar/compute/src/main.rs`

The Compute service is the simplest — it's a standalone HTTP server with a supervised worker pool. No inter-service transport needed (Gateway calls it directly for now, we'll add Rebar transport later).

**Step 1: Write compute service**

```rust
use std::sync::Arc;
use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use bench_common::{fib, ComputeRequest, ComputeResponse, COMPUTE_HTTP_PORT};
use rebar_core::process::ExitReason;
use rebar_core::runtime::Runtime;
use rebar_core::supervisor::*;

struct AppState {
    runtime: Arc<Runtime>,
    supervisor: SupervisorHandle,
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn compute(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ComputeRequest>,
) -> Result<Json<ComputeResponse>, StatusCode> {
    // Validate input — n < 0 or n > 92 triggers crash injection
    if req.n < 0 || req.n > 92 {
        // Spawn a worker that panics to exercise supervisor restart
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = state.supervisor.add_child(ChildEntry::new(
            ChildSpec::new("crash-worker").restart(RestartType::Permanent),
            move || {
                Box::pin(async move {
                    ExitReason::Abnormal("crash injection: invalid n".into())
                })
            },
        )).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let n = req.n as u64;

    // Spawn a supervised worker process to compute fib(n)
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.runtime.spawn(move |_ctx| async move {
        let result = fib(n);
        let _ = tx.send(result);
    }).await;

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(result)) => Ok(Json(ComputeResponse { result })),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(Runtime::new(2)); // node_id 2 for Compute

    let spec = SupervisorSpec::new(RestartStrategy::OneForOne)
        .max_restarts(100)
        .max_seconds(60);

    let supervisor = start_supervisor(runtime.clone(), spec, vec![]).await;

    let state = Arc::new(AppState { runtime, supervisor });

    let app = Router::new()
        .route("/health", get(health))
        .route("/compute", post(compute))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", COMPUTE_HTTP_PORT);
    tracing::info!("compute listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

**Step 2: Verify build**

```bash
cd bench/rebar && cargo build -p bench-compute
```

**Step 3: Commit**

```bash
git add bench/rebar/compute/ && git commit -m "bench(rebar): implement compute service with supervisor"
```

---

### Task 4: Implement Store service

**Files:**
- Modify: `bench/rebar/store/src/main.rs`

Process-per-key KV store. Each key gets its own long-lived Rebar process that holds the value and responds to get/put messages via channels.

**Step 1: Write store service**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use bench_common::{StoreResponse, StoreValue, STORE_HTTP_PORT};
use rebar_core::process::ProcessId;
use rebar_core::runtime::Runtime;
use tokio::sync::{oneshot, Mutex};

/// Commands sent to key-actor processes
enum KeyCommand {
    Get {
        reply: oneshot::Sender<Option<String>>,
    },
    Put {
        value: String,
        reply: oneshot::Sender<()>,
    },
}

struct AppState {
    runtime: Arc<Runtime>,
    /// Maps key name → channel sender for the key's actor process
    keys: Mutex<HashMap<String, tokio::sync::mpsc::Sender<KeyCommand>>>,
}

impl AppState {
    async fn get_or_spawn_key(&self, key: &str) -> tokio::sync::mpsc::Sender<KeyCommand> {
        let mut keys = self.keys.lock().await;
        if let Some(tx) = keys.get(key) {
            if !tx.is_closed() {
                return tx.clone();
            }
        }

        // Spawn a new key-actor process
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<KeyCommand>(64);
        let key_name = key.to_string();

        self.runtime
            .spawn(move |_ctx| async move {
                let mut value: Option<String> = None;
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        KeyCommand::Get { reply } => {
                            let _ = reply.send(value.clone());
                        }
                        KeyCommand::Put {
                            value: new_val,
                            reply,
                        } => {
                            value = Some(new_val);
                            let _ = reply.send(());
                        }
                    }
                }
                tracing::debug!("key actor for '{}' shutting down", key_name);
            })
            .await;

        keys.insert(key.to_string(), cmd_tx.clone());
        cmd_tx
    }
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn get_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<StoreResponse>, StatusCode> {
    let tx = state.get_or_spawn_key(&key).await;
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(KeyCommand::Get { reply: reply_tx })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await {
        Ok(Ok(Some(value))) => Ok(Json(StoreResponse { key, value })),
        Ok(Ok(None)) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn put_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<StoreValue>,
) -> StatusCode {
    let tx = state.get_or_spawn_key(&key).await;
    let (reply_tx, reply_rx) = oneshot::channel();
    if tx
        .send(KeyCommand::Put {
            value: body.value,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await {
        Ok(Ok(())) => StatusCode::OK,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let runtime = Arc::new(Runtime::new(3)); // node_id 3 for Store
    let state = Arc::new(AppState {
        runtime,
        keys: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/store/{key}", get(get_key).put(put_key))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", STORE_HTTP_PORT);
    tracing::info!("store listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

**Step 2: Verify build**

```bash
cd bench/rebar && cargo build -p bench-store
```

**Step 3: Commit**

```bash
git add bench/rebar/store/ && git commit -m "bench(rebar): implement store service with process-per-key"
```

---

### Task 5: Implement Gateway service

**Files:**
- Modify: `bench/rebar/gateway/src/main.rs`

Gateway accepts external HTTP, routes to Compute and Store via internal HTTP (first pass — we'll add Rebar transport in Task 6).

**Step 1: Write gateway service**

```rust
use std::sync::Arc;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use bench_common::*;

struct AppState {
    compute_url: String,
    store_url: String,
    client: reqwest::Client,
}

async fn health(State(state): State<Arc<AppState>>) -> StatusCode {
    let compute_ok = state
        .client
        .get(format!("{}/health", state.compute_url))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let store_ok = state
        .client
        .get(format!("{}/health", state.store_url))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if compute_ok && store_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn compute(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ComputeRequest>,
) -> Result<Json<ComputeResponse>, StatusCode> {
    let resp = state
        .client
        .post(format!("{}/compute", state.compute_url))
        .json(&req)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !resp.status().is_success() {
        return Err(StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    }

    let body: ComputeResponse = resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(body))
}

async fn get_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<StoreResponse>, StatusCode> {
    let resp = state
        .client
        .get(format!("{}/store/{}", state.store_url, key))
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(StatusCode::NOT_FOUND);
    }
    if !resp.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }

    let body: StoreResponse = resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(body))
}

async fn put_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<StoreValue>,
) -> StatusCode {
    let resp = state
        .client
        .put(format!("{}/store/{}", state.store_url, key))
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => StatusCode::OK,
        _ => StatusCode::BAD_GATEWAY,
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let compute_host = std::env::var("COMPUTE_HOST").unwrap_or_else(|_| "compute".into());
    let store_host = std::env::var("STORE_HOST").unwrap_or_else(|_| "store".into());

    let state = Arc::new(AppState {
        compute_url: format!("http://{}:{}", compute_host, COMPUTE_HTTP_PORT),
        store_url: format!("http://{}:{}", store_host, STORE_HTTP_PORT),
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap(),
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/compute", post(compute))
        .route("/store/{key}", get(get_key).put(put_key))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", GATEWAY_HTTP_PORT);
    tracing::info!("gateway listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

**Step 2: Add reqwest dependency to gateway**

Add to `bench/rebar/gateway/Cargo.toml`:
```toml
reqwest = { version = "0.12", features = ["json"] }
```

And to workspace deps in `bench/rebar/Cargo.toml`:
```toml
reqwest = { version = "0.12", features = ["json"] }
```

**Step 3: Verify build**

```bash
cd bench/rebar && cargo build
```

**Step 4: Commit**

```bash
git add bench/rebar/ && git commit -m "bench(rebar): implement gateway service with HTTP routing"
```

---

### Task 6: Rebar Dockerfile and Docker Compose

**Files:**
- Create: `bench/rebar/Dockerfile`
- Create: `bench/docker-compose.rebar.yml`

**Step 1: Write Dockerfile**

```dockerfile
FROM rust:1.85-bookworm AS builder

WORKDIR /app

# Copy the full rebar workspace (crates/) and bench/rebar/
COPY crates/ /app/crates/
COPY bench/rebar/ /app/bench/rebar/

WORKDIR /app/bench/rebar
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/bench/rebar/target/release/bench-gateway /usr/local/bin/gateway
COPY --from=builder /app/bench/rebar/target/release/bench-compute /usr/local/bin/compute
COPY --from=builder /app/bench/rebar/target/release/bench-store /usr/local/bin/store

CMD ["gateway"]
```

**Step 2: Write Docker Compose**

`bench/docker-compose.rebar.yml`:
```yaml
name: rebar-bench

services:
  gateway:
    build:
      context: ..
      dockerfile: bench/rebar/Dockerfile
    command: gateway
    environment:
      COMPUTE_HOST: compute
      STORE_HOST: store
      RUST_LOG: info
    ports:
      - "8080:8080"
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8080/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    depends_on:
      compute:
        condition: service_healthy
      store:
        condition: service_healthy
    networks:
      - rebar-mesh

  compute:
    build:
      context: ..
      dockerfile: bench/rebar/Dockerfile
    command: compute
    environment:
      RUST_LOG: info
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8081/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - rebar-mesh

  store:
    build:
      context: ..
      dockerfile: bench/rebar/Dockerfile
    command: store
    environment:
      RUST_LOG: info
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8082/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - rebar-mesh

  bench:
    image: ghcr.io/hatoo/oha:latest
    entrypoint: ["sleep", "infinity"]
    depends_on:
      gateway:
        condition: service_healthy
    networks:
      - rebar-mesh

networks:
  rebar-mesh:
    driver: bridge
```

**Step 3: Verify compose config parses**

```bash
cd /home/alexandernicholson/.pxycrab/workspace/rebar && docker compose -f bench/docker-compose.rebar.yml config
```

**Step 4: Commit**

```bash
git add bench/rebar/Dockerfile bench/docker-compose.rebar.yml && git commit -m "bench(rebar): add Dockerfile and docker-compose"
```

---

## Phase 2: Elixir/Phoenix Stack

### Task 7: Scaffold Elixir services

**Files:**
- Create: `bench/elixir/gateway/` (Phoenix app)
- Create: `bench/elixir/compute/` (Phoenix app)
- Create: `bench/elixir/store/` (Phoenix app)
- Create: `bench/elixir/Dockerfile`
- Create: `bench/docker-compose.elixir.yml`

**Step 1: Create gateway Phoenix app**

`bench/elixir/gateway/mix.exs`:
```elixir
defmodule Gateway.MixProject do
  use Mix.Project

  def project do
    [
      app: :gateway,
      version: "0.1.0",
      elixir: "~> 1.17",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [
      extra_applications: [:logger],
      mod: {Gateway.Application, []}
    ]
  end

  defp deps do
    [
      {:plug_cowboy, "~> 2.7"},
      {:jason, "~> 1.4"},
      {:req, "~> 0.5"},
      {:libcluster, "~> 3.4"}
    ]
  end
end
```

`bench/elixir/gateway/lib/gateway/application.ex`:
```elixir
defmodule Gateway.Application do
  use Application

  @impl true
  def start(_type, _args) do
    compute_host = System.get_env("COMPUTE_HOST", "compute")
    store_host = System.get_env("STORE_HOST", "store")

    topologies = [
      mesh: [
        strategy: Cluster.Strategy.Gossip,
        config: [port: 45892]
      ]
    ]

    children = [
      {Cluster.Supervisor, [topologies, [name: Gateway.ClusterSupervisor]]},
      {Plug.Cowboy, scheme: :http, plug: Gateway.Router, options: [port: 8080]}
    ]

    opts = [strategy: :one_for_one, name: Gateway.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
```

`bench/elixir/gateway/lib/gateway/router.ex`:
```elixir
defmodule Gateway.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  post "/compute" do
    compute_host = System.get_env("COMPUTE_HOST", "compute")
    url = "http://#{compute_host}:8081/compute"

    case Req.post(url, json: conn.body_params) do
      {:ok, %{status: 200, body: body}} ->
        conn
        |> put_resp_content_type("application/json")
        |> send_resp(200, Jason.encode!(body))

      _ ->
        send_resp(conn, 502, "Bad Gateway")
    end
  end

  get "/store/:key" do
    store_host = System.get_env("STORE_HOST", "store")
    url = "http://#{store_host}:8082/store/#{key}"

    case Req.get(url) do
      {:ok, %{status: 200, body: body}} ->
        conn
        |> put_resp_content_type("application/json")
        |> send_resp(200, Jason.encode!(body))

      {:ok, %{status: 404}} ->
        send_resp(conn, 404, "Not Found")

      _ ->
        send_resp(conn, 502, "Bad Gateway")
    end
  end

  put "/store/:key" do
    store_host = System.get_env("STORE_HOST", "store")
    url = "http://#{store_host}:8082/store/#{key}"

    case Req.put(url, json: conn.body_params) do
      {:ok, %{status: 200}} ->
        send_resp(conn, 200, "ok")

      _ ->
        send_resp(conn, 502, "Bad Gateway")
    end
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end
end
```

**Step 2: Create compute Elixir app**

`bench/elixir/compute/mix.exs`:
```elixir
defmodule Compute.MixProject do
  use Mix.Project

  def project do
    [
      app: :compute,
      version: "0.1.0",
      elixir: "~> 1.17",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [
      extra_applications: [:logger],
      mod: {Compute.Application, []}
    ]
  end

  defp deps do
    [
      {:plug_cowboy, "~> 2.7"},
      {:jason, "~> 1.4"}
    ]
  end
end
```

`bench/elixir/compute/lib/compute/application.ex`:
```elixir
defmodule Compute.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Task.Supervisor, name: Compute.WorkerSupervisor},
      {Plug.Cowboy, scheme: :http, plug: Compute.Router, options: [port: 8081]}
    ]

    opts = [strategy: :one_for_one, name: Compute.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
```

`bench/elixir/compute/lib/compute/router.ex`:
```elixir
defmodule Compute.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  post "/compute" do
    n = conn.body_params["n"]

    if n < 0 or n > 92 do
      # Crash injection — spawn a task that crashes
      Task.Supervisor.async_nolink(Compute.WorkerSupervisor, fn ->
        raise "crash injection: invalid n"
      end)

      send_resp(conn, 500, "Internal Server Error")
    else
      task =
        Task.Supervisor.async_nolink(Compute.WorkerSupervisor, fn ->
          fib(n)
        end)

      case Task.yield(task, 5000) || Task.shutdown(task) do
        {:ok, result} ->
          conn
          |> put_resp_content_type("application/json")
          |> send_resp(200, Jason.encode!(%{result: result}))

        _ ->
          send_resp(conn, 500, "Timeout")
      end
    end
  end

  defp fib(n) when n <= 1, do: n

  defp fib(n) do
    Enum.reduce(2..n, {0, 1}, fn _, {a, b} ->
      {b, a + b}
    end)
    |> elem(1)
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end
end
```

**Step 3: Create store Elixir app**

`bench/elixir/store/mix.exs`:
```elixir
defmodule Store.MixProject do
  use Mix.Project

  def project do
    [
      app: :store,
      version: "0.1.0",
      elixir: "~> 1.17",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [
      extra_applications: [:logger],
      mod: {Store.Application, []}
    ]
  end

  defp deps do
    [
      {:plug_cowboy, "~> 2.7"},
      {:jason, "~> 1.4"}
    ]
  end
end
```

`bench/elixir/store/lib/store/application.ex`:
```elixir
defmodule Store.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {DynamicSupervisor, name: Store.KeySupervisor, strategy: :one_for_one},
      {Plug.Cowboy, scheme: :http, plug: Store.Router, options: [port: 8082]}
    ]

    opts = [strategy: :one_for_one, name: Store.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
```

`bench/elixir/store/lib/store/key_agent.ex`:
```elixir
defmodule Store.KeyAgent do
  use Agent

  def start_link(key) do
    Agent.start_link(fn -> nil end, name: via(key))
  end

  def get(key) do
    case Registry.lookup(Store.KeyRegistry, key) do
      [{pid, _}] -> Agent.get(pid, & &1)
      [] -> nil
    end
  end

  def put(key, value) do
    pid = get_or_start(key)
    Agent.update(pid, fn _ -> value end)
  end

  defp get_or_start(key) do
    case Registry.lookup(Store.KeyRegistry, key) do
      [{pid, _}] ->
        pid

      [] ->
        case DynamicSupervisor.start_child(Store.KeySupervisor, {__MODULE__, key}) do
          {:ok, pid} -> pid
          {:error, {:already_started, pid}} -> pid
        end
    end
  end

  defp via(key), do: {:via, Registry, {Store.KeyRegistry, key}}
end
```

`bench/elixir/store/lib/store/router.ex`:
```elixir
defmodule Store.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  get "/store/:key" do
    case Store.KeyAgent.get(key) do
      nil ->
        send_resp(conn, 404, "Not Found")

      value ->
        conn
        |> put_resp_content_type("application/json")
        |> send_resp(200, Jason.encode!(%{key: key, value: value}))
    end
  end

  put "/store/:key" do
    value = conn.body_params["value"]
    Store.KeyAgent.put(key, value)
    send_resp(conn, 200, "ok")
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end
end
```

Update `bench/elixir/store/lib/store/application.ex` to include Registry:
```elixir
defmodule Store.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Registry, keys: :unique, name: Store.KeyRegistry},
      {DynamicSupervisor, name: Store.KeySupervisor, strategy: :one_for_one},
      {Plug.Cowboy, scheme: :http, plug: Store.Router, options: [port: 8082]}
    ]

    opts = [strategy: :one_for_one, name: Store.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
```

**Step 4: Create Dockerfile**

`bench/elixir/Dockerfile`:
```dockerfile
FROM elixir:1.17-otp-27-slim AS builder

ENV MIX_ENV=prod

# Build gateway
WORKDIR /app/gateway
COPY bench/elixir/gateway/mix.exs ./
RUN mix local.hex --force && mix local.rebar --force && mix deps.get && mix deps.compile
COPY bench/elixir/gateway/lib/ ./lib/
RUN mix release

# Build compute
WORKDIR /app/compute
COPY bench/elixir/compute/mix.exs ./
RUN mix deps.get && mix deps.compile
COPY bench/elixir/compute/lib/ ./lib/
RUN mix release

# Build store
WORKDIR /app/store
COPY bench/elixir/store/mix.exs ./
RUN mix deps.get && mix deps.compile
COPY bench/elixir/store/lib/ ./lib/
RUN mix release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libncurses6 libstdc++6 curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/gateway/_build/prod/rel/gateway /opt/gateway
COPY --from=builder /app/compute/_build/prod/rel/compute /opt/compute
COPY --from=builder /app/store/_build/prod/rel/store /opt/store

# Default to gateway
CMD ["/opt/gateway/bin/gateway", "start"]
```

**Step 5: Create Docker Compose**

`bench/docker-compose.elixir.yml`:
```yaml
name: elixir-bench

services:
  gateway:
    build:
      context: ..
      dockerfile: bench/elixir/Dockerfile
    command: /opt/gateway/bin/gateway start
    environment:
      COMPUTE_HOST: compute
      STORE_HOST: store
    ports:
      - "8080:8080"
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8080/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    depends_on:
      compute:
        condition: service_healthy
      store:
        condition: service_healthy
    networks:
      - elixir-mesh

  compute:
    build:
      context: ..
      dockerfile: bench/elixir/Dockerfile
    command: /opt/compute/bin/compute start
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8081/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - elixir-mesh

  store:
    build:
      context: ..
      dockerfile: bench/elixir/Dockerfile
    command: /opt/store/bin/store start
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8082/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - elixir-mesh

  bench:
    image: ghcr.io/hatoo/oha:latest
    entrypoint: ["sleep", "infinity"]
    depends_on:
      gateway:
        condition: service_healthy
    networks:
      - elixir-mesh

networks:
  elixir-mesh:
    driver: bridge
```

**Step 6: Commit**

```bash
git add bench/elixir/ bench/docker-compose.elixir.yml && git commit -m "bench(elixir): implement Phoenix HTTP mesh"
```

---

## Phase 3: Go Stack

### Task 8: Implement Go services

**Files:**
- Create: `bench/go/go.mod`
- Create: `bench/go/common/common.go`
- Create: `bench/go/gateway/main.go`
- Create: `bench/go/compute/main.go`
- Create: `bench/go/store/main.go`
- Create: `bench/go/Dockerfile`
- Create: `bench/docker-compose.go.yml`

**Step 1: Create go module and common package**

`bench/go/go.mod`:
```
module github.com/alexandernicholson/rebar/bench/go

go 1.24

require (
    github.com/gorilla/mux v1.8.1
)
```

`bench/go/common/common.go`:
```go
package common

func Fib(n int64) uint64 {
	if n <= 1 {
		return uint64(n)
	}
	var a, b uint64 = 0, 1
	for i := int64(2); i <= n; i++ {
		a, b = b, a+b
	}
	return b
}

const (
	GatewayHTTPPort = "8080"
	ComputeHTTPPort = "8081"
	StoreHTTPPort   = "8082"
)
```

**Step 2: Write compute service**

`bench/go/compute/main.go`:
```go
package main

import (
	"encoding/json"
	"log"
	"net/http"
	"sync"

	"github.com/alexandernicholson/rebar/bench/go/common"
)

type computeRequest struct {
	N int64 `json:"n"`
}

type computeResponse struct {
	Result uint64 `json:"result"`
}

var workerPool = sync.Pool{
	New: func() interface{} { return new(uint64) },
}

func healthHandler(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusOK)
	w.Write([]byte("ok"))
}

func computeHandler(w http.ResponseWriter, r *http.Request) {
	var req computeRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}

	if req.N < 0 || req.N > 92 {
		// Crash injection — simulate panic + recovery
		func() {
			defer func() { recover() }()
			panic("crash injection: invalid n")
		}()
		http.Error(w, "Internal Server Error", http.StatusInternalServerError)
		return
	}

	// Compute in a goroutine (analogous to spawning a process)
	ch := make(chan uint64, 1)
	go func() {
		result := common.Fib(req.N)
		ch <- result
	}()

	result := <-ch
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(computeResponse{Result: result})
}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/compute", computeHandler)

	addr := ":" + common.ComputeHTTPPort
	log.Printf("compute listening on %s", addr)
	log.Fatal(http.ListenAndServe(addr, mux))
}
```

**Step 3: Write store service**

`bench/go/store/main.go`:
```go
package main

import (
	"encoding/json"
	"log"
	"net/http"
	"strings"
	"sync"

	"github.com/alexandernicholson/rebar/bench/go/common"
)

type storeValue struct {
	Value string `json:"value"`
}

type storeResponse struct {
	Key   string `json:"key"`
	Value string `json:"value"`
}

// keyActor represents a goroutine managing a single key
type keyActor struct {
	value string
	mu    sync.RWMutex
}

var (
	keys   = make(map[string]*keyActor)
	keysMu sync.RWMutex
)

func getOrCreateKey(key string) *keyActor {
	keysMu.RLock()
	if actor, ok := keys[key]; ok {
		keysMu.RUnlock()
		return actor
	}
	keysMu.RUnlock()

	keysMu.Lock()
	defer keysMu.Unlock()
	if actor, ok := keys[key]; ok {
		return actor
	}
	actor := &keyActor{}
	keys[key] = actor
	return actor
}

func healthHandler(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusOK)
	w.Write([]byte("ok"))
}

func storeHandler(w http.ResponseWriter, r *http.Request) {
	// Extract key from path /store/{key}
	parts := strings.SplitN(strings.TrimPrefix(r.URL.Path, "/store/"), "/", 2)
	if len(parts) == 0 || parts[0] == "" {
		http.Error(w, "missing key", http.StatusBadRequest)
		return
	}
	key := parts[0]

	switch r.Method {
	case http.MethodGet:
		actor := getOrCreateKey(key)
		actor.mu.RLock()
		val := actor.value
		actor.mu.RUnlock()

		if val == "" {
			http.Error(w, "Not Found", http.StatusNotFound)
			return
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(storeResponse{Key: key, Value: val})

	case http.MethodPut:
		var body storeValue
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}

		actor := getOrCreateKey(key)
		actor.mu.Lock()
		actor.value = body.Value
		actor.mu.Unlock()

		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok"))

	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/store/", storeHandler)

	addr := ":" + common.StoreHTTPPort
	log.Printf("store listening on %s", addr)
	log.Fatal(http.ListenAndServe(addr, mux))
}
```

**Step 4: Write gateway service**

`bench/go/gateway/main.go`:
```go
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/alexandernicholson/rebar/bench/go/common"
)

var client = &http.Client{Timeout: 10 * time.Second}

func envOrDefault(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func healthHandler(w http.ResponseWriter, r *http.Request) {
	computeHost := envOrDefault("COMPUTE_HOST", "compute")
	storeHost := envOrDefault("STORE_HOST", "store")

	computeOK := ping(fmt.Sprintf("http://%s:%s/health", computeHost, common.ComputeHTTPPort))
	storeOK := ping(fmt.Sprintf("http://%s:%s/health", storeHost, common.StoreHTTPPort))

	if computeOK && storeOK {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok"))
	} else {
		w.WriteHeader(http.StatusServiceUnavailable)
		w.Write([]byte("unhealthy"))
	}
}

func ping(url string) bool {
	resp, err := client.Get(url)
	if err != nil {
		return false
	}
	resp.Body.Close()
	return resp.StatusCode == http.StatusOK
}

func computeHandler(w http.ResponseWriter, r *http.Request) {
	computeHost := envOrDefault("COMPUTE_HOST", "compute")
	url := fmt.Sprintf("http://%s:%s/compute", computeHost, common.ComputeHTTPPort)

	body, _ := io.ReadAll(r.Body)
	resp, err := client.Post(url, "application/json", bytes.NewReader(body))
	if err != nil {
		http.Error(w, "Bad Gateway", http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(resp.StatusCode)
	io.Copy(w, resp.Body)
}

func storeHandler(w http.ResponseWriter, r *http.Request) {
	storeHost := envOrDefault("STORE_HOST", "store")
	key := strings.TrimPrefix(r.URL.Path, "/store/")
	url := fmt.Sprintf("http://%s:%s/store/%s", storeHost, common.StoreHTTPPort, key)

	var resp *http.Response
	var err error

	switch r.Method {
	case http.MethodGet:
		resp, err = client.Get(url)
	case http.MethodPut:
		body, _ := io.ReadAll(r.Body)
		req, _ := http.NewRequest(http.MethodPut, url, bytes.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		resp, err = client.Do(req)
	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	if err != nil {
		http.Error(w, "Bad Gateway", http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()

	w.Header().Set("Content-Type", resp.Header.Get("Content-Type"))
	w.WriteHeader(resp.StatusCode)
	io.Copy(w, resp.Body)
}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", healthHandler)
	mux.HandleFunc("/compute", computeHandler)
	mux.HandleFunc("/store/", storeHandler)

	addr := ":" + common.GatewayHTTPPort
	log.Printf("gateway listening on %s", addr)
	log.Fatal(http.ListenAndServe(addr, mux))
}
```

**Step 5: Create Dockerfile and Docker Compose**

`bench/go/Dockerfile`:
```dockerfile
FROM golang:1.24-bookworm AS builder

WORKDIR /app
COPY bench/go/ ./

RUN go mod download
RUN CGO_ENABLED=0 go build -o /gateway ./gateway
RUN CGO_ENABLED=0 go build -o /compute ./compute
RUN CGO_ENABLED=0 go build -o /store ./store

FROM gcr.io/distroless/static-debian12
COPY --from=builder /gateway /usr/local/bin/gateway
COPY --from=builder /compute /usr/local/bin/compute
COPY --from=builder /store /usr/local/bin/store
CMD ["gateway"]
```

`bench/docker-compose.go.yml`:
```yaml
name: go-bench

services:
  gateway:
    build:
      context: ..
      dockerfile: bench/go/Dockerfile
    command: gateway
    environment:
      COMPUTE_HOST: compute
      STORE_HOST: store
    ports:
      - "8080:8080"
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD", "gateway", "--version"]
      interval: 2s
      timeout: 5s
      retries: 10
      start_period: 5s
    depends_on:
      compute:
        condition: service_started
      store:
        condition: service_started
    networks:
      - go-mesh

  compute:
    build:
      context: ..
      dockerfile: bench/go/Dockerfile
    command: compute
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    networks:
      - go-mesh

  store:
    build:
      context: ..
      dockerfile: bench/go/Dockerfile
    command: store
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    networks:
      - go-mesh

  bench:
    image: ghcr.io/hatoo/oha:latest
    entrypoint: ["sleep", "infinity"]
    depends_on:
      - gateway
    networks:
      - go-mesh

networks:
  go-mesh:
    driver: bridge
```

**Step 6: Commit**

```bash
git add bench/go/ bench/docker-compose.go.yml && git commit -m "bench(go): implement Go HTTP mesh"
```

---

## Phase 4: Actix Stack

### Task 9: Implement Actix services

**Files:**
- Create: `bench/actix/Cargo.toml` (workspace)
- Create: `bench/actix/common/` (shared crate)
- Create: `bench/actix/gateway/`
- Create: `bench/actix/compute/`
- Create: `bench/actix/store/`
- Create: `bench/actix/Dockerfile`
- Create: `bench/docker-compose.actix.yml`

**Step 1: Create workspace and common crate**

`bench/actix/Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["gateway", "compute", "store", "common"]

[workspace.package]
edition = "2024"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
actix-web = "4"
actix-rt = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json"] }
tracing = "0.1"
tracing-subscriber = "0.3"
```

`bench/actix/common/Cargo.toml`:
```toml
[package]
name = "bench-actix-common"
version = "0.1.0"
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
```

`bench/actix/common/src/lib.rs` — same as Rebar's common but standalone:
```rust
use serde::{Deserialize, Serialize};

pub fn fib(n: u64) -> u64 {
    if n <= 1 { return n; }
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 2..=n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeRequest { pub n: i64 }

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeResponse { pub result: u64 }

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreValue { pub value: String }

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreResponse { pub key: String, pub value: String }
```

**Step 2: Write compute service**

`bench/actix/compute/Cargo.toml`:
```toml
[package]
name = "bench-actix-compute"
version = "0.1.0"
edition.workspace = true

[dependencies]
bench-actix-common = { path = "../common" }
actix-web.workspace = true
actix-rt.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

`bench/actix/compute/src/main.rs`:
```rust
use actix_web::{web, App, HttpResponse, HttpServer};
use bench_actix_common::*;

async fn health() -> HttpResponse {
    HttpResponse::Ok().body("ok")
}

async fn compute(req: web::Json<ComputeRequest>) -> HttpResponse {
    if req.n < 0 || req.n > 92 {
        return HttpResponse::InternalServerError().body("crash injection");
    }
    let n = req.n as u64;
    let result = tokio::task::spawn_blocking(move || fib(n)).await.unwrap();
    HttpResponse::Ok().json(ComputeResponse { result })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("actix compute listening on 0.0.0.0:8081");
    HttpServer::new(|| {
        App::new()
            .route("/health", web::get().to(health))
            .route("/compute", web::post().to(compute))
    })
    .bind("0.0.0.0:8081")?
    .run()
    .await
}
```

**Step 3: Write store service**

`bench/actix/store/Cargo.toml`:
```toml
[package]
name = "bench-actix-store"
version = "0.1.0"
edition.workspace = true

[dependencies]
bench-actix-common = { path = "../common" }
actix-web.workspace = true
actix-rt.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
dashmap = "6"
```

`bench/actix/store/src/main.rs`:
```rust
use actix_web::{web, App, HttpResponse, HttpServer};
use bench_actix_common::*;
use dashmap::DashMap;
use std::sync::Arc;

struct AppState {
    store: DashMap<String, String>,
}

async fn health() -> HttpResponse {
    HttpResponse::Ok().body("ok")
}

async fn get_key(
    state: web::Data<Arc<AppState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let key = path.into_inner();
    match state.store.get(&key) {
        Some(val) => HttpResponse::Ok().json(StoreResponse {
            key,
            value: val.clone(),
        }),
        None => HttpResponse::NotFound().body("Not Found"),
    }
}

async fn put_key(
    state: web::Data<Arc<AppState>>,
    path: web::Path<String>,
    body: web::Json<StoreValue>,
) -> HttpResponse {
    let key = path.into_inner();
    state.store.insert(key, body.value.clone());
    HttpResponse::Ok().body("ok")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();
    let state = Arc::new(AppState {
        store: DashMap::new(),
    });
    tracing::info!("actix store listening on 0.0.0.0:8082");
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            .route("/store/{key}", web::get().to(get_key))
            .route("/store/{key}", web::put().to(put_key))
    })
    .bind("0.0.0.0:8082")?
    .run()
    .await
}
```

**Step 4: Write gateway service**

`bench/actix/gateway/Cargo.toml`:
```toml
[package]
name = "bench-actix-gateway"
version = "0.1.0"
edition.workspace = true

[dependencies]
bench-actix-common = { path = "../common" }
actix-web.workspace = true
actix-rt.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
reqwest.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

`bench/actix/gateway/src/main.rs`:
```rust
use actix_web::{web, App, HttpResponse, HttpServer};
use bench_actix_common::*;
use std::sync::Arc;

struct AppState {
    compute_url: String,
    store_url: String,
    client: reqwest::Client,
}

async fn health(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let compute_ok = state.client.get(format!("{}/health", state.compute_url))
        .send().await.map(|r| r.status().is_success()).unwrap_or(false);
    let store_ok = state.client.get(format!("{}/health", state.store_url))
        .send().await.map(|r| r.status().is_success()).unwrap_or(false);

    if compute_ok && store_ok {
        HttpResponse::Ok().body("ok")
    } else {
        HttpResponse::ServiceUnavailable().body("unhealthy")
    }
}

async fn compute(
    state: web::Data<Arc<AppState>>,
    req: web::Json<ComputeRequest>,
) -> HttpResponse {
    let resp = state.client
        .post(format!("{}/compute", state.compute_url))
        .json(&req.into_inner())
        .send().await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: ComputeResponse = r.json().await.unwrap();
            HttpResponse::Ok().json(body)
        }
        Ok(r) => HttpResponse::build(
            actix_web::http::StatusCode::from_u16(r.status().as_u16()).unwrap()
        ).finish(),
        Err(_) => HttpResponse::BadGateway().body("Bad Gateway"),
    }
}

async fn get_key(
    state: web::Data<Arc<AppState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let key = path.into_inner();
    let resp = state.client.get(format!("{}/store/{}", state.store_url, key))
        .send().await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: StoreResponse = r.json().await.unwrap();
            HttpResponse::Ok().json(body)
        }
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND =>
            HttpResponse::NotFound().body("Not Found"),
        _ => HttpResponse::BadGateway().body("Bad Gateway"),
    }
}

async fn put_key(
    state: web::Data<Arc<AppState>>,
    path: web::Path<String>,
    body: web::Json<StoreValue>,
) -> HttpResponse {
    let key = path.into_inner();
    let resp = state.client.put(format!("{}/store/{}", state.store_url, key))
        .json(&body.into_inner())
        .send().await;

    match resp {
        Ok(r) if r.status().is_success() => HttpResponse::Ok().body("ok"),
        _ => HttpResponse::BadGateway().body("Bad Gateway"),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();
    let compute_host = std::env::var("COMPUTE_HOST").unwrap_or_else(|_| "compute".into());
    let store_host = std::env::var("STORE_HOST").unwrap_or_else(|_| "store".into());

    let state = Arc::new(AppState {
        compute_url: format!("http://{}:8081", compute_host),
        store_url: format!("http://{}:8082", store_host),
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build().unwrap(),
    });

    tracing::info!("actix gateway listening on 0.0.0.0:8080");
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            .route("/compute", web::post().to(compute))
            .route("/store/{key}", web::get().to(get_key))
            .route("/store/{key}", web::put().to(put_key))
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
```

**Step 5: Create Dockerfile and Docker Compose**

`bench/actix/Dockerfile`:
```dockerfile
FROM rust:1.85-bookworm AS builder

WORKDIR /app
COPY bench/actix/ ./

RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/bench-actix-gateway /usr/local/bin/gateway
COPY --from=builder /app/target/release/bench-actix-compute /usr/local/bin/compute
COPY --from=builder /app/target/release/bench-actix-store /usr/local/bin/store

CMD ["gateway"]
```

`bench/docker-compose.actix.yml`:
```yaml
name: actix-bench

services:
  gateway:
    build:
      context: ..
      dockerfile: bench/actix/Dockerfile
    command: gateway
    environment:
      COMPUTE_HOST: compute
      STORE_HOST: store
      RUST_LOG: info
    ports:
      - "8080:8080"
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8080/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    depends_on:
      compute:
        condition: service_healthy
      store:
        condition: service_healthy
    networks:
      - actix-mesh

  compute:
    build:
      context: ..
      dockerfile: bench/actix/Dockerfile
    command: compute
    environment:
      RUST_LOG: info
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8081/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - actix-mesh

  store:
    build:
      context: ..
      dockerfile: bench/actix/Dockerfile
    command: store
    environment:
      RUST_LOG: info
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 512M
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:8082/health || exit 1"]
      interval: 2s
      timeout: 5s
      retries: 10
    networks:
      - actix-mesh

  bench:
    image: ghcr.io/hatoo/oha:latest
    entrypoint: ["sleep", "infinity"]
    depends_on:
      gateway:
        condition: service_healthy
    networks:
      - actix-mesh

networks:
  actix-mesh:
    driver: bridge
```

**Step 6: Commit**

```bash
git add bench/actix/ bench/docker-compose.actix.yml && git commit -m "bench(actix): implement Actix HTTP mesh"
```

---

## Phase 5: Benchmark Harness

### Task 10: Create benchmark runner scripts

**Files:**
- Create: `bench/harness/run.sh`
- Create: `bench/harness/scenarios.sh`
- Create: `bench/harness/wait-healthy.sh`

**Step 1: Write wait-healthy.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

URL="${1:?Usage: wait-healthy.sh <health-url> [timeout-seconds]}"
TIMEOUT="${2:-30}"
ELAPSED=0

echo "Waiting for $URL to be healthy (timeout: ${TIMEOUT}s)..."

while [ $ELAPSED -lt $TIMEOUT ]; do
    if curl -sf "$URL" > /dev/null 2>&1; then
        echo "OK: $URL is healthy after ${ELAPSED}s"
        exit 0
    fi
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

echo "TIMEOUT: $URL not healthy after ${TIMEOUT}s"
exit 1
```

**Step 2: Write scenarios.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

GATEWAY="${GATEWAY_URL:-http://gateway:8080}"
RESULTS_DIR="${RESULTS_DIR:-.}"

mkdir -p "$RESULTS_DIR"

echo "=== Scenario 1: Throughput Ramp ==="
for C in 1 10 50 100 500 1000; do
    echo "  Concurrency: $C"
    oha -c "$C" -z 15s --json \
        -m POST -d '{"n":20}' -T application/json \
        "$GATEWAY/compute" > "$RESULTS_DIR/throughput_c${C}.json" 2>/dev/null || true
done

echo "=== Scenario 2: Latency Profile ==="
oha -c 100 -z 30s --json \
    -m POST -d '{"n":30}' -T application/json \
    "$GATEWAY/compute" > "$RESULTS_DIR/latency.json" 2>/dev/null || true

echo "=== Scenario 3: Fault Tolerance (skipped in automated run — requires docker kill) ==="
# This scenario requires docker access from within the bench container.
# Run manually: docker kill <compute>, wait, measure recovery.
echo '{"note": "run manually with docker kill"}' > "$RESULTS_DIR/fault_tolerance.json"

echo "=== Scenario 4: Process Spawn Stress ==="
# Generate 10K unique PUT requests
for i in $(seq 1 10000); do
    echo "$GATEWAY/store/key-$i"
done | oha -c 50 -z 0s --json \
    -m PUT -d '{"value":"test"}' -T application/json \
    --read-stdin > "$RESULTS_DIR/spawn_stress.json" 2>/dev/null || \
oha -c 50 -n 10000 --json \
    -m PUT -d '{"value":"test"}' -T application/json \
    "$GATEWAY/store/key-{seq}" > "$RESULTS_DIR/spawn_stress.json" 2>/dev/null || true

echo "=== Scenario 5: Cross-Node Messaging ==="
oha -c 100 -z 30s --json \
    -m POST -d '{"n":10}' -T application/json \
    "$GATEWAY/compute" > "$RESULTS_DIR/cross_node.json" 2>/dev/null || true

echo "All scenarios complete. Results in $RESULTS_DIR/"
```

**Step 3: Write run.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$(dirname "$SCRIPT_DIR")"
REPO_DIR="$(dirname "$BENCH_DIR")"
RESULTS_BASE="$BENCH_DIR/results"

STACK="${1:?Usage: run.sh <rebar|elixir|go|actix|all>}"

run_stack() {
    local stack="$1"
    local compose_file="$BENCH_DIR/docker-compose.${stack}.yml"
    local results_dir="$RESULTS_BASE/$stack"

    if [ ! -f "$compose_file" ]; then
        echo "ERROR: $compose_file not found"
        return 1
    fi

    echo "======================================"
    echo "  Running benchmark: $stack"
    echo "======================================"

    mkdir -p "$results_dir"

    echo "Starting $stack stack..."
    docker compose -f "$compose_file" up -d --build

    echo "Waiting for services to be healthy..."
    sleep 5  # Give containers time to start

    # Wait for gateway health
    local retries=30
    while [ $retries -gt 0 ]; do
        if docker compose -f "$compose_file" exec -T bench \
            sh -c "wget -q -O /dev/null http://gateway:8080/health 2>/dev/null" 2>/dev/null; then
            echo "Gateway is healthy"
            break
        fi
        retries=$((retries - 1))
        sleep 2
    done

    if [ $retries -eq 0 ]; then
        echo "WARNING: Gateway health check failed, running benchmarks anyway"
    fi

    echo "Running scenarios..."
    docker compose -f "$compose_file" exec -T bench \
        sh -c "GATEWAY_URL=http://gateway:8080 RESULTS_DIR=/tmp/results /scenarios.sh" || true

    # Copy results out
    docker compose -f "$compose_file" cp bench:/tmp/results/. "$results_dir/" 2>/dev/null || true

    echo "Stopping $stack stack..."
    docker compose -f "$compose_file" down

    echo "Results saved to $results_dir/"
    echo ""
}

if [ "$STACK" = "all" ]; then
    for s in rebar elixir go actix; do
        run_stack "$s" || echo "WARNING: $s benchmark failed"
    done
else
    run_stack "$STACK"
fi

echo "======================================"
echo "  Generating report..."
echo "======================================"

if command -v python3 &> /dev/null; then
    python3 "$SCRIPT_DIR/report.py" "$RESULTS_BASE"
else
    echo "Python3 not found, skipping report generation"
fi
```

**Step 4: Commit**

```bash
chmod +x bench/harness/*.sh
git add bench/harness/ && git commit -m "bench: add benchmark runner scripts"
```

---

### Task 11: Create report generator

**Files:**
- Create: `bench/harness/report.py`

**Step 1: Write report generator**

```python
#!/usr/bin/env python3
"""Parse oha JSON output and generate comparison tables."""

import json
import os
import sys
from pathlib import Path


def load_result(path):
    """Load an oha JSON result file."""
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def extract_metrics(data):
    """Extract key metrics from oha JSON output."""
    if not data:
        return {"rps": "N/A", "p50": "N/A", "p95": "N/A", "p99": "N/A", "errors": "N/A"}

    summary = data.get("summary", {})
    latency = data.get("latencyPercentiles", data.get("latency_percentiles", {}))

    rps = summary.get("requestsPerSec", summary.get("requests_per_sec", 0))

    # oha formats vary by version; handle both
    p50 = "N/A"
    p95 = "N/A"
    p99 = "N/A"

    if isinstance(latency, list):
        for item in latency:
            p = item.get("percentile", item.get("p", 0))
            val = item.get("latency", item.get("value", 0))
            if abs(p - 0.5) < 0.01:
                p50 = f"{val*1000:.2f}ms" if val < 1 else f"{val:.2f}s"
            elif abs(p - 0.95) < 0.01:
                p95 = f"{val*1000:.2f}ms" if val < 1 else f"{val:.2f}s"
            elif abs(p - 0.99) < 0.01:
                p99 = f"{val*1000:.2f}ms" if val < 1 else f"{val:.2f}s"
    elif isinstance(latency, dict):
        p50 = latency.get("p50", "N/A")
        p95 = latency.get("p95", "N/A")
        p99 = latency.get("p99", "N/A")

    errors = summary.get("errorCount", summary.get("status_code_distribution", {}).get("error", 0))

    return {
        "rps": f"{rps:.0f}" if isinstance(rps, (int, float)) else str(rps),
        "p50": p50,
        "p95": p95,
        "p99": p99,
        "errors": str(errors),
    }


def generate_report(results_dir):
    """Generate markdown comparison report."""
    stacks = ["rebar", "elixir", "go", "actix"]
    scenarios = {
        "throughput_c100": "Throughput (c=100)",
        "latency": "Latency Profile",
        "spawn_stress": "Spawn 10K Keys",
        "cross_node": "Cross-Node Messaging",
    }

    lines = []
    lines.append("# Benchmark Results\n")
    lines.append(f"Generated from results in `{results_dir}`\n")

    # Throughput comparison
    lines.append("## Throughput (requests/sec)\n")
    lines.append("| Concurrency | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|" + "---|" * (len(stacks) + 1))

    for c in [1, 10, 50, 100, 500, 1000]:
        row = [f"c={c}"]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, f"throughput_c{c}.json"))
            metrics = extract_metrics(data)
            row.append(metrics["rps"])
        lines.append("| " + " | ".join(row) + " |")

    # Latency comparison
    lines.append("\n## Latency (c=100, 30s)\n")
    lines.append("| Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|" + "---|" * (len(stacks) + 1))

    for metric in ["p50", "p95", "p99"]:
        row = [metric.upper()]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, "latency.json"))
            metrics = extract_metrics(data)
            row.append(metrics[metric])
        lines.append("| " + " | ".join(row) + " |")

    # Summary table
    lines.append("\n## Summary\n")
    lines.append("| Scenario | Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|" + "---|" * (len(stacks) + 2))

    for scenario_key, scenario_name in scenarios.items():
        row = [scenario_name, "req/s"]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, f"{scenario_key}.json"))
            metrics = extract_metrics(data)
            row.append(metrics["rps"])
        lines.append("| " + " | ".join(row) + " |")

    report = "\n".join(lines)

    # Write to file
    report_path = os.path.join(results_dir, "report.md")
    with open(report_path, "w") as f:
        f.write(report)

    # Also write CSV
    csv_path = os.path.join(results_dir, "report.csv")
    with open(csv_path, "w") as f:
        f.write("scenario,metric," + ",".join(stacks) + "\n")
        for c in [1, 10, 50, 100, 500, 1000]:
            row = [f"throughput_c{c}", "rps"]
            for stack in stacks:
                data = load_result(os.path.join(results_dir, stack, f"throughput_c{c}.json"))
                metrics = extract_metrics(data)
                row.append(metrics["rps"])
            f.write(",".join(row) + "\n")

    print(report)
    print(f"\nReport written to {report_path}")
    print(f"CSV written to {csv_path}")


if __name__ == "__main__":
    results_dir = sys.argv[1] if len(sys.argv) > 1 else "results"
    generate_report(results_dir)
```

**Step 2: Commit**

```bash
git add bench/harness/report.py && git commit -m "bench: add report generator script"
```

---

## Phase 6: Build, Test, Run

### Task 12: Build and smoke-test Rebar stack locally

**Step 1: Build Rebar services locally**

```bash
cd bench/rebar && cargo build --release
```

**Step 2: Smoke-test locally** (3 terminals)

```bash
# Terminal 1
RUST_LOG=info ./target/release/bench-compute

# Terminal 2
RUST_LOG=info ./target/release/bench-store

# Terminal 3
COMPUTE_HOST=127.0.0.1 STORE_HOST=127.0.0.1 RUST_LOG=info ./target/release/bench-gateway
```

**Step 3: Test endpoints**

```bash
curl http://localhost:8080/health
curl -X POST http://localhost:8080/compute -H 'Content-Type: application/json' -d '{"n":35}'
curl -X PUT http://localhost:8080/store/test -H 'Content-Type: application/json' -d '{"value":"hello"}'
curl http://localhost:8080/store/test
```

Expected:
- `/health` → 200
- `/compute` → `{"result":9227465}`
- `PUT /store/test` → 200
- `GET /store/test` → `{"key":"test","value":"hello"}`

**Step 4: Commit if any fixes needed**

---

### Task 13: Docker build and smoke-test all stacks

**Step 1: Build and test Rebar Docker stack**

```bash
cd /home/alexandernicholson/.pxycrab/workspace/rebar
docker compose -f bench/docker-compose.rebar.yml up -d --build
# Wait for healthy
sleep 10
curl http://localhost:8080/health
curl -X POST http://localhost:8080/compute -H 'Content-Type: application/json' -d '{"n":35}'
docker compose -f bench/docker-compose.rebar.yml down
```

**Step 2: Repeat for each stack** (elixir, go, actix)

Fix any build or runtime issues found.

**Step 3: Commit all fixes**

```bash
git add -A && git commit -m "bench: fix Docker build and runtime issues"
```

---

### Task 14: Run full benchmark suite

**Step 1: Run all benchmarks**

```bash
cd /home/alexandernicholson/.pxycrab/workspace/rebar
bash bench/harness/run.sh all
```

**Step 2: Review results**

```bash
cat bench/results/report.md
```

**Step 3: Commit results**

```bash
git add bench/results/ && git commit -m "bench: add initial benchmark results"
```

---

## Task Summary

| Task | Component | Est. Tests |
|------|-----------|-----------|
| 1 | Install Docker and oha | 0 (system) |
| 2 | Scaffold Rebar bench workspace | build check |
| 3 | Rebar Compute service | smoke test |
| 4 | Rebar Store service | smoke test |
| 5 | Rebar Gateway service | smoke test |
| 6 | Rebar Dockerfile + Compose | docker build |
| 7 | Elixir/Phoenix stack | docker build |
| 8 | Go stack | docker build |
| 9 | Actix stack | docker build |
| 10 | Benchmark runner scripts | manual run |
| 11 | Report generator | manual run |
| 12 | Local smoke test | curl tests |
| 13 | Docker smoke test all stacks | curl tests |
| 14 | Full benchmark run | all scenarios |

## Execution

Plan complete and saved to `docs/plans/2026-03-03-benchmark-implementation.md`.

Two execution options:

**1. Subagent-Driven (this session)** — Dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** — Open new session with executing-plans skill, batch execution with checkpoints

Which approach?
