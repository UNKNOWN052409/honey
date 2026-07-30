# 🚀 hg-supervisor v2.0 — Multi-Proxy Honeygain Manager

> **50+ honeygain instances, each as a unique Android device, across a pool of residential proxies.**  
> Single 4.2MB Rust binary replaces Docker entirely.

---

## Features

| Feature | Details |
|---------|---------|
| 🚫 **No Docker** | Zero containers, zero daemon, zero iptables |
| 📱 **Device Spoofing** | 50+ Android models (Xiaomi, Samsung, OnePlus, etc.) |
| 🌐 **Proxy Pool** | 3-10+ upstream proxies, instances distributed round-robin |
| ❤️ **Health Checks** | Circuit breaker: 3 failures → dead → auto-revive (60s) |
| 🚨 **Overuse Detection** | "Network Overused" → automatic cooldown |
| 🩺 **Health Endpoint** | `GET /health` → JSON with instance states |
| 🐌 **Staggered Startup** | 30s gap between instances |
| 🔧 **Render Ready** | Dockerfile + render.yaml for one-click deploy |

---

## Architecture

```
hg-supervisor (4.2MB)
├── Proxy Pool
│   ├── proxy-0 → http://res-any:token@gw.proxyrise.com:443
│   ├── proxy-1 → http://res-us:token@gw.proxyrise.com:443
│   ├── proxy-2 → http://res-eu:token@gw.proxyrise.com:443
│   └── proxy-3 → http://res-asia:token@gw.proxyrise.com:443
│
├── Instance Manager (50x)
│   ├── proxy:9150 ← honeygain-1 (Xiaomi 2311DRK48I)
│   ├── proxy:9151 ← honeygain-2 (Samsung SM-S938B)
│   ├── proxy:9152 ← honeygain-3 (OnePlus CPH2581)
│   └── ...
│
├── Monitor
│   ├── stdout parser → state machine
│   ├── proxy health → circuit breaker
│   └── overuse detection → cooldown
│
└── Health Server (:8080)
    └── GET /health → JSON
```

---

## Quick Start

### Prerequisites
- Rust toolchain (`cargo 1.97+`)
- Honeygain binary + `libhg.so.2.0.0` (in `../honeygain-binary/`)
- ProxyRise or any SOCKS5/HTTP proxy account

### 1. Build

```bash
cd supervisor
cargo build --release
```

### 2. Configure

Via env vars (recommended for Render):

```bash
export HG_EMAIL="your_email@example.com"
export HG_PASS="your_password"
export HG_PROXY_POOL="http://res-any:token1@gw.proxyrise.com:443,http://res-us:token2@gw.proxyrise.com:443"
export HG_INSTANCES=4
export HG_LIB_DIR=../honeygain-binary/libs
```

Or via config file:

```toml
# hg-supervisor.toml
instances = 4
email = "your_email@example.com"
pass = "your_password"
proxy_pool = [
    "http://res-any:token1@gw.proxyrise.com:443",
    "http://res-us:token2@gw.proxyrise.com:443",
]
tunnel_lifetime_secs = 300
proxy_base_port = 9150
health_port = 8080
proxy_max_retries = 3
overuse_cooldown_secs = 300
honeygain_bin = "./honeygain"
lib_dir = "./libs"
```

### 3. Run

```bash
export LD_LIBRARY_PATH=../honeygain-binary/libs
./target/release/hg-supervisor
```

### 4. Check Health

```bash
curl http://localhost:8080/health
```

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `HG_EMAIL` | — | Honeygain account email (required) |
| `HG_PASS` | — | Honeygain account password (required) |
| `HG_PROXY_POOL` | — | Comma-separated proxy URLs (required) |
| `UPSTREAM_PROXY_URL` | — | Single proxy (alt to pool) |
| `HG_DEVICE_POOL` | [50 built-in] | Custom Android model list |
| `HG_INSTANCES` | `1` | Number of instances |
| `TUNNEL_MAX_LIFETIME_SECS` | `300` | Tunnel rotation interval |
| `HG_PROXY_BASE_PORT` | `9150` | First proxy port |
| `HG_HEALTH_PORT` | `8080` | Health endpoint port |
| `PROXY_MAX_RETRIES` | `3` | Failures before proxy dead |
| `OVERUSE_COOLDOWN_SECS` | `300` | Cooldown after overuse |
| `HG_BIN_PATH` | `./honeygain` | Path to honeygain binary |
| `HG_LIB_DIR` | — | Path to libs directory |
| `RUST_LOG` | `info` | Log level |

