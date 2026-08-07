#!/usr/bin/env bash
# soak-test.sh — 12-hour runtime monitoring for hg-supervisor v4.0
# Collects CPU, RAM, restart count, and network usage every 60 seconds.
# Usage: bash soak-test.sh [DURATION_HOURS] [INTERVAL_SECS]
set -euo pipefail

DURATION_HOURS=${1:-12}
INTERVAL_SECS=${2:-60}
TOTAL_SECS=$((DURATION_HOURS * 3600))
OUTPUT_DIR="soak-test-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUTPUT_DIR"

LOG_FILE="$OUTPUT_DIR/soak-test.log"
METRICS_FILE="$OUTPUT_DIR/metrics.csv"
SUMMARY_FILE="$OUTPUT_DIR/SUMMARY.md"

echo "============================================"
echo "  hg-supervisor v4.0 Soak Test"
echo "  Duration: ${DURATION_HOURS}h | Interval: ${INTERVAL_SECS}s"
echo "  Output: $OUTPUT_DIR/"
echo "============================================"
echo ""

# CSV header
echo "timestamp,container,cpu_pct,mem_mb,mem_limit_mb,net_rx_bytes,net_tx_bytes,restart_count,status" > "$METRICS_FILE"

get_container_metrics() {
    local name=$1
    local stats
    stats=$(docker stats --no-stream --format "{{.Name}},{{.CPUPerc}},{{.MemUsage}},{{.NetIO}}" "$name" 2>/dev/null || echo "")
    if [ -z "$stats" ]; then
        return
    fi

    local cpu mem net
    cpu=$(echo "$stats" | cut -d',' -f2 | tr -d '%' || echo "0")
    mem=$(echo "$stats" | cut -d',' -f3)
    mem_used=$(echo "$mem" | awk '{print $1}' | sed 's/[A-Za-z]*//g' || echo "0")
    mem_limit=$(echo "$mem" | awk '{print $3}' | sed 's/[A-Za-z]*//g' || echo "0")
    net=$(echo "$stats" | cut -d',' -f4)
    net_rx=$(echo "$net" | awk -F'/' '{print $1}' | tr -d ' ' || echo "0")
    net_tx=$(echo "$net" | awk -F'/' '{print $2}' | tr -d ' ' || echo "0")

    local restarts status
    restarts=$(docker inspect --format '{{.RestartCount}}' "$name" 2>/dev/null || echo "0")
    status=$(docker inspect --format '{{.State.Status}}' "$name" 2>/dev/null || echo "unknown")

    # Convert memory to MB
    local mem_mb mem_limit_mb
    if echo "$mem_used" | grep -q 'GiB'; then
        mem_mb=$(echo "$mem_used" | sed 's/GiB//' | awk '{printf "%.1f", $1 * 1024}')
    elif echo "$mem_used" | grep -q 'MiB'; then
        mem_mb=$(echo "$mem_used" | sed 's/MiB//' | awk '{printf "%.1f", $1}')
    elif echo "$mem_used" | grep -q 'KiB'; then
        mem_mb=$(echo "$mem_used" | sed 's/KiB//' | awk '{printf "%.1f", $1 / 1024}')
    else
        mem_mb="$mem_used"
    fi

    if echo "$mem_limit" | grep -q 'GiB'; then
        mem_limit_mb=$(echo "$mem_limit" | sed 's/GiB//' | awk '{printf "%.1f", $1 * 1024}')
    elif echo "$mem_limit" | grep -q 'MiB'; then
        mem_limit_mb=$(echo "$mem_limit" | sed 's/MiB//' | awk '{printf "%.1f", $1}')
    else
        mem_limit_mb="$mem_limit"
    fi

    echo "$(date -Iseconds),$name,$cpu,$mem_mb,$mem_limit_mb,$net_rx,$net_tx,$restarts,$status"
}

# Get all container names
CONTAINERS=$(docker compose ps --format json 2>/dev/null | grep -o '"name":"[^"]*"' | sed 's/"name":"//;s/"//' || \
    docker compose ps | tail -n +2 | awk '{print $1}' || true)

if [ -z "$CONTAINERS" ]; then
    echo "ERROR: No containers found. Run 'docker compose up -d' first."
    exit 1
fi

CONTAINER_COUNT=$(echo "$CONTAINERS" | wc -w)
echo "Monitoring $CONTAINER_COUNT containers..."
echo ""

START_TIME=$(date +%s)
ELAPSED=0
ITERATION=0

