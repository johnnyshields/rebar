# Rebar Benchmark: HTTP Microservices Mesh

## Overview

A benchmark suite comparing Rebar against Elixir/Phoenix, Go, and Actix by building identical HTTP microservice meshes with 3 services per stack, orchestrated via Docker Compose.

Each implementation uses framework-native patterns (idiomatic concurrency, discovery, fault tolerance) while exposing the same HTTP endpoints with identical business logic. A shared load generator (oha) runs the same scenarios against each stack.

## Service Architecture

Three services form the mesh, each running as a separate Docker container (separate node):

### Gateway (port 8080)

HTTP ingress — accepts external requests from the load generator. Routes to Compute and Store via the framework's native inter-service transport. Discovers backend services via the framework's native discovery mechanism.

Endpoints:
- `POST /compute` — accepts `{"n": <int>}`, forwards to Compute, returns `{"result": <int>}`
- `GET /store/:key` — forwards to Store, returns `{"key": "...", "value": "..."}` or 404
- `PUT /store/:key` — accepts `{"value": "..."}`, forwards to Store, returns 200
- `GET /health` — returns 200 if all backend services are reachable
- `GET /metrics` — Prometheus-format metrics

### Compute (port 8081, internal only)

CPU-bound work: receives a number, computes fibonacci(n) iteratively (no recursion, u64 arithmetic). Supervised worker pool — each request spawns a child process/goroutine/actor under the framework's supervision equivalent.

Crash injection: `n < 0` or `n > 92` (overflow) panics the worker; the supervisor restarts it.

### Store (port 8082, internal only)

Process-per-key key-value store. Each key gets its own actor/process/goroutine, supervised. Keys are registered in the framework's native registry (global registry for Rebar, `:global` for Elixir, `sync.Map` for Go, actix Registry for Actix).

Values are UTF-8 strings. GET on missing key returns 404. PUT upserts.

### Request Flow

```
Client -> Gateway:8080/compute {"n": 35}
  Gateway discovers "compute" service via registry/discovery
  Gateway sends request to Compute node via framework transport
  Compute supervisor spawns worker, computes fib(35) = 9227465
  Result returns via framework transport to Gateway
  Gateway responds: {"result": 9227465}
```

## Docker Compose Topology

5 containers per stack:

```yaml
services:
  gateway:      # Node 1: HTTP :8080, framework transport :9000
  compute:      # Node 2: HTTP :8081, framework transport :9001
  store:        # Node 3: HTTP :8082, framework transport :9002
  bench:        # oha load generator, runs scenario scripts
  prometheus:   # Metrics collection for resource monitoring
```

Networking:
- All services on a shared Docker network (`rebar-mesh`, `elixir-mesh`, etc.)
- Gateway is the only externally-exposed HTTP port (8080)
- Seed-node discovery: Compute and Store join by contacting Gateway's transport port
- Each service exposes `/metrics` for Prometheus scraping

Resource constraints (identical for all stacks):
- Each service container: 2 CPU cores, 512MB RAM (`deploy.resources.limits`)

Restart policy: `restart: unless-stopped` (for fault tolerance scenario)

### Compose Files

- `docker-compose.rebar.yml`
- `docker-compose.elixir.yml`
- `docker-compose.go.yml`
- `docker-compose.actix.yml`

Each file is self-contained with its own network, volumes, and Prometheus config.

## Competitor Implementations

### Rebar (Rust)

- HTTP: `axum` server on each service
- Transport: Rebar TCP transport with Frame codec
- Discovery: SWIM gossip between nodes
- Registry: CRDT OR-Set global registry for service names
- Supervision: OneForOne supervisor with Permanent restart for workers
- Process model: Rebar processes (tokio tasks + mailboxes)

### Elixir/Phoenix

- HTTP: Phoenix endpoint with Plug
- Transport: Native distributed Erlang (EPMD, cookie-based clustering)
- Discovery: `libcluster` with gossip strategy
- Registry: `:global` module
- Supervision: `DynamicSupervisor` with `:one_for_one` strategy
- Process model: BEAM processes (lightweight, preemptive)

### Go

- HTTP: `net/http` standard library
- Transport: gRPC with protobuf between services
- Discovery: `hashicorp/memberlist` (SWIM implementation)
- Registry: `sync.Map` with service name → address mapping
- Supervision: `errgroup` with panic recovery middleware
- Process model: goroutines with channel-based mailboxes

### Actix (Rust)

- HTTP: `actix-web` server
- Transport: TCP with custom actor message framing
- Discovery: Seed-node TCP ping (simple custom discovery)
- Registry: `actix::Registry` for typed actors
- Supervision: `actix::Supervisor` with restart on failure
- Process model: Actix actors with `Addr<A>` handles

### Shared Constraints

All implementations:
- Same `fib(n)` algorithm: iterative, u64, no memoization
- Same KV semantics: GET returns 404 if missing, PUT upserts, values are strings
- Same Docker resource limits: 2 CPU cores, 512MB RAM per service
- Same health check: `/health` returns 200 when backends reachable
- Same metrics: request count, latency histogram, error count via `/metrics`

## Benchmark Scenarios

### Scenario 1: Throughput Ramp

Purpose: measure maximum sustainable throughput at increasing concurrency.

- Concurrency levels: 1, 10, 50, 100, 500, 1000
- Request: `POST /compute {"n": 20}` (fast, ~microseconds CPU)
- Duration: 15 seconds per level
- Metrics: requests/sec, mean latency
- Features exercised: spawn, mailbox, supervisor worker pool