---

## Proxy Pool

Multiple proxies → different exit IPs → avoid "Network Overused":

```bash
export HG_PROXY_POOL="http://pool-a:token@proxy1.com:443,http://pool-b:token@proxy2.com:443,http://pool-c:token@proxy3.com:443"
```

- Instances are distributed round-robin
- Each proxy has a circuit breaker (3 failures → dead → revive after 60s)
- Successes reset the failure counter

---

## Device Spoofing

50+ Android models are built-in:

| Brand | Models |
|-------|--------|
| **Xiaomi** | 2311DRK48I, 2306EPN60G, Redmi Note 14 Pro, Poco X7 Pro |
| **Samsung** | SM-S938B (S25 Ultra), SM-S928B, Galaxy S25 Ultra |
| **OnePlus** | CPH2581, CPH2609, OnePlus 13, 13R |
| **Oppo** | CPH2605, Find X8 Pro, Reno 20 Pro |
| **Vivo** | V2425, X200 Pro, Y300 Pro, iQOO 15 |
| **Realme** | RMX5000, GT 8 Pro, Narzo 80 Pro |
| **Honor** | Magic V4, 400 Pro, X50 GT |
| **Google** | Pixel 10 Pro, Pixel 9a |
| **Nothing** | Phone 3a, Phone 3, CMF Phone 2 |
| **Motorola** | Moto G Power 2026, Edge 60 Pro, Razr 60 Ultra |
| **Asus** | Zenfone 12 Ultra, ROG Phone 10 |

Customize via `HG_DEVICE_POOL`:

```bash
export HG_DEVICE_POOL="Xiaomi 2311DRK48I Android 16,Samsung SM-S938B Android 16,OnePlus 13 Android 16"
```

---

## Monitoring

### Health Endpoint

```
GET / → JSON
GET /health → JSON
```

Response:

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
  "proxies": { "total": 5, "healthy": 4, "dead": 1 },
  "details": [
    {"id":1, "device":"hgmain-1", "model":"Xiaomi 2311DRK48I Android 16", "state":"Connected", "errors":0, "overuses":0, "uptime_secs":3600}
  ]
}
```

### Instance States

| State | Action Taken |
|---|---|
| `Starting` | Proxy binding... |
| `Connecting` | Awaiting honeygain auth |
| `Connected` | ✅ Earning! |
| `Overused` | ⏸ Cooldown (configurable) |
| `AuthError` | ❌ Check credentials |
| `ProxyError` | 🔄 Circuit breaker |
| `ServerDown` | ⏳ Waiting for honeygain API |
| `Dead` | 🛑 Stop (max errors exceeded) |

---

## Docker (for Render only)

You don't need Docker locally. The Dockerfile is for Render's cloud builder:

```dockerfile
FROM rust:1.81-slim-bookworm AS builder
# ... builds hg-supervisor

FROM debian:bookworm-slim
# ... bundles honeygain binary + libs + supervisor
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --start-period=15s \
  CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health
```

---

## Legacy: Old rotate-proxy

The old iptables-based transparent proxy is in `../rust-proxy/`. It's **not needed** — the supervisor has proxy rotation built in. Kept for reference only.

---

## Files

| File | Description |
|------|-------------|
| `src/main.rs` | Complete supervisor (1348 lines Rust) |
| `Cargo.toml` | Dependencies (tokio, serde, chrono, rand, tracing) |
| `Cargo.lock` | Locked dependency versions |
| `hg-supervisor.toml` | Config template |
| `README.md` | This file |
| `target/release/hg-supervisor` | Compiled binary (4.2MB Windows, 2.5MB Linux) |
| `../Dockerfile` | Multi-stage build (Render) |
| `../render.yaml` | Render Blueprint |
