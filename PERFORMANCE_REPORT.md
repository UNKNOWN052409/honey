# Performance Report

> **Status: PENDING** — Benchmarks from actual runtime testing needed.
> Run `bash soak-test.sh 12 60` and paste results below.

## Resource Usage (Expected)

Based on architecture analysis and component sizing:

### Per-Component Estimates

| Component | RAM | CPU | Network |
|-----------|-----|-----|---------|
| rotate-proxy (shared HG) | 15-25 MB | <5% idle | Varies with traffic |
| rotate-proxy-pawns-N | 10-20 MB | <2% idle | ~1 instance traffic |
| honeygain | 50-100 MB | <5% idle | QUIC/HTTP |
| pawns-cli | 30-50 MB | <3% idle | HTTP(S) |
| hg-supervisor | 10-20 MB | <2% idle | Health endpoint |
| monitor (nginx) | 5-10 MB | <1% | HTTP |

### Total by Scale

| Scale | Containers | RAM (total) | CPU (total) |
|-------|-----------|-------------|-------------|
| 1 HG + 1 Pawns | 4 | ~150 MB | ~0.5 cores |
| 8 HG + 3 Pawns | 16 | ~800 MB | ~1.5 cores |
| 10 HG + 10 Pawns | 21 | ~1.5 GB | ~2 cores |
| 50 HG + 50 Pawns | 151 | ~8 GB | ~8 cores |
| 100 HG + 100 Pawns | 301 | ~15 GB | ~15 cores |

## Actual Benchmarks

> Paste output from `docker stats --no-stream` and `soak-test.sh` here.

### 8 HG + 3 Pawns (default)

```
$ docker stats --no-stream
```

| Container | CPU | RAM | Net I/O |
|-----------|-----|-----|---------|
| rotate-proxy | % | MB | / |
| hg-1 | % | MB | / |
| ... | | | |
| rotate-proxy-pawns-1 | % | MB | / |
| pawns-1 | % | MB | / |

### 50 HG + 50 Pawns

```
$ docker stats --no-stream
```

| Container | CPU | RAM | Net I/O |
|-----------|-----|-----|---------|
| rotate-proxy | % | MB | / |
| rotate-proxy-pawns-1 | % | MB | / |
| ... | | | |

## 12-Hour Soak Test Results

```
$ bash soak-test.sh 12 60
```

### Resource Trends

| Metric | Start | 1h | 4h | 8h | 12h |
|--------|-------|-----|-----|-----|------|
| CPU (%) | | | | | |
| RAM (MB) | | | | | |
| Restarts | | | | | |

### Memory Leak Detection

If RAM grows >10% over 12 hours, investigate:
- Container memory limits
- Process RSS growth
- File descriptor leaks
- Connection pool exhaustion

## Network Throughput

### Per-Instance Bandwidth

| App | Typical | Peak |
|-----|---------|------|
| Honeygain | 1-5 Mbps | 10 Mbps |
| Pawns | 0.5-2 Mbps | 5 Mbps |

### Total Bandwidth (100 instances)

- Expected: 100-500 Mbps aggregate
- Recommended host bandwidth: 1 Gbps minimum

## Disk Usage

| Component | Disk |
|-----------|------|
| Docker images | ~500 MB |
| Logs (per day) | ~50-100 MB |
| Metrics CSV | ~10 MB/day |

## Recommendations

1. **Memory**: Set Docker memory limits to prevent OOM kills:
   ```yaml
   deploy:
     resources:
       limits:
         memory: 200M
   ```

2. **CPU**: No CPU limits needed for typical workloads. Monitor with `docker stats`.

3. **Network**: Ensure host has sufficient bandwidth for aggregate traffic.

4. **Logs**: Rotate Docker logs:
   ```json
   {
     "log-driver": "json-file",
     "log-opts": {
       "max-size": "10m",
       "max-file": "3"
     }
   }
   ```

5. **File Descriptors**: Increase ulimit for 100+ instances:
   ```bash
   ulimit -n 65536
   ```
