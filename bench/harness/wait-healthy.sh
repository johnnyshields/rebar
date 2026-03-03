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
