#!/bin/bash
# Run TLC model checker on Rebar TLA+ specifications
#
# Models are auto-detected by finding MC*.tla files in specs/tla/.
# Each MC wrapper is paired with its cfg file(s) by naming convention:
#   MCRebarRegistry.tla  -> RebarRegistry.cfg
#   MCRebarSupervisor.tla -> RebarSupervisor.cfg, RebarSupervisor-OneForAll.cfg, ...
#
# Usage:
#   ./run_tlc.sh                        # Run all models (default)
#   ./run_tlc.sh RebarSwim              # Run specific model(s)
#   ./run_tlc.sh -f, --force            # Force re-run ignoring .tlc_passed hashes
#   ./run_tlc.sh -k, --hash-check       # Hash check: fail if any hashes missing
#   ./run_tlc.sh -s, --sanity           # Sanity check: run each model for 30s
#   ./run_tlc.sh -x, --fail-fast        # Stop on first failure
#   ./run_tlc.sh -r, --recover          # Resume from most recent checkpoint
#   ./run_tlc.sh --ci                   # CI mode: hash check + sanity checks (30s each)
#   ./run_tlc.sh -d, --download-jar     # Auto-download TLC jar to specs/tla/tmp if missing
#   ./run_tlc.sh --jar ~/tla2tools.jar  # Specify path to tla2tools.jar

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PASSED_FILE="$SCRIPT_DIR/.tlc_passed"
TMP_DIR="$SCRIPT_DIR/tmp"

mkdir -p "$TMP_DIR"

# Auto-detect all model/config pairs.
# Finds MC*.tla files (the spec name is the MC file minus "MC" prefix and ".tla"),
# then finds matching cfg files: RebarFoo.cfg, RebarFoo-*.cfg
detect_runs() {
    local runs=""
    while IFS= read -r -d '' mc_file; do
        local mc_basename=$(basename "$mc_file" .tla)         # MCRebarFoo
        local spec_name=${mc_basename#MC}                      # RebarFoo

        # Find all matching cfg files for this spec
        while IFS= read -r -d '' cfg_file; do
            local cfg_basename=$(basename "$cfg_file")
            local run_id="${spec_name}:${cfg_basename}"
            if [[ -z "$runs" ]]; then
                runs="$run_id"
            else
                runs="$runs,$run_id"
            fi
        done < <(find "$SCRIPT_DIR" -maxdepth 1 \( -name "${spec_name}.cfg" -o -name "${spec_name}-*.cfg" \) -print0 2>/dev/null | sort -z)
    done < <(find "$SCRIPT_DIR" -maxdepth 1 -name "MC*.tla" -print0 2>/dev/null | sort -z)
    echo "$runs"
}

ALL_RUNS=$(detect_runs)

# Parse arguments
FORCE=false
CI_MODE=false
HASH_CHECK_ONLY=false
SANITY_MODE=false
FAIL_FAST=false
DOWNLOAD_JAR=false
RECOVER_MODE=false
TLC_JAR=""
MODEL_ARGS=()

show_help() {
    head -19 "$0" | tail -14
    echo ""
    echo "Detected runs:"
    IFS=',' read -ra RUNS <<< "$ALL_RUNS"
    for run in "${RUNS[@]}"; do
        echo "  $run"
    done
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) show_help ;;
        -f|--force) FORCE=true; shift ;;
        --ci) CI_MODE=true; shift ;;
        -k|--hash-check) HASH_CHECK_ONLY=true; shift ;;
        -s|--sanity) SANITY_MODE=true; shift ;;
        -x|--fail-fast) FAIL_FAST=true; shift ;;
        -r|--recover) RECOVER_MODE=true; shift ;;
        -d|--download-jar) DOWNLOAD_JAR=true; shift ;;
        --jar)
            TLC_JAR="$2"
            shift 2
            ;;
        --jar=*)
            TLC_JAR="${1#*=}"
            shift
            ;;
        -*) echo "Unknown option: $1"; exit 1 ;;
        *) MODEL_ARGS+=("$1"); shift ;;
    esac
