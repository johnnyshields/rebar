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

    echo "Waiting for gateway health..."
    local retries=60
    while [ $retries -gt 0 ]; do
        if curl -sf http://localhost:8080/health > /dev/null 2>&1; then
            echo "Gateway is healthy"
            break
        fi
        retries=$((retries - 1))
        sleep 2
    done

    if [ $retries -eq 0 ]; then
        echo "WARNING: Gateway health check timed out"
        docker compose -f "$compose_file" logs
    fi

    echo "Running scenarios..."

    # Scenario 1: Throughput Ramp
    for C in 1 10 50 100 500 1000; do
        echo "  Throughput c=$C"
        oha -c "$C" -z 15s --output-format json \
            -m POST -d '{"n":20}' -T application/json \
            http://localhost:8080/compute > "$results_dir/throughput_c${C}.json" 2>/dev/null || true
    done

    # Scenario 2: Latency Profile
    echo "  Latency profile (c=100, 30s)"
    oha -c 100 -z 30s --output-format json \
        -m POST -d '{"n":30}' -T application/json \
        http://localhost:8080/compute > "$results_dir/latency.json" 2>/dev/null || true

    # Scenario 4: Spawn Stress
    echo "  Spawn stress (10k keys)"
    oha -c 50 -n 10000 --output-format json \
        -m PUT -d '{"value":"test"}' -T application/json \
        http://localhost:8080/store/key-spawn > "$results_dir/spawn_stress.json" 2>/dev/null || true

    # Scenario 5: Cross-Node
    echo "  Cross-node messaging (c=100, 30s)"
    oha -c 100 -z 30s --output-format json \
        -m POST -d '{"n":10}' -T application/json \
        http://localhost:8080/compute > "$results_dir/cross_node.json" 2>/dev/null || true

    echo "Stopping $stack stack..."
    docker compose -f "$compose_file" down

    echo "Results saved to $results_dir/"
    echo ""
}

if [ "$STACK" = "all" ]; then
    for s in rebar go actix elixir; do
        run_stack "$s" || echo "WARNING: $s benchmark failed, continuing..."
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