### Scenario 2: Latency Profile

Purpose: measure tail latency under sustained load.

- Concurrency: 100 connections
- Request: `POST /compute {"n": 30}` (moderate, ~ms CPU)
- Duration: 30 seconds
- Metrics: p50, p95, p99, p99.9 latency
- Features exercised: cross-node transport, message routing, registry lookup

### Scenario 3: Fault Tolerance

Purpose: measure recovery time when a service crashes.

- Concurrency: 100 connections, sustained
- At t=10s: `docker kill <compute container>`
- Docker restart policy brings it back
- Duration: 30 seconds total
- Metrics: error rate during outage, time-to-recovery (first successful request after restart), total requests lost
- Features exercised: SWIM failure detection, supervisor restart, reconnection backoff

### Scenario 4: Process Spawn Stress

Purpose: measure actor/process creation throughput.

- Request: `PUT /store/{key}` with 10,000 unique sequential keys
- Concurrency: 50
- Each key spawns a new actor/process
- Metrics: total time to create all 10K processes, memory usage, p99 response time
- Features exercised: process table, supervisor dynamic children, registry registration

### Scenario 5: Cross-Node Messaging

Purpose: measure inter-service communication overhead.

- Concurrency: 100 connections
- Requests: alternating `POST /compute {"n": 10}` and `GET /store/key-{rand}`
- Every request traverses Gateway → Backend → Gateway (2 cross-node hops)
- Duration: 30 seconds
- Metrics: latency overhead vs single-node baseline, throughput
- Features exercised: TCP transport, frame codec, connection manager

## Metrics Collection

### Per-Scenario (from oha)

- Requests/sec (mean, min, max)
- Latency: p50, p95, p99, p99.9 (in ms)
- Error count and rate
- Total requests completed

### Per-Container (from Prometheus + Docker stats)

- CPU usage (percentage of limit)
- Memory usage (RSS, percentage of limit)
- Network I/O (bytes in/out)

### Framework-Specific

- Rebar: process count, mailbox depth
- Elixir: process count, reductions, scheduler utilization
- Go: goroutine count, GC pause time
- Actix: actor count, mailbox queue length

## Benchmark Harness

### Directory Structure

```
bench/
├── docker-compose.rebar.yml
├── docker-compose.elixir.yml
├── docker-compose.go.yml
├── docker-compose.actix.yml
├── prometheus.yml
├── harness/
│   ├── run.sh              # Main entry: ./run.sh rebar | ./run.sh all
│   ├── scenarios.sh         # Defines the 5 benchmark scenarios
│   ├── wait-healthy.sh      # Polls /health until all services ready
│   └── report.py           # Parses oha JSON → comparison tables + CSV
├── rebar/
│   ├── Dockerfile
│   ├── gateway/  (Rust binary)
│   ├── compute/  (Rust binary)
│   └── store/    (Rust binary)
├── elixir/
│   ├── Dockerfile
│   ├── gateway/  (Phoenix app)
│   ├── compute/  (Phoenix app)
│   └── store/    (Phoenix app)
├── go/
│   ├── Dockerfile
│   ├── gateway/  (Go module)
│   ├── compute/  (Go module)
│   └── store/    (Go module)
├── actix/
│   ├── Dockerfile
│   ├── gateway/  (Rust binary)
│   ├── compute/  (Rust binary)
│   └── store/    (Rust binary)
└── results/
    ├── rebar/     (JSON output per scenario)
    ├── elixir/
    ├── go/
    ├── actix/
    └── report.md  (generated comparison)
```

### Run Script (`harness/run.sh`)

1. Accept stack argument: `./run.sh rebar` or `./run.sh all`
2. `docker compose -f docker-compose.{stack}.yml up -d`
3. Wait for health checks on all 3 services (poll `/health`, timeout 30s)
4. Run each of the 5 scenarios sequentially via oha
5. Capture oha JSON output to `results/{stack}/{scenario}.json`
6. Capture Docker stats snapshot to `results/{stack}/resources.json`
7. `docker compose -f docker-compose.{stack}.yml down`
8. Repeat for each stack if `all`
9. Run `report.py` to generate comparison tables

### Load Generator

oha (Rust HTTP load generator):
- Outputs JSON with latency percentiles
- Supports concurrency ramp, duration-based runs
- Example: `oha -c 100 -z 30s --json -m POST -d '{"n":30}' -T application/json http://gateway:8080/compute`

### Report Output

Markdown table format:

```
| Scenario          | Metric    | Rebar    | Elixir   | Go       | Actix    |
|-------------------|-----------|----------|----------|----------|----------|
| Throughput (c=100)| req/s     | 45,000   | 32,000   | 50,000   | 48,000   |
| Latency (c=100)   | p99 (ms)  | 4.2      | 8.1      | 3.8      | 3.9      |
| Fault Recovery    | TTR (ms)  | 1,200    | 800      | 2,500    | 1,500    |
| Spawn 10K         | time (ms) | 150      | 45       | 80       | 200      |
| Cross-Node        | overhead  | +1.2ms   | +0.8ms   | +2.1ms   | +1.5ms   |
```

Also outputs CSV for further analysis.

## Fibonacci Algorithm (shared)

All implementations use this exact algorithm:

```
fn fib(n: u64) -> u64 {
    if n <= 1 { return n; }
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 2..=n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}
```

Wrapping add prevents overflow panics. `n > 92` triggers crash injection (value check before compute).