done

# Build RUN_LIST from positional arguments.
# If arg matches a spec name (e.g. "RebarSwim"), expand to all its cfg variants.
# If arg is a full run_id (e.g. "RebarSupervisor:RebarSupervisor-OneForAll.cfg"), use directly.
RUN_LIST=""
for arg in "${MODEL_ARGS[@]}"; do
    # Check if it's a run_id (contains ':')
    if [[ "$arg" == *":"* ]]; then
        if [[ -z "$RUN_LIST" ]]; then
            RUN_LIST="$arg"
        else
            RUN_LIST="$RUN_LIST,$arg"
        fi
    else
        # It's a spec name — find all matching runs
        IFS=',' read -ra ALL <<< "$ALL_RUNS"
        for run in "${ALL[@]}"; do
            local_spec="${run%%:*}"
            if [[ "$local_spec" == "$arg" ]]; then
                if [[ -z "$RUN_LIST" ]]; then
                    RUN_LIST="$run"
                else
                    RUN_LIST="$RUN_LIST,$run"
                fi
            fi
        done
    fi
done

# Default to all runs if none specified
if [[ -z "$RUN_LIST" ]]; then
    RUN_LIST="$ALL_RUNS"
fi

# TLC JAR configuration
TLC_VERSION="1.8.0"
TLC_URL="https://github.com/tlaplus/tlaplus/releases/download/v${TLC_VERSION}/tla2tools.jar"
LOCAL_JAR="$TMP_DIR/tla2tools.jar"

# Parse a run_id into SPEC_NAME and CFG_FILE
parse_run() {
    local run_id="$1"
    RUN_SPEC="${run_id%%:*}"
    RUN_CFG="${run_id##*:}"
}

# Compute hash for a run (spec .tla + MC wrapper .tla + cfg)
compute_hash() {
    local spec_name="$1"
    local cfg_file="$2"
    local spec_file="$SCRIPT_DIR/${spec_name}.tla"
    local mc_file="$SCRIPT_DIR/MC${spec_name}.tla"
    local cfg_path="$SCRIPT_DIR/${cfg_file}"
    cat "$spec_file" "$mc_file" "$cfg_path" 2>/dev/null | sha256sum | cut -d' ' -f1
}

