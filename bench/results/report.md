# Rebar Benchmark Results

HTTP microservices mesh: Gateway -> Compute/Store (3 containers per stack)
Each container: 2 CPU cores, 512MB RAM

## Throughput (requests/sec)

| Concurrency | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| c=1 | 2,649 | 2,625 | 2,464 | 1,112 |
| c=10 | 11,583 | 9,230 | 2,216 | 2,734 |
| c=50 | 17,048 | 11,812 | 721 | 3,560 |
| c=100 | 18,333 | 13,175 | 642 | 3,410 |
| c=500 | 15,645 | 11,478 | 604 | 3,330 |
| c=1000 | 13,396 | 11,795 | 2,138 | 3,320 |

## Latency Profile (c=100, POST /compute n=30, 30s)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 18,467 | 10,794 | 993 | 3,405 |
| P50 | 4.89ms | 9.02ms | 54.98ms | 28.78ms |
| P95 | 10.18ms | 14.45ms | 332ms | 40.05ms |
| P99 | 13.31ms | 17.71ms | 866ms | 47.28ms |
| P99.9 | 17.55ms | 22.94ms | 1300ms | 59.43ms |

## Cross-Node Messaging (c=100, POST /compute n=10, 30s)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 18,082 | 10,865 | 1,262 | 3,420 |
| P50 | 5.00ms | 8.95ms | 20.44ms | 28.61ms |
| P99 | 13.55ms | 18.12ms | 1103ms | 46.83ms |

## Process Spawn Stress (c=50, PUT /store/key, 10k requests)

| Metric | Rebar | Actix | Go | Elixir |
|---|---|---|---|---|
| req/s | 18,055 | 12,664 | 5,025 | 3,735 |
| P50 | 2.37ms | 3.61ms | 8.30ms | 13.10ms |
| P99 | 7.46ms | 9.89ms | 31.92ms | 25.00ms |
