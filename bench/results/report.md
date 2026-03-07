# Rebar Benchmark Results

HTTP microservices mesh: Gateway -> Compute/Store (3 containers per stack)
Each container: 2 CPU cores, 512MB RAM

## Throughput (requests/sec)

| Concurrency | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| c=1 | 1,012 | 2,625 | 2,464 | 1,112 |
| c=10 | 14,506 | 9,230 | 2,216 | 2,734 |
| c=50 | 25,785 | 11,812 | 721 | 3,560 |
| c=100 | 25,856 | 13,175 | 642 | 3,410 |
| c=500 | 17,616 | 11,478 | 604 | 3,330 |
| c=1000 | 14,323 | 11,795 | 2,138 | 3,320 |

## Latency Profile (c=100, POST /compute n=30, 30s)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 16,890 | 10,794 | 993 | 3,405 |
| P50 | 5.34ms | 9.02ms | 54.98ms | 28.78ms |
| P95 | 10.65ms | 14.45ms | 332ms | 40.05ms |
| P99 | 14.11ms | 17.71ms | 866ms | 47.28ms |
| P99.9 | 26.61ms | 22.94ms | 1300ms | 59.43ms |

## Cross-Node Messaging (c=100, POST /compute n=10, 30s)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 16,302 | 10,865 | 1,262 | 3,420 |
| P50 | 5.68ms | 8.95ms | 20.44ms | 28.61ms |
| P99 | 14.13ms | 18.12ms | 1103ms | 46.83ms |

## Process Spawn Stress (c=50, PUT /store/key, 10k requests)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 14,118 | 12,664 | 5,025 | 3,735 |
| P50 | 3.11ms | 3.61ms | 8.30ms | 13.10ms |
| P99 | 10.63ms | 9.89ms | 31.92ms | 25.00ms |
