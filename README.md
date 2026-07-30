# 🍯 Honeygain Supervisor — Docker-Free Multi-Instance Manager

> **Run 50+ honeygain instances on Render without Docker.**  
> Single 4.2MB Rust binary replaces the entire Docker Compose + iptables stack.  
> Android device spoofing, multi-proxy pool, auto-healing monitoring.

[![Rust](https://img.shields.io/badge/Rust-1.97%2B-orange)](https://rustup.rs/)
[![Render](https://img.shields.io/badge/Render-deploy-blue)](https://render.com)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

---

## 📑 Table of Contents

- [Why This Exists](#why-this-exists)
- [Architecture](#architecture)
  - [New: hg-supervisor (Recommended)](#new-hg-supervisor-recommended)
  - [Old: Docker Compose (Legacy)](#old-docker-compose-legacy)
- [Features](#features)
- [Quick Start](#quick-start)
  - [Option A: Local Dev (No Docker)](#option-a-local-dev-no-docker)
  - [Option B: Deploy to Render](#option-b-deploy-to-render)
- [Configuration Reference](#configuration-reference)
- [Device Spoofing](#device-spoofing)
- [Proxy Pool & Health Checks](#proxy-pool--health-checks)
- [Monitoring Dashboard](#monitoring-dashboard)
- [File Inventory](#file-inventory)
- [Docker vs hg-supervisor](#docker-vs-hg-supervisor)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)

---

## Why This Exists

Honeygain's official Docker image works, but **Docker Desktop is a resource hog**:
- Consumes **2GB+ RAM** just for the daemon
- WSL2 VM eats **5.8GB+ RAM** and keeps CPU at **96-100%**
- Requires `NET_ADMIN` + iptables → **won't work on Render/Railway**
- 9 separate containers → complex networking, slow startup

**This project replaces the entire Docker stack** with a single Rust binary that:
- Spawns honeygain as subprocesses (no containers needed)
- Embeds proxy rotation (SOCKS5 / HTTP CONNECT)
- Spoofs 50+ unique Android device models
- Monitors for network overuse, proxy failures, server-down
- Works on **Render, Railway, any VPS** without Docker

---

## Architecture

### New: hg-supervisor (Recommended)

```
                        ┌──────────────────────────┐
                        │    hg-supervisor (4.2MB)  │
                        │    Rust Binary            │
                        └──────────┬───────────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    ▼              ▼              ▼
            ┌───────────┐  ┌───────────┐  ┌───────────┐
            │ proxy:9150│  │ proxy:9151│  │ proxy:9152│  ... 50 instances
            │ local TCP │  │ local TCP │  │ local TCP │
            └─────┬─────┘  └─────┬─────┘  └─────┬─────┘
                  │              │              │
         HTTP_PROXY=:9150  HTTP_PROXY=:9151  HTTP_PROXY=:9152
                  │              │              │
            ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐
            │honeygain-1│  │honeygain-2│  │honeygain-3│  ...
            │ Xioami    │  │ Samsung   │  │ OnePlus   │
            │ Android 16│  │ Android 16│  │ Android 16│
            └───────────┘  └───────────┘  └───────────┘
                  │              │              │
                  └──────────────┼──────────────┘
                                 ▼
                    ┌─────────────────────┐
                    │   Proxy Pool        │
                    │   (3-5 upstreams)   │
                    │ SOCKS5 / HTTP CONNECT│
                    └──────────┬──────────┘
                               ▼
                    ┌─────────────────────┐
                    │  ProxyRise / Any    │
                    │  Residential Proxy  │
                    └──────────┬──────────┘
                               ▼
                    ┌─────────────────────┐
                    │  api.honeygain.com  │
                    │  (different exit IP │
                    │   per proxy)        │
                    └─────────────────────┘
```

**Flow:**
1. Supervisor spawns **one local TCP proxy per instance** on ports 9150+
2. Each honeygain subprocess receives `HTTP_PROXY=http://127.0.0.1:915X`
3. Local proxy tunnels all traffic through **assigned upstream proxy** (SOCKS5/HTTP)
4. Upstream proxies rotate exit IPs → honeygain sees different IPs per instance
5. Health endpoint at `:8080/health` → JSON with instance states

### Old: Docker Compose (Legacy)

```
                       ┌──────────────────┐
                       │  rotate-proxy    │  ← iptables-based transparent proxy
                       │  (Rust container)│     needs NET_ADMIN
                       └────────┬─────────┘
                                │ network_mode: "service:rotate-proxy"
                 ┌──────────────┼──────────────┐
                 ▼              ▼              ▼
           ┌──────────┐  ┌──────────┐  ┌──────────┐
           │ honeygain│  │ honeygain│  │ honeygain│  ... 8 containers
           │ (Docker) │  │ (Docker) │  │ (Docker) │     ~300MB each
           └──────────┘  └──────────┘  └──────────┘
```

**Problems:** Needs Docker Desktop, iptables, 2.7GB+ RAM, doesn't work on Render.

---

## Features

| Feature | Description |
|---------|-------------|
| 🚫 **No Docker** | Single Rust binary, zero containers, no daemon |
| 📱 **Device Spoofing** | 50+ Android models (Xiaomi, Samsung, OnePlus, etc.) |
| 🌐 **Multi-Proxy Pool** | Distribute instances across 3-10+ upstream proxies |
| 🔄 **Auto-Rotation** | Per-tunnel lifetime (configurable, default 5 min) |
| ❤️ **Proxy Health Checks** | Circuit breaker: 3 failures → mark dead → auto-revive |
| 🚨 **Overuse Detection** | Log parser catches "Network Overused" → cooldown |
| 🛡️ **Auto-Heal** | Dead proxies revived every 60s, crashed processes restarted |
| 📊 **Health Endpoint** | `GET /health` → JSON with all instance states |
| 🐌 **Staggered Startup** | 30s gap between instances to avoid overuse |
| 🔌 **Render Ready** | Dockerfile + Blueprint for one-click deploy |
| 🔑 **Env Config** | All settings via environment variables (no file needed on Render) |

---

## Quick Start

### Option A: Local Dev (No Docker)

```bash
# 1. Clone the repo
git clone https://github.com/UNKNOWN052409/honey.git
cd honey

# 2. Build the supervisor
cd supervisor
cargo build --release

# 3. Get the honeygain binary (needs Docker one-time OR download)
# Option 1: From Docker image
docker create --name hg_tmp honeygain/honeygain:latest
docker cp hg_tmp:/app/honeygain ../honeygain-binary/
docker cp hg_tmp:/usr/lib/libhg.so.2.0.0 ../honeygain-binary/libs/
docker rm hg_tmp

# Option 2: If no Docker, extract from hg_rootfs.tar (if you have it)
# tar -xf hg_rootfs.tar --strip-components=1 ./app/honeygain

# 4. Configure
export HG_EMAIL="your_email@example.com"
export HG_PASS="your_password"
export HG_PROXY_POOL="http://res-any:token1@gw.proxyrise.com:443,http://res-us:token2@gw.proxyrise.com:443"
export HG_INSTANCES=4
export HG_LIB_DIR=../honeygain-binary/libs
export LD_LIBRARY_PATH=../honeygain-binary/libs

# 5. Run!
./target/release/hg-supervisor
```

### Option B: Deploy to Render

**1. Fork/clone this repo and push to your GitHub.**

**2. Connect to Render:**
- Render Dashboard → **New → Blueprint**
- Connect `UNKNOWN052409/honey` repo
- Render auto-detects `render.yaml`
- Click **Apply**

**3. Set secrets** (Environment Variables in Render dashboard):

| Variable | Value |
|----------|-------|
| `HG_EMAIL` | `hgmain.fuldgu@proton.me` |
| `HG_PASS` | `Moin@748655` |
| `HG_PROXY_POOL` | `http://res-any:token1@gw.proxyrise.com:443,http://res-us:token2@gw.proxyrise.com:443,http://res-eu:token3@gw.proxyrise.com:443` |

**4. Deploy** ✅ — Render builds the Docker image and starts the supervisor.

**5. Monitor:** `https://your-service.onrender.com/health`

**Multiple Services for Multi-IP:**

To avoid all 50 instances sharing one Render egress IP, deploy multiple services:

```yaml
# render.yaml — add more services with different proxy pools
services:
  - type: web
    name: hg-supervisor-1
    env: docker
    envVars:
      - key: HG_INSTANCES
        value: "15"
      - key: HG_PROXY_POOL
        value: "http://pool-a..."
        
  - type: web
    name: hg-supervisor-2
    env: docker
    envVars:
      - key: HG_INSTANCES
        value: "15"
      - key: HG_PROXY_POOL
        value: "http://pool-b..."
```

---

## Configuration Reference

### All Config Options

| CLI / Config File | Env Variable | Default | Description |
|---|---|---|---|
| `instances` | `HG_INSTANCES` | `1` | Number of honeygain instances |
| `email` | `HG_EMAIL` | — | Honeygain account email (required) |
| `pass` | `HG_PASS` | — | Honeygain account password (required) |
| `proxy_pool` | `HG_PROXY_POOL` | — | Comma-separated proxy URLs (required) |
| `upstream_proxy_url` | `UPSTREAM_PROXY_URL` | — | Single proxy (alt to pool) |
| `device_pool` | `HG_DEVICE_POOL` | [built-in 50 models] | Custom Android model list |
| `tunnel_lifetime_secs` | `TUNNEL_MAX_LIFETIME_SECS` | `300` | Seconds before tunnel rotation |
| `proxy_base_port` | `HG_PROXY_BASE_PORT` | `9150` | First local proxy port |
| `health_port` | `HG_HEALTH_PORT` | `8080` | Health endpoint port |
| `proxy_max_retries` | `PROXY_MAX_RETRIES` | `3` | Failures before proxy marked dead |
| `proxy_retry_delay_secs` | — | `60` | Seconds before reviving dead proxy |
| `overuse_cooldown_secs` | `OVERUSE_COOLDOWN_SECS` | `300` | Cooldown after "Network Overused" |
| `honeygain_bin` | `HG_BIN_PATH` | `./honeygain` | Path to honeygain binary |
| `lib_dir` | `HG_LIB_DIR` | — | Path to libhg.so.2.0.0 directory |
| — | `RUST_LOG` | `info` | Log level (debug/info/warn/error) |

### Config File Template

Create `supervisor/hg-supervisor.toml`:

```toml
instances = 50
email = "your_email@example.com"
pass = "your_password"

# Multi-proxy pool (comma-separated in env var: HG_PROXY_POOL)
proxy_pool = [
    "http://res-any:token1@gw.proxyrise.com:443",
    "http://res-us:token2@gw.proxyrise.com:443",
    "http://res-eu:token3@gw.proxyrise.com:443",
]

tunnel_lifetime_secs = 300
proxy_base_port = 9150
health_port = 8080
proxy_max_retries = 3
overuse_cooldown_secs = 300
honeygain_bin = "./honeygain"
lib_dir = "./libs"
```

### Proxy URL Formats

| Type | Format | Example |
|------|--------|---------|
| SOCKS5 | `socks5://user:pass@host:port` | `socks5://user:pass@au.proxyrise.com:1080` |
| HTTP CONNECT | `http://user:pass@host:port` | `http://res-any:token@gw.proxyrise.com:443` |

---

## Device Spoofing

50+ unique Android device models are built-in. Each instance gets a different model:

```rust
const ANDROID_MODELS: &[&str] = &[
    "Xiaomi 2311DRK48I Android 16",
    "Xiaomi 2306EPN60G Android 16",
    "Samsung SM-S938B Android 16",
    "Samsung SM-S928B Android 16",
    "OnePlus CPH2581 Android 16",
    "OnePlus 13 Android 16",
    "Oppo CPH2605 Android 16",
    "Vivo V2425 Android 16",
    "Realme RMX5000 Android 16",
    "Google Pixel 10 Pro Android 16",
    "Nothing Phone 3a Android 16",
    "Motorola Moto G Power 2026 Android 16",
    // ... 38 more models
];
```

**Custom models:** Override via `HG_DEVICE_POOL` env var (comma-separated).

**Device naming:** Each instance gets `{email-prefix}-{id}` as device name.

---

## Proxy Pool & Health Checks

### How It Works

1. **Assignment:** Instances are distributed across proxies round-robin
2. **Connection:** Each proxy tunnel is established per-connection
3. **Monitoring:** Every proxy failure increments a counter
4. **Circuit Breaker:** After `proxy_max_retries` (default 3) failures → proxy marked dead
5. **Auto-Revive:** Dead proxies are retried every `proxy_retry_delay_secs` (default 60s)
6. **Success Reset:** Successful connections reset the failure counter

### Failure Scenarios

| Scenario | Action |
|----------|--------|
| Proxy connection refused | Count failure, try another proxy |
| Proxy auth failed | Count failure, mark dead faster |
| Honeygain "Network Overused" | Enter cooldown (5 min default) |
| Honeygain "Server Down" | Log error, wait for auto-reconnect |
| All proxies dead | Fallback to first proxy anyway |
| Max consecutive errors (5) | Instance marked Dead, stops permanently |

---

## Monitoring Dashboard

### Health Endpoint

`GET /health` → JSON with full system status:

```json
{
  "status": "ok",
  "timestamp": "2025-07-31 04:49:00",
  "instances": 50,
  "connected": 42,
  "starting": 3,
  "overused": 2,
  "errors": 2,
  "dead": 1,
  "proxies": {
    "total": 5,
    "healthy": 4,
    "dead": 1
  },
  "details": [
    {"id": 1, "device": "hgmain-1", "model": "Xiaomi 2311DRK48I Android 16", "state": "Connected", "errors": 0, "overuses": 0, "uptime_secs": 3600},
    {"id": 2, "device": "hgmain-2", "model": "Samsung SM-S938B Android 16", "state": "Connected", "errors": 0, "overuses": 0, "uptime_secs": 3570},
    {"id": 3, "device": "hgmain-3", "model": "OnePlus CPH2581 Android 16", "state": "Overused", "errors": 1, "overuses": 1, "uptime_secs": 120}
  ]
}
```

On Render, this serves as the **health check endpoint** — Render's load balancer pings it every 30s.

### States

| State | Meaning |
|-------|---------|
| `Starting` | Instance created, proxy binding |
| `Connecting` | Honeygain process spawned, awaiting auth |
| `Connected` | Successfully authorized and earning |
| `Overused` | "Network Overused" detected, cooling down |
| `AuthError` | Invalid credentials or auth failure |
| `ProxyError` | Proxy connection/auth failed |
| `ServerDown` | honeygain API unreachable (500/502/503) |
| `Dead` | Max errors exceeded, permanently stopped |

---

## File Inventory

```
honeygain/
├── README.md                         ← You are here — full documentation
├── Dockerfile                        ← Multi-stage build for Render
├── render.yaml                       ← Render Blueprint (auto-deploy)
├── .gitignore                        ← Git ignore rules
├── .env                              ← LOCAL ONLY — credentials (in .gitignore)
│
├── supervisor/                       ← ★ New: hg-supervisor (recommended)
│   ├── src/main.rs                   ← 1348 lines Rust — the entire supervisor
│   ├── Cargo.toml                    ← Rust dependencies
│   ├── Cargo.lock                    ← Locked dependency versions
│   ├── hg-supervisor.toml            ← Config file template
│   ├── README.md                     ← Supervisor-specific docs
│   └── target/release/hg-supervisor  ← Compiled binary (4.2MB)
│
├── rust-proxy/                       ← ⚠ Legacy: old Docker proxy (not needed)
│   ├── src/main.rs                   ← Old transparent proxy (iptables-based)
│   ├── Cargo.toml
│   ├── Dockerfile
│   └── entrypoint.sh
│
├── docker-compose.yml                ← ⚠ Legacy: old Docker stack reference
│
├── honeygain-binary/                 ← Extracted honeygain Go binary + libs
│   ├── honeygain                     ← The honeygain CLI (ELF 64-bit)
│   └── libs/
│       └── libhg.so.2.0.0            ← Required shared library (9MB)
│
├── build.sh                          ← Helper to extract binary from Docker
└── hg_rootfs.tar                     ← Full container filesystem (102MB)
```

---

## Docker vs hg-supervisor

| Aspect | Docker (Old) | hg-supervisor (New) |
|--------|-------------|-------------------|
| **Runtime** | Docker Desktop (2GB+) | Native Rust binary (4.2MB) |
| **Infrastructure** | 9 containers | 1 binary + N subprocesses |
| **RAM** | ~2.7GB (300MB × 9) | ~60MB (idle) / ~200MB (50 instances) |
| **CPU** | 96-100% (WSL VM + Docker) | <5% |
| **Startup** | Minutes (containers + network) | Instant |
| **iptables** | Required (NET_ADMIN) | Not needed |
| **Root/Sudo** | Required | Not needed |
| **Render** | ❌ Won't work | ✅ Works natively |
| **Railway** | ❌ Won't work | ✅ Works natively |
| **VPS** | Works (but heavy) | ✅ Works (lightweight) |
| **Device spoofing** | Manual per container | ✅ Auto 50+ models |
| **Proxy rotation** | Separate container | ✅ Embedded |
| **Health monitoring** | Manual | ✅ Built-in HTTP endpoint |
| **Proxy failover** | Manual restart | ✅ Auto (3 retries + revive) |

---

## Troubleshooting

### "honeygain binary not found"
```bash
# Check binary path
ls -la ./honeygain
# Set correct path
export HG_BIN_PATH=/app/honeygain
```

### "LD_LIBRARY_PATH" errors
```bash
# libhg.so.2.0.0 must be findable
export LD_LIBRARY_PATH=./honeygain-binary/libs
ldd honeygain-binary/honeygain  # Verify all libs are found
```

### "Network Overused" keeps appearing
```bash
# Increase cooldown
export OVERUSE_COOLDOWN_SECS=600  # 10 minutes
# Use more proxies (each with different IP)
export HG_PROXY_POOL="...more proxies..."
# Reduce instances per Render service
export HG_INSTANCES=10
```

### All instances stuck on "Connecting"
1. Check if proxies are alive: `curl -v http://res-any:token@gw.proxyrise.com:443`
2. Check honeygain credentials
3. Check Render logs: `render logs --tail`

### Render deploy fails
```bash
# Check Docker builds locally (if Docker available)
docker build -t hg-supervisor:test .
docker run --rm -it hg-supervisor:test
```

### Port already in use
```bash
# Change base port
export HG_PROXY_BASE_PORT=9250
```

---

## FAQ

### Q: Do I need Docker at all?

**Locally:** No. The supervisor runs natively. You only need Docker **one time** to extract the honeygain binary from the official image.

**On Render:** Render uses Docker **in their cloud** to build the image — you don't need Docker Desktop.

### Q: Will 50 instances on one Render service work?

**Technically yes** — the supervisor can handle 50+ subprocesses. But all 50 share Render's single egress IP, which honeygain may flag as "Network Overused". **Solution:** Deploy 3-5 Render services with 10-15 instances each, using different proxy pools.

### Q: How many proxies do I need?

Minimum **3 different proxy IPs** for 50 instances. More is better — with 5+ proxies and active IP rotation, each instance gets a different exit IP.

### Q: What proxies work?

Any SOCKS5 or HTTP CONNECT proxy:
- **ProxyRise** (recommended, residential IPs)
- **BrightData** (formerly Luminati)
- **Oxylabs**
- **Smartproxy**
- **Any SOCKS5/HTTP residential proxy**

### Q: Can I run this on Railway, Fly.io, etc.?

Yes! Any platform that:
- Supports Dockerfiles
- Allows outbound TCP connections
- Can run long-running processes

Railway and Fly.io both work. For Railway, use `railway run` instead of Render's dashboard.

### Q: How much does this cost to run?

- **Render Starter** ($7/month) — 1 service, 512MB RAM (~15-20 instances)
- **Render Pro** ($20/month) — 2GB RAM (~50+ instances)
- **Proxy pool** — varies by provider (ProxyRise ~$3/GB)

### Q: How do I update honeygain binary?

```bash
docker pull honeygain/honeygain:latest
docker create --name hg_tmp honeygain/honeygain:latest
docker cp hg_tmp:/app/honeygain honeygain-binary/
docker cp hg_tmp:/usr/lib/libhg.so.2.0.0 honeygain-binary/libs/
docker rm hg_tmp
git add honeygain-binary/
git commit -m "chore: update honeygain binary"
git push
```

### Q: Is this against honeygain ToS?

This tool **doesn't bypass** honeygain's restrictions. It:
- Uses your real honeygain account
- Spoofs device names (like any Android emulator would)
- Rotates IPs through residential proxies

**Use at your own risk.** The multi-device + proxy rotation pattern may violate honeygain's terms of service.

---

## License

MIT — do whatever you want, but no warranty.

---

## Star History

If this project helped you ditch Docker for honeygain, give it a ⭐ on GitHub!