echo "Start: $(date)" >> "$LOG_FILE"
echo "Containers: $CONTAINER_COUNT" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

while [ "$ELAPSED" -lt "$TOTAL_SECS" ]; do
    ITERATION=$((ITERATION+1))
    TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')
    echo "[$TIMESTAMP] Iteration $ITERATION (elapsed: $((ELAPSED/3600))h $((ELAPSED%3600/60))m)"

    TOTAL_CPU=0
    TOTAL_MEM=0
    TOTAL_RESTARTS=0

    for c in $CONTAINERS; do
        METRICS=$(get_container_metrics "$c")
        if [ -n "$METRICS" ]; then
            echo "$METRICS" >> "$METRICS_FILE"
            CPU=$(echo "$METRICS" | cut -d',' -f3)
            MEM=$(echo "$METRICS" | cut -d',' -f4)
            RESTARTS=$(echo "$METRICS" | cut -d',' -f9)
            TOTAL_CPU=$(echo "$TOTAL_CPU + $CPU" | bc 2>/dev/null || echo "0")
            TOTAL_MEM=$(echo "$TOTAL_MEM + $MEM" | bc 2>/dev/null || echo "0")
            TOTAL_RESTARTS=$((TOTAL_RESTARTS + RESTARTS))
        fi
    done

    echo "  CPU: ${TOTAL_CPU}% | RAM: ${TOTAL_MEM}MB | Restarts: $TOTAL_RESTARTS"
    echo "$TIMESTAMP cpu=${TOTAL_CPU}% mem=${TOTAL_MEM}MB restarts=$TOTAL_RESTARTS" >> "$LOG_FILE"

    sleep "$INTERVAL_SECS"
    ELAPSED=$(($(date +%s) - START_TIME))
done

echo "" >> "$LOG_FILE"
echo "End: $(date)" >> "$LOG_FILE"

# ── Generate Summary ──
echo "Generating summary..."

# Calculate stats from CSV
AVG_CPU=$(awk -F',' 'NR>1 {sum+=$3; n++} END {if(n>0) printf "%.1f", sum/n; else print "0"}' "$METRICS_FILE")
MAX_CPU=$(awk -F',' 'NR>1 {if($3>max) max=$3} END {print max+0}' "$METRICS_FILE")
AVG_MEM=$(awk -F',' 'NR>1 {sum+=$4; n++} END {if(n>0) printf "%.1f", sum/n; else print "0"}' "$METRICS_FILE")
MAX_MEM=$(awk -F',' 'NR>1 {if($4>max) max=$4} END {print max+0}' "$METRICS_FILE")
TOTAL_RESTARTS=$(awk -F',' 'NR>1 {sum+=$9; n++} END {print sum+0}' "$METRICS_FILE" | tail -1)

cat > "$SUMMARY_FILE" <<EOF
# Soak Test Summary

## Configuration
- Duration: ${DURATION_HOURS} hours
- Interval: ${INTERVAL_SECS} seconds
- Containers: $CONTAINER_COUNT
- Start: $(head -1 "$LOG_FILE" | cut -d' ' -f2-)
- End: $(tail -2 "$LOG_FILE" | head -1 | cut -d' ' -f2-)

## Resource Usage

| Metric | Average | Maximum |
|--------|---------|---------|
| CPU (%) | $AVG_CPU | $MAX_CPU |
| RAM (MB) | $AVG_MEM | $MAX_MEM |

## Stability
- Total restarts across all containers: $TOTAL_RESTARTS
- Uptime: $((ELAPSED/3600))h $((ELAPSED%3600/60))m

## Per-Container Breakdown

\`\`\`
$(awk -F',' 'NR>1 {printf "%-30s CPU: %6s%%  RAM: %7sMB  Restarts: %s  Status: %s\n", $2, $3, $4, $9, $10}' "$METRICS_FILE" | sort -u)
\`\`\`

## Files
- metrics.csv — Raw metrics (every interval)
- soak-test.log — Timestamped log
- SUMMARY.md — This file
EOF

echo ""
echo "============================================"
echo "  Soak Test Complete"
echo "  Duration: $((ELAPSED/3600))h $((ELAPSED%3600/60))m"
echo "  Avg CPU: ${AVG_CPU}% | Max CPU: ${MAX_CPU}%"
echo "  Avg RAM: ${AVG_MEM}MB | Max RAM: ${MAX_MEM}MB"
echo "  Restarts: $TOTAL_RESTARTS"
echo "  Output: $OUTPUT_DIR/"
echo "============================================"
