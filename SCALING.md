# Scaling Guide

## How It Works

`generate-compose.py` reads `HG_INSTANCES` and `PAWNS_INSTANCES` from environment variables and generates a complete `docker-compose.yml` with:

- **Honeygain**: 1 shared `rotate-proxy` + N honeygain containers (all share network namespace)
- **Pawns**: N dedicated `rotate-proxy-pawns-{i}` + N pawns containers (1:1 isolation)

Each Pawns proxy gets a unique ProxyRise sticky session (`res-{country}-sid-{i}`), ensuring unique residential exit IPs.

## Scaling Examples

### 1 instance each

```bash
HG_INSTANCES=1 PAWNS_INSTANCES=1 python generate-compose.py > docker-compose.yml
docker compose up -d
```

Result: 1 rotate-proxy + 1 hg + 1 rotate-proxy-pawns-1 + 1 pawns-1 = **4 containers**

### 10 instances each

```bash
HG_INSTANCES=10 PAWNS_INSTANCES=10 python generate-compose.py > docker-compose.yml
docker compose up -d
```

Result: 1 rotate-proxy + 10 hg + 10 rotate-proxy-pawns + 10 pawns = **21 containers**

### 50 instances each

```bash
HG_INSTANCES=50 PAWNS_INSTANCES=50 python generate-compose.py > docker-compose.yml
docker compose up -d
```

Result: 1 rotate-proxy + 50 hg + 50 rotate-proxy-pawns + 50 pawns = **151 containers**

### 100 instances each

```bash
HG_INSTANCES=100 PAWNS_INSTANCES=100 python generate-compose.py > docker-compose.yml
docker compose up -d
```

Result: 1 rotate-proxy + 100 hg + 100 rotate-proxy-pawns + 100 pawns = **301 containers**

## Resource Estimates

| Instances | Containers | RAM (approx) | CPU (approx) |
|-----------|-----------|-------------|-------------|
| 1+1 | 4 | ~150 MB | ~0.5 cores |
| 10+10 | 21 | ~1.5 GB | ~2 cores |
| 50+50 | 151 | ~8 GB | ~8 cores |
| 100+100 | 301 | ~15 GB | ~15 cores |

Per-component estimates:
- `rotate-proxy` (shared): ~20 MB RAM
- `rotate-proxy-pawns-N` (dedicated): ~10-20 MB RAM each
- `honeygain`: ~50-100 MB RAM each
- `pawns-cli`: ~30-50 MB RAM each

## Port Allocation

### Honeygain

| Component | Host Port | Container Port |
|-----------|-----------|---------------|
| rotate-proxy | 8081 | 8080 |
| monitor | 9090 | 80 |

### Pawns

Each proxy gets a unique host port starting from `PAWNS_PROXY_BASE_PORT` (default: 8082):

| Instance | Host Port |
|----------|-----------|
| pawns-1 | 8082 |
| pawns-2 | 8083 |
| pawns-3 | 8084 |
| pawns-N | 8082 + N - 1 |

## Network Isolation

### Honeygain

All honeygain instances share the `rotate-proxy` network namespace. IP diversity comes from per-instance sticky session credentials (`res-{country}-sid-{N}`), not from separate network namespaces.

```
hg-1 ─┐
hg-2 ─┤── network_mode: service:rotate-proxy
hg-N ─┘         │
                 ▼
         iptables REDIRECT → port 8080 → ProxyRise
         (different sid-N per instance = different exit IPs)
```

### Pawns

Each pawns instance gets a completely isolated network namespace via its own proxy container:

```
pawns-1 ──network_mode──> rotate-proxy-pawns-1 ──socks5──> res-us-sid-1 ──> Exit IP A
pawns-2 ──network_mode──> rotate-proxy-pawns-2 ──socks5──> res-us-sid-2 ──> Exit IP B
pawns-N ──network_mode──> rotate-proxy-pawns-N ──socks5──> res-us-sid-N ──> Exit IP N
```

## Customization

### Change Pawns country

```bash
PAWNS_COUNTRY=de python generate-compose.py > docker-compose.yml
```

This sets all Pawns sticky sessions to German exit IPs (`res-de-sid-1`, `res-de-sid-2`, ...).

### Change base port

```bash
PAWNS_PROXY_BASE_PORT=9000 python generate-compose.py > docker-compose.yml
```

### Change tunnel rotation interval

```bash
TUNNEL_MAX_LIFETIME_SECS=300 python generate-compose.py > docker-compose.yml
```

Shorter intervals = more frequent IP rotation = more diverse IPs over time.

## Performance Notes

- The shared `rotate-proxy` for Honeygain handles all HG traffic through a single iptables chain. At 100+ instances, consider monitoring CPU usage.
- Each Pawns proxy container is lightweight (~10-20MB) since it only handles one instance's traffic.
- Docker Compose can handle 300+ containers, but `docker compose ps` may be slow. Use `docker ps --filter "name=pawns"` for faster filtering.
- At scale, consider using Docker Swarm or Kubernetes for better container orchestration.
