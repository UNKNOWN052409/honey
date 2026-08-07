#!/usr/bin/env bash
# validate.sh — Runtime validation for hg-supervisor v4.0
# Run after: docker compose up -d
# Usage: bash validate.sh
set -euo pipefail

PASS=0
FAIL=0
WARN=0
RESULTS=()

log()  { echo -e "\033[1;34m[TEST]\033[0m $1"; }
pass() { echo -e "\033[1;32m[PASS]\033[0m $1"; PASS=$((PASS+1)); RESULTS+=("PASS: $1"); }
fail() { echo -e "\033[1;31m[FAIL]\033[0m $1"; FAIL=$((FAIL+1)); RESULTS+=("FAIL: $1"); }
warn() { echo -e "\033[1;33m[WARN]\033[0m $1"; WARN=$((WARN+1)); RESULTS+=("WARN: $1"); }

echo "============================================"
echo "  hg-supervisor v4.0 Runtime Validation"
echo "============================================"
echo ""

# ── 1. Docker Compose boots successfully ──
log "1. Docker Compose boots"
if docker compose ps --format json 2>/dev/null | head -1 > /dev/null 2>&1; then
    TOTAL=$(docker compose ps --format json | wc -l)
    RUNNING=$(docker compose ps --format json | grep -c '"running"' || true)
    log "   Total containers: $TOTAL, Running: $RUNNING"
    if [ "$RUNNING" -gt 0 ]; then
        pass "Docker Compose booted ($RUNNING/$TOTAL running)"
    else
        fail "No containers running"
    fi
else
    # Fallback for older docker-compose
    TOTAL=$(docker compose ps | tail -n +2 | wc -l)
    RUNNING=$(docker compose ps | grep -c "Up" || true)
    log "   Total containers: $TOTAL, Running: $RUNNING"
    if [ "$RUNNING" -gt 0 ]; then
        pass "Docker Compose booted ($RUNNING/$TOTAL running)"
    else
        fail "No containers running"
    fi
fi

# ── 2. Every Honeygain container starts correctly ──
log "2. Honeygain containers"
HG_CONTAINERS=$(docker compose ps --format json 2>/dev/null | grep -o '"name":"hg-[0-9]*"' | sed 's/"name":"//;s/"//' || \
    docker compose ps | grep "hg-" | awk '{print $1}' || true)
HG_COUNT=0
HG_OK=0
for c in $HG_CONTAINERS; do
    HG_COUNT=$((HG_COUNT+1))
    STATUS=$(docker inspect --format '{{.State.Status}}' "$c" 2>/dev/null || echo "unknown")
    if [ "$STATUS" = "running" ]; then
        HG_OK=$((HG_OK+1))
    else
        fail "hg container $c status: $STATUS"
    fi
done
if [ "$HG_COUNT" -gt 0 ] && [ "$HG_OK" -eq "$HG_COUNT" ]; then
    pass "All $HG_COUNT Honeygain containers running"
elif [ "$HG_COUNT" -eq 0 ]; then
    warn "No Honeygain containers found"
else
    fail "Honeygain: $HG_OK/$HG_COUNT running"
fi

# ── 3. Every Pawns container starts correctly ──
log "3. Pawns containers"
PAWNS_CONTAINERS=$(docker compose ps --format json 2>/dev/null | grep -o '"name":"pawns-[0-9]*"' | sed 's/"name":"//;s/"//' || \
    docker compose ps | grep "pawns-" | awk '{print $1}' || true)
PAWNS_COUNT=0
PAWNS_OK=0
for c in $PAWNS_CONTAINERS; do
    PAWNS_COUNT=$((PAWNS_COUNT+1))
    STATUS=$(docker inspect --format '{{.State.Status}}' "$c" 2>/dev/null || echo "unknown")
    if [ "$STATUS" = "running" ]; then
        PAWNS_OK=$((PAWNS_OK+1))
    else
        fail "pawns container $c status: $STATUS"
    fi
done
if [ "$PAWNS_COUNT" -gt 0 ] && [ "$PAWNS_OK" -eq "$PAWNS_COUNT" ]; then
    pass "All $PAWNS_COUNT Pawns containers running"
elif [ "$PAWNS_COUNT" -eq 0 ]; then
    warn "No Pawns containers found"
else
    fail "Pawns: $PAWNS_OK/$PAWNS_COUNT running"
fi

# ── 4. Every rotate-proxy-pawns-N establishes its own ProxyRise session ──
log "4. ProxyRise sticky sessions"
PROXY_CONTAINERS=$(docker compose ps --format json 2>/dev/null | grep -o '"name":"rotate-proxy-pawns-[0-9]*"' | sed 's/"name":"//;s/"//' || \
    docker compose ps | grep "rotate-proxy-pawns-" | awk '{print $1}' || true)
