#!/usr/bin/env python3
"""Parse oha JSON output and generate comparison tables."""

import json
import os
import sys


def load_result(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def fmt_ms(seconds):
    """Format seconds as milliseconds string."""
    if seconds is None:
        return "N/A"
    ms = seconds * 1000
    if ms < 1:
        return f"{ms:.3f}ms"
    elif ms < 100:
        return f"{ms:.2f}ms"
    else:
        return f"{ms:.0f}ms"


def extract_metrics(data):
    if not data:
        return {"rps": "N/A", "p50": "N/A", "p95": "N/A", "p99": "N/A", "p999": "N/A"}

    rps = data.get("summary", {}).get("requestsPerSec", 0)

    # oha 1.14 uses a flat dict: {"p50": float, "p95": float, ...}
    percentiles = data.get("latencyPercentiles", {})

    return {
        "rps": f"{rps:,.0f}" if isinstance(rps, (int, float)) and rps > 0 else "N/A",
        "p50": fmt_ms(percentiles.get("p50")),
        "p95": fmt_ms(percentiles.get("p95")),
        "p99": fmt_ms(percentiles.get("p99")),
        "p999": fmt_ms(percentiles.get("p99.9")),
    }


def generate_report(results_dir):
    stacks = ["rebar", "actix", "go", "elixir"]
    lines = ["# Rebar Benchmark Results\n"]
    lines.append("HTTP microservices mesh: Gateway -> Compute/Store (3 containers per stack)")
    lines.append("Each container: 2 CPU cores, 512MB RAM\n")

    # Throughput table
    lines.append("## Throughput (requests/sec)\n")
    lines.append("| Concurrency | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|---" + "|---" * len(stacks) + "|")

    for c in [1, 10, 50, 100, 500, 1000]:
        row = [f"c={c}"]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, f"throughput_c{c}.json"))
            row.append(extract_metrics(data)["rps"])
        lines.append("| " + " | ".join(row) + " |")

    # Latency table (from latency.json: c=100, n=30, 30s)
    lines.append("\n## Latency Profile (c=100, POST /compute n=30, 30s)\n")
    lines.append("| Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|---" + "|---" * len(stacks) + "|")

    for metric, label in [("rps", "req/s"), ("p50", "P50"), ("p95", "P95"), ("p99", "P99"), ("p999", "P99.9")]:
        row = [label]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, "latency.json"))
            row.append(extract_metrics(data)[metric])
        lines.append("| " + " | ".join(row) + " |")

    # Cross-node table
    lines.append("\n## Cross-Node Messaging (c=100, POST /compute n=10, 30s)\n")
    lines.append("| Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|---" + "|---" * len(stacks) + "|")

    for metric, label in [("rps", "req/s"), ("p50", "P50"), ("p99", "P99")]:
        row = [label]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, "cross_node.json"))
            row.append(extract_metrics(data)[metric])
        lines.append("| " + " | ".join(row) + " |")

    # Spawn stress table
    lines.append("\n## Process Spawn Stress (c=50, PUT /store/key, 10k requests)\n")
    lines.append("| Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|---" + "|---" * len(stacks) + "|")

    for metric, label in [("rps", "req/s"), ("p50", "P50"), ("p99", "P99")]:
        row = [label]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, "spawn_stress.json"))
            row.append(extract_metrics(data)[metric])
        lines.append("| " + " | ".join(row) + " |")

    report = "\n".join(lines) + "\n"
    report_path = os.path.join(results_dir, "report.md")
    with open(report_path, "w") as f:
        f.write(report)

    # CSV
    csv_path = os.path.join(results_dir, "report.csv")
    with open(csv_path, "w") as f:
        f.write("scenario,metric," + ",".join(stacks) + "\n")
        for c in [1, 10, 50, 100, 500, 1000]:
            row = [f"throughput_c{c}", "rps"]
            for stack in stacks:
                data = load_result(os.path.join(results_dir, stack, f"throughput_c{c}.json"))
                row.append(extract_metrics(data)["rps"])
            f.write(",".join(row) + "\n")
        for scenario in ["latency", "cross_node", "spawn_stress"]:
            for metric in ["rps", "p50", "p95", "p99"]:
                row = [scenario, metric]
                for stack in stacks:
                    data = load_result(os.path.join(results_dir, stack, f"{scenario}.json"))
                    row.append(extract_metrics(data).get(metric, "N/A"))
                f.write(",".join(row) + "\n")

    print(report)
    print(f"Report: {report_path}")
    print(f"CSV: {csv_path}")


if __name__ == "__main__":
    results_dir = sys.argv[1] if len(sys.argv) > 1 else "results"
    generate_report(results_dir)
