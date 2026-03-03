#!/usr/bin/env python3
"""Parse oha JSON output and generate comparison tables."""

import json
import os
import sys
from pathlib import Path


def load_result(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def extract_metrics(data):
    if not data:
        return {"rps": "N/A", "p50": "N/A", "p95": "N/A", "p99": "N/A"}

    # oha JSON format
    rps = data.get("summary", {}).get("requestsPerSec", 0)
    if not rps:
        rps = data.get("summary", {}).get("requests_per_sec", 0)

    percentiles = data.get("latencyPercentiles", data.get("latency_percentiles", []))

    p50 = p95 = p99 = "N/A"
    if isinstance(percentiles, list):
        for item in percentiles:
            p = item.get("percentile", item.get("p", 0))
            val = item.get("latency", item.get("value", 0))
            if isinstance(val, (int, float)):
                formatted = f"{val*1000:.2f}ms" if val < 1 else f"{val:.2f}s"
            else:
                formatted = str(val)
            if abs(p - 0.5) < 0.01: p50 = formatted
            elif abs(p - 0.95) < 0.01: p95 = formatted
            elif abs(p - 0.99) < 0.01: p99 = formatted

    return {
        "rps": f"{rps:.0f}" if isinstance(rps, (int, float)) and rps > 0 else "N/A",
        "p50": p50, "p95": p95, "p99": p99,
    }


def generate_report(results_dir):
    stacks = ["rebar", "go", "actix", "elixir"]
    lines = ["# Benchmark Results\n"]

    # Throughput table
    lines.append("## Throughput (requests/sec)\n")
    lines.append("| Concurrency | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|" + "---|" * (len(stacks) + 1))

    for c in [1, 10, 50, 100, 500, 1000]:
        row = [f"c={c}"]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, f"throughput_c{c}.json"))
            row.append(extract_metrics(data)["rps"])
        lines.append("| " + " | ".join(row) + " |")

    # Latency table
    lines.append("\n## Latency (c=100, 30s)\n")
    lines.append("| Metric | " + " | ".join(s.title() for s in stacks) + " |")
    lines.append("|" + "---|" * (len(stacks) + 1))

    for metric in ["p50", "p95", "p99"]:
        row = [metric.upper()]
        for stack in stacks:
            data = load_result(os.path.join(results_dir, stack, "latency.json"))
            row.append(extract_metrics(data)[metric])
        lines.append("| " + " | ".join(row) + " |")

    report = "\n".join(lines)
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

    print(report)
    print(f"\nReport: {report_path}")
    print(f"CSV: {csv_path}")


if __name__ == "__main__":
    results_dir = sys.argv[1] if len(sys.argv) > 1 else "results"
    generate_report(results_dir)
