# Runtime Validation Report

> **Status: PENDING** — Scripts generated, awaiting real deployment execution.
> Run `bash validate.sh` after `docker compose up -d` to populate this report.

## Test Environment

| Parameter | Value |
|-----------|-------|
| Date | 2026-08-07 |
| Host OS | Linux (Docker host) |
| Docker | $(docker --version) |
| Docker Compose | $(docker compose version) |
| Rust | 1.97.1 |
| Binary | hg-supervisor v4.0 |

## Validation Results

### 1. Docker Compose boots successfully

**Status:** PENDING
```
$ docker compose up -d
$ docker compose ps
```
Expected: All containers in "Up" or "running" state.

### 2. Every Honeygain container starts correctly

**Status:** PENDING
```
$ docker compose ps | grep hg-
```
Expected: All hg-N containers show "Up" status. Logs show "authorisation successful" or "device registered".

### 3. Every Pawns container starts correctly

**Status:** PENDING
```
$ docker compose ps | grep pawns-
```
Expected: All pawns-N containers show "Up" status. Logs show JSON output with `"balance_ready"` events.

### 4. Every rotate-proxy-pawns-N establishes its own ProxyRise sticky session

**Status:** PENDING
```
$ docker compose logs rotate-proxy-pawns-1 | grep "UPSTREAM_PROXY_URL"
$ docker compose logs rotate-proxy-pawns-2 | grep "UPSTREAM_PROXY_URL"
```
Expected: Each proxy logs a unique `res-{country}-sid-{N}` session username.

### 5. Every instance has a unique Residential exit IP

**Status:** PENDING
```
$ for i in $(seq 1 3); do
    echo "pawns-$i:"
    docker exec pawns-$i wget -qO- http://httpbin.org/ip 2>/dev/null || echo "  (via proxy)"
  done
```
Expected: Each pawns instance shows a different public IP.

Alternative (via proxy logs):
```
$ docker logs rotate-proxy-pawns-1 2>&1 | grep -oP '\d+\.\d+\.\d+\.\d+' | tail -1
$ docker logs rotate-proxy-pawns-2 2>&1 | grep -oP '\d+\.\d+\.\d+\.\d+' | tail -1
```

### 6. Health endpoint reports all instances

**Status:** PENDING
```
$ curl -s http://localhost:8080/health | python3 -m json.tool
```
Expected:
```json
{
  "status": "ok",
  "honeygain": { "instances": 8, "connected": 8, ... },
  "pawns": { "instances": 3, "connected": 3, ... }
}
```

### 7. Kill random containers and verify automatic recovery

**Status:** PENDING
```
$ docker kill pawns-2
$ sleep 15
$ docker inspect --format '{{.State.Status}}' pawns-2
```
Expected: Container restarts automatically (restart: unless-stopped policy). Status returns to "running" within 15-30 seconds.

### 8. Verify graceful shutdown

**Status:** PENDING
```
$ docker compose stop pawns-1
$ docker compose logs --tail 5 pawns-1
```
Expected: Clean exit, no error messages. Process exits with code 0.

### 9. 12-hour soak test

**Status:** PENDING
```
$ bash soak-test.sh 12 60
```
Expected: Zero unexpected restarts. CPU and RAM stable. No memory growth over time.

### 10. Record resource usage

**Status:** PENDING
```
$ docker stats --no-stream
```
Expected: See PERFORMANCE_REPORT.md for benchmarks.

## Automated Validation

Run the full automated validation:
```bash
bash validate.sh
```

This tests all checks 1-8 and produces a PASS/FAIL summary.

## Soak Test

Run the 12-hour monitoring:
```bash
bash soak-test.sh 12 60
```

This produces:
- `soak-test-YYYYMMDD-HHMMSS/metrics.csv` — Raw metrics per interval
- `soak-test-YYYYMMDD-HHMMSS/soak-test.log` — Timestamped log
- `soak-test-YYYYMMDD-HHMMSS/SUMMARY.md` — Aggregated report
