#!/usr/bin/env bash
set -euo pipefail

GATEWAY="${GATEWAY_URL:-http://gateway:8080}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/results}"

mkdir -p "$RESULTS_DIR"

echo "=== Scenario 1: Throughput Ramp ==="
for C in 1 10 50 100 500 1000; do
    echo "  Concurrency: $C"
    oha -c "$C" -z 15s --output-format json \
        -m POST -d '{"n":20}' -T application/json \
        "$GATEWAY/compute" > "$RESULTS_DIR/throughput_c${C}.json" 2>/dev/null || true
done

echo "=== Scenario 2: Latency Profile ==="
oha -c 100 -z 30s --output-format json \
    -m POST -d '{"n":30}' -T application/json \
    "$GATEWAY/compute" > "$RESULTS_DIR/latency.json" 2>/dev/null || true

echo "=== Scenario 3: Fault Tolerance ==="
echo '{"note": "run manually with docker kill"}' > "$RESULTS_DIR/fault_tolerance.json"

echo "=== Scenario 4: Process Spawn Stress ==="
oha -c 50 -n 10000 --output-format json \
    -m PUT -d '{"value":"test"}' -T application/json \
    "$GATEWAY/store/key-spawn" > "$RESULTS_DIR/spawn_stress.json" 2>/dev/null || true

echo "=== Scenario 5: Cross-Node Messaging ==="
oha -c 100 -z 30s --output-format json \
    -m POST -d '{"n":10}' -T application/json \
    "$GATEWAY/compute" > "$RESULTS_DIR/cross_node.json" 2>/dev/null || true

echo "All scenarios complete. Results in $RESULTS_DIR/"