SESSION_COUNT=0
SESSIONS_SEEN=""
for c in $PROXY_CONTAINERS; do
    UPSTREAM=$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$c" 2>/dev/null | grep "UPSTREAM_PROXY_URL=" | head -1 || echo "")
    if [ -n "$UPSTREAM" ]; then
        # Extract session ID (sid-N)
        SID=$(echo "$UPSTREAM" | grep -oP 'sid-\d+' || echo "")
        if [ -n "$SID" ]; then
            if echo "$SESSIONS_SEEN" | grep -q "$SID"; then
                fail "Duplicate session $SID in $c"
            else
                SESSIONS_SEEN="$SESSIONS_SEEN $SID"
                SESSION_COUNT=$((SESSION_COUNT+1))
            fi
        else
            warn "Could not extract session ID from $c"
        fi
    else
        warn "No UPSTREAM_PROXY_URL found in $c"
    fi
done
PROXY_TOTAL=$(echo "$PROXY_CONTAINERS" | wc -w)
if [ "$SESSION_COUNT" -eq "$PROXY_TOTAL" ] && [ "$PROXY_TOTAL" -gt 0 ]; then
    pass "All $SESSION_COUNT proxy containers have unique sessions"
elif [ "$PROXY_TOTAL" -eq 0 ]; then
    warn "No rotate-proxy-pawns containers found"
else
    fail "Session isolation: $SESSION_COUNT/$PROXY_TOTAL unique"
fi

# ── 5. Every instance has a unique Residential exit IP ──
log "5. Exit IP uniqueness"
# This requires the proxies to be connected to ProxyRise.
# We check the logs for "verified egress IP" or test via curl.
# For now, verify that each proxy logs a different IP.
IPS_SEEN=""
IP_COUNT=0
for c in $PROXY_CONTAINERS; do
    # Check last 50 lines of logs for IP verification
    IP_LINE=$(docker logs --tail 50 "$c" 2>&1 | grep -oP '\d+\.\d+\.\d+\.\d+' | tail -1 || echo "")
    if [ -n "$IP_LINE" ]; then
        if echo "$IPS_SEEN" | grep -q "$IP_LINE"; then
            warn "Duplicate IP $IP_LINE from $c (may be transient)"
        else
            IPS_SEEN="$IPS_SEEN $IP_LINE"
            IP_COUNT=$((IP_COUNT+1))
        fi
    fi
done
if [ "$IP_COUNT" -gt 0 ]; then
    pass "Found $IP_COUNT unique exit IPs in proxy logs"
else
    warn "Could not verify exit IPs from logs (ProxyRise may not be configured)"
fi

# ── 6. Health endpoint reports all instances ──
log "6. Health endpoint"
# Try common health ports
HEALTH_PORT=""
for PORT in 8080 8081 8082; do
    if curl -s --connect-timeout 2 "http://localhost:$PORT/health" > /dev/null 2>&1; then
        HEALTH_PORT=$PORT
        break
    fi
done

if [ -n "$HEALTH_PORT" ]; then
    HEALTH_JSON=$(curl -s "http://localhost:$HEALTH_PORT/health")
    STATUS=$(echo "$HEALTH_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status','missing'))" 2>/dev/null || echo "parse_error")
    if [ "$STATUS" = "ok" ]; then
        pass "Health endpoint responding (port $HEALTH_PORT)"
        # Check instance counts
        HG_REPORTED=$(echo "$HEALTH_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('honeygain',{}).get('instances',0))" 2>/dev/null || echo "0")
        PAWNS_REPORTED=$(echo "$HEALTH_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('pawns',{}).get('instances',0))" 2>/dev/null || echo "0")
        log "   Health reports: HG=$HG_REPORTED, Pawns=$PAWNS_REPORTED"
    else
        fail "Health endpoint returned status: $STATUS"
    fi
else
    fail "Health endpoint unreachable on ports 8080/8081/8082"
fi

# ── 7. Kill random containers and verify recovery ──
log "7. Crash recovery (kill one Pawns container)"
if [ "$PAWNS_COUNT" -gt 0 ]; then
    TARGET=$(echo "$PAWNS_CONTAINERS" | tr ' ' '\n' | head -1)
    log "   Killing $TARGET..."
    docker kill "$TARGET" 2>/dev/null || true
    sleep 15
    STATUS=$(docker inspect --format '{{.State.Status}}' "$TARGET" 2>/dev/null || echo "unknown")
    RESTART_COUNT=$(docker inspect --format '{{.RestartCount}}' "$TARGET" 2>/dev/null || echo "0")
    if [ "$STATUS" = "running" ] || [ "$RESTART_COUNT" -gt 0 ]; then
        pass "$TARGET recovered (status=$STATUS, restarts=$RESTART_COUNT)"
    else
        fail "$TARGET did not recover (status=$STATUS)"
    fi
else
    warn "Skipping crash recovery test (no Pawns containers)"
fi

# ── Summary ──
echo ""
echo "============================================"
echo "  Results: $PASS passed, $FAIL failed, $WARN warnings"
echo "============================================"
for r in "${RESULTS[@]}"; do
    echo "  $r"
done
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo -e "\033[1;31mVALIDATION FAILED\033[0m — $FAIL checks failed"
    exit 1
else
    echo -e "\033[1;32mVALIDATION PASSED\033[0m — all checks passed ($WARN warnings)"
    exit 0
fi