# Hash check mode
if [[ "$HASH_CHECK_ONLY" == "true" ]]; then
    MISSING=()
    IFS=',' read -ra RUNS <<< "$RUN_LIST"
    for run in "${RUNS[@]}"; do
        run=$(echo "$run" | xargs)
        parse_run "$run"

        if [[ ! -f "$SCRIPT_DIR/${RUN_SPEC}.tla" ]]; then
            echo "Error: ${RUN_SPEC}.tla not found"
            exit 1
        fi

        HASH=$(compute_hash "$RUN_SPEC" "$RUN_CFG")
        if [[ ! -f "$PASSED_FILE" ]] || ! grep -q "$HASH" "$PASSED_FILE"; then
            MISSING+=("$run")
            echo "[$run] MISSING - hash not found: ${HASH:0:16}..."
        else
            echo "[$run] OK - hash found: ${HASH:0:16}..."
        fi
    done

    if [[ ${#MISSING[@]} -gt 0 ]]; then
        echo ""
        echo "ERROR: ${#MISSING[@]} run(s) have not been verified: ${MISSING[*]}"
        echo ""
        echo "Run specs/tla/run_tlc.sh locally and commit specs/tla/.tlc_passed after it succeeds"
        exit 1
    fi

    echo ""
    echo "All model hashes verified."
    exit 0
fi

# CI mode: check hashes THEN run sanity checks
if [[ "$CI_MODE" == "true" ]]; then
    echo "=== Phase 1: Verify model hashes ==="
    echo ""
    MISSING=()
    IFS=',' read -ra RUNS <<< "$RUN_LIST"
    for run in "${RUNS[@]}"; do
        run=$(echo "$run" | xargs)
        parse_run "$run"

        if [[ ! -f "$SCRIPT_DIR/${RUN_SPEC}.tla" ]]; then
            echo "Error: ${RUN_SPEC}.tla not found"
            exit 1
        fi

        HASH=$(compute_hash "$RUN_SPEC" "$RUN_CFG")
        if [[ ! -f "$PASSED_FILE" ]] || ! grep -q "$HASH" "$PASSED_FILE"; then
            MISSING+=("$run")
            echo "[$run] MISSING - hash not found: ${HASH:0:16}..."
        else
            echo "[$run] OK - hash found: ${HASH:0:16}..."
        fi
    done

    if [[ ${#MISSING[@]} -gt 0 ]]; then
        echo ""
        echo "ERROR: ${#MISSING[@]} run(s) have not been verified: ${MISSING[*]}"
        echo ""
        echo "Run specs/tla/run_tlc.sh locally and commit specs/tla/.tlc_passed after it succeeds"
        exit 1
    fi

    echo ""
    echo "All model hashes verified."
    echo ""

    # Try to find or download JAR for sanity checks
    if [[ -z "$TLC_JAR" ]] || [[ ! -f "$TLC_JAR" ]]; then
        if [[ -f "$LOCAL_JAR" ]]; then
            TLC_JAR="$LOCAL_JAR"
        elif [[ "$DOWNLOAD_JAR" == "true" ]]; then
            echo "Downloading TLC tools v${TLC_VERSION} for sanity checks..."
            curl -L -o "$LOCAL_JAR" "$TLC_URL"
            TLC_JAR="$LOCAL_JAR"
        fi
    fi

    if [[ -n "$TLC_JAR" ]] && [[ -f "$TLC_JAR" ]]; then
        echo "=== Phase 2: Sanity checks (30s per model) ==="
        echo ""
        SANITY_MODE=true
    else
        echo "Skipping sanity checks (TLC JAR not available)"
        echo "Use --download-jar to enable sanity checks in CI"
        exit 0
    fi
fi

# Recover mode
if [[ "$RECOVER_MODE" == "true" ]]; then
    CHECKPOINT_DIR=$(find "$TMP_DIR" -maxdepth 1 -type d -name "[0-9][0-9]-[0-9][0-9]-[0-9][0-9]-[0-9][0-9]-[0-9][0-9]-[0-9][0-9].*" 2>/dev/null | sort -r | head -1)

    if [[ -z "$CHECKPOINT_DIR" ]]; then
        echo "Error: No checkpoint found in $TMP_DIR"
        exit 1
    fi

    echo "Found checkpoint: $CHECKPOINT_DIR"
    echo ""

    if [[ ${#MODEL_ARGS[@]} -gt 0 ]]; then
        RECOVER_MODEL="${MODEL_ARGS[0]}"
    else
        RECOVER_MODEL=""
        for st_file in "$CHECKPOINT_DIR"/*.st; do
            if [[ -f "$st_file" ]]; then
                st_basename=$(basename "$st_file" .st)
                RECOVER_MODEL="${st_basename#MC}"
                break
            fi
        done

        if [[ -z "$RECOVER_MODEL" ]]; then
            echo "Error: Cannot determine which model to recover."
            echo "Please specify the model name: ./run_tlc.sh -r RebarSwim"
            exit 1
        fi
    fi

    cd "$SCRIPT_DIR"

    echo "Resuming TLC from checkpoint: $CHECKPOINT_DIR"
    echo "Model: $RECOVER_MODEL"
    echo ""

    java -XX:+UseParallelGC \
         -Dtlc2.tool.fp.FPSet.impl=tlc2.tool.fp.OffHeapDiskFPSet \
         -jar "$TLC_JAR" \
         -config "$(ls ${RECOVER_MODEL}.cfg ${RECOVER_MODEL}-*.cfg 2>/dev/null | head -1)" \
         -recover "$CHECKPOINT_DIR" \
         -metadir "$TMP_DIR" \
         -lncheck final \
         -workers auto \
         "MC${RECOVER_MODEL}.tla"

    exit $?
fi

# Resolve TLC JAR
if [[ -z "$TLC_JAR" ]] || [[ ! -f "$TLC_JAR" ]]; then
    if [[ -f "$LOCAL_JAR" ]]; then
        TLC_JAR="$LOCAL_JAR"
    elif command -v tla2tools.jar &> /dev/null; then
        TLC_JAR="$(command -v tla2tools.jar)"
    fi

    if [[ "$DOWNLOAD_JAR" == "true" ]] && { [[ -z "$TLC_JAR" ]] || [[ ! -f "$TLC_JAR" ]]; }; then
        echo "Downloading TLC tools v${TLC_VERSION}..."
        curl -L -o "$LOCAL_JAR" "$TLC_URL"
        echo "Downloaded to $LOCAL_JAR"
        TLC_JAR="$LOCAL_JAR"
    fi

    if [[ -z "$TLC_JAR" ]] || [[ ! -f "$TLC_JAR" ]]; then
        echo "Error: tla2tools.jar not found"
        echo ""
        echo "Options:"
        echo "  1. Download from https://github.com/tlaplus/tlaplus/releases and add to PATH"
        echo "  2. Use --jar /path/to/tla2tools.jar"
        echo "  3. Use --download-jar to auto-download"
        exit 1
    fi
fi

# Track failures
FAILED_RUNS=()

run_sanity() {
    local spec_name="$1"
    local cfg_file="$2"
    local run_id="${spec_name}:${cfg_file}"
    local TIMEOUT=${3:-30}

    local mc_file="MC${spec_name}.tla"
    if [[ ! -f "$SCRIPT_DIR/$mc_file" ]]; then
        echo "Error: $mc_file not found"
        FAILED_RUNS+=("$run_id")
        return 1
    fi

    cd "$SCRIPT_DIR"
    echo "[$run_id] Sanity check (${TIMEOUT}s timeout)..."

    local OUTPUT
    OUTPUT=$(timeout "${TIMEOUT}s" java -XX:+UseParallelGC \
         -Dtlc2.tool.fp.FPSet.impl=tlc2.tool.fp.OffHeapDiskFPSet \
         -jar "$TLC_JAR" \
         -config "$cfg_file" \
         -metadir "$TMP_DIR" \
         -workers auto \
         -lncheck final \
         "$mc_file" 2>&1) || true

    if echo "$OUTPUT" | grep -q "Error:"; then
        echo "[$run_id] FAILED - error found:"
        echo "$OUTPUT" | grep -A5 "Error:"
        FAILED_RUNS+=("$run_id")
        return 1
    elif echo "$OUTPUT" | grep -q "Invariant.*violated"; then
        echo "[$run_id] FAILED - invariant violated:"
        echo "$OUTPUT" | grep -B2 -A10 "Invariant.*violated"
        FAILED_RUNS+=("$run_id")
        return 1
    else
        echo "[$run_id] OK - no errors in ${TIMEOUT}s"
        return 0
    fi
}

run_model() {
    local spec_name="$1"
    local cfg_file="$2"
    local run_id="${spec_name}:${cfg_file}"

    local mc_file="MC${spec_name}.tla"
    if [[ ! -f "$SCRIPT_DIR/$mc_file" ]]; then
        echo "Error: $mc_file not found"
        FAILED_RUNS+=("$run_id")
        [[ "$FAIL_FAST" == "true" ]] && exit 1
        return 1
    fi

    if [[ ! -f "$SCRIPT_DIR/$cfg_file" ]]; then
        echo "Error: $cfg_file not found"
        FAILED_RUNS+=("$run_id")
        [[ "$FAIL_FAST" == "true" ]] && exit 1
        return 1
    fi

    local HASH=$(compute_hash "$spec_name" "$cfg_file")
    echo "[$run_id] Hash: ${HASH:0:16}..."

    if [[ "$FORCE" == "false" ]] && [[ -f "$PASSED_FILE" ]] && grep -q "$HASH" "$PASSED_FILE"; then
        echo "[$run_id] Already passed (use --force to re-run)"
        return 0
    fi

    cd "$SCRIPT_DIR"
    echo "[$run_id] Running TLC model checker..."
    echo ""

    local TLC_OUTPUT
    TLC_OUTPUT=$(java -XX:+UseParallelGC \
         -Dtlc2.tool.fp.FPSet.impl=tlc2.tool.fp.OffHeapDiskFPSet \
         -jar "$TLC_JAR" \
         -config "$cfg_file" \
         -metadir "$TMP_DIR" \
         -lncheck final \
         -cleanup \
         -workers auto \
         "$mc_file" 2>&1 | tee /dev/stderr)
    local TLC_EXIT=${PIPESTATUS[0]}

    if echo "$TLC_OUTPUT" | grep -qE "Error:|Invariant.*violated|violated.*Invariant"; then
        echo "[$run_id] FAILED - error found in output"
        FAILED_RUNS+=("$run_id")
        [[ "$FAIL_FAST" == "true" ]] && exit 1
        return 1
    fi

    if [[ $TLC_EXIT -eq 0 ]]; then
        local STATES_LINE=$(echo "$TLC_OUTPUT" | grep -E "^[0-9]+ states generated")
        local STATES_GENERATED="" STATES_DISTINCT=""
        if [[ -n "$STATES_LINE" ]]; then
            STATES_GENERATED=$(echo "$STATES_LINE" | sed -E 's/^([0-9]+) states generated.*/\1/')
            STATES_DISTINCT=$(echo "$STATES_LINE" | sed -E 's/.*[^0-9]([0-9]+) distinct states found.*/\1/')
        fi

        local DEPTH_LINE=$(echo "$TLC_OUTPUT" | grep -E "^The depth of the complete state graph search is")
        local GRAPH_DEPTH=""
        if [[ -n "$DEPTH_LINE" ]]; then
            GRAPH_DEPTH=$(echo "$DEPTH_LINE" | sed -E 's/.*is ([0-9]+)\..*/\1/')
        fi

        local RECORD="$HASH  $run_id  $(date -Iseconds)"
        [[ -n "$STATES_GENERATED" ]] && [[ -n "$STATES_DISTINCT" ]] && \
            RECORD="$RECORD  states_generated=$STATES_GENERATED  states_distinct=$STATES_DISTINCT"
        [[ -n "$GRAPH_DEPTH" ]] && RECORD="$RECORD  graph_depth=$GRAPH_DEPTH"

        echo "$RECORD" >> "$PASSED_FILE"
        echo "[$run_id] PASSED - hash recorded: ${HASH:0:16}..."
        echo ""
        return 0
    else
        echo "[$run_id] FAILED"
        FAILED_RUNS+=("$run_id")
        [[ "$FAIL_FAST" == "true" ]] && exit 1
        return 1
    fi
}

# Main execution
IFS=',' read -ra RUNS <<< "$RUN_LIST"

if [[ "$SANITY_MODE" == "true" ]]; then
    if [[ "$CI_MODE" != "true" ]]; then
        echo "Running sanity checks (30s per model)..."
        echo ""
    fi
    for run in "${RUNS[@]}"; do
        run=$(echo "$run" | xargs)
        parse_run "$run"
        run_sanity "$RUN_SPEC" "$RUN_CFG" 30
        [[ "$FAIL_FAST" == "true" ]] && [[ ${#FAILED_RUNS[@]} -gt 0 ]] && break
    done
else
    for run in "${RUNS[@]}"; do
        run=$(echo "$run" | xargs)
        parse_run "$run"
        run_model "$RUN_SPEC" "$RUN_CFG"
    done
fi

echo ""

if [[ ${#FAILED_RUNS[@]} -gt 0 ]]; then
    echo "========================================="
    echo "FAILED (${#FAILED_RUNS[@]}):"
    for failed in "${FAILED_RUNS[@]}"; do
        echo "  - $failed"
    done
    echo "========================================="
    exit 1
fi

if [[ "$SANITY_MODE" == "true" ]]; then
    echo "All sanity checks passed."
else
    echo "All model checks passed."
fi
