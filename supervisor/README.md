# 🚀 hg-supervisor v3.1 — Sticky Session + Multi-Account Edition

> **50+ honeygain instances, each with its own UNIQUE static IP via ProxyRise sticky sessions.**  
> Single 4.4MB Rust binary replaces Docker entirely. 1 container = 1 IP, 100% isolation.

---

## Features

| Feature | Details |
|---------|---------|
| 🚫 **No Docker** | Zero containers, zero daemon, zero iptables |
| 📱 **Device Spoofing** | 50+ Android models (Xiaomi, Samsung, OnePlus, etc.) |
| 🌐 **1 Instance = 1 IP** | Unique `res-{country}-sid-{N}` sticky session per instance |
| 🗺️ **40 Countries** | US, UK, DE, JP, CA, AU, FR... max IP diversity |
| 🔄 **Overuse Rotation** | "Network Overused" → kill → rotate SID → NEW IP |
| 👥 **Multi-Account** | `HG_ACCOUNTS="e1:p1,e2:p2"` — honeygain allows ~10 devices/account |
| 🩺 **Health Endpoint** | `GET /health` → JSON with per-instance IPs + isolation % |
| 🐌 **Staggered Startup** | 30s gap between instances |
| 🔧 **Render Ready** | Dockerfile + render.yaml for one-click deploy |

---

## Architecture

```
hg-supervisor (4.4MB)
├── Account Pool
│   ├── acct-1 → email1:pass1  (instances 1-10)
│   ├── acct-2 → email2:pass2  (instances 11-20)
│   └── acct-3 → email3:pass3  (instances 21-30)
│
├── Instance Manager (up to 50x)
│   ├── instance-1 → res-us-sid-123456  → US IP   → acct-1
│   ├── instance-2 → res-uk-sid-234567  → UK IP   → acct-1
│   ├── instance-3 → res-de-sid-345678  → DE IP   → acct-1
│   ├── instance-11 → res-jp-sid-456789 → JP IP   → acct-2
│   └── ...
│
├── Monitor
│   ├── stdout parser → state machine
│   ├── overuse detection → rotation signal
│   └── egress IP verification (ipquery.io, optional)
│
└── Health Server (:8080)
    └── GET /health → JSON (IPs, accounts, isolation %)
```

---

## Honeygain's 10-Device Rule (Important)

> **Honeygain allows ~10 devices per account on different networks.**  
> 50 instances on ONE account triggers "Network Overused" — regardless of IP isolation.
> **Solution: 50 instances = 5 accounts × 10 devices.**

Account assignment is round-robin: instances 1-10 → account 1, 11-20 → account 2, etc.
`MAX_DEVICES_PER_ACCOUNT` (default 10) enforces the cap; startup logs a warning if exceeded.

---

## Quick Start

### Prerequisites
- Rust toolchain (`cargo 1.97+`)
- Honeygain binary + `libhg.so.2.0.0` (in `../honeygain-binary/`)
- ProxyRise account with API key (sticky sessions)

### 1. Build

```bash
cd supervisor
cargo build --release
```

### 2. Configure

Via env vars (recommended for Render):

```bash
# Multi-account mode (recommended for 10+ instances)
export HG_ACCOUNTS="acct1@example.com:pass1,acct2@example.com:pass2,acct3@example.com:pass3"
export MAX_DEVICES_PER_ACCOUNT=10
export HG_INSTANCES=30

# Single-account mode (backward compatible; used if HG_ACCOUNTS empty)
export HG_EMAIL="your_email@example.com"
export HG_PASS="your_password"

# ProxyRise sticky sessions
export PROXYRISE_ENDPOINT="gw.proxyrise.com:443"
export PROXYRISE_API_KEY="pgw-your-api-key-here"
export PROXY_TYPE="res"          # res, stc, mob, dc
export VERIFY_IP="true"          # ipquery.io egress check (set false on 0.1-core)

export HG_LIB_DIR=../honeygain-binary/libs
```

Or via config file:

```toml
# hg-supervisor.toml
instances = 30
accounts = [
    { email = "acct1@example.com", pass = "pass1" },
    { email = "acct2@example.com", pass = "pass2" },
    { email = "acct3@example.com", pass = "pass3" },
]
max_devices_per_account = 10
proxyrise_endpoint = "gw.proxyrise.com:443"
proxyrise_api_key = "pgw-your-api-key-here"
proxy_type = "res"
verify_ip = true
overuse_cooldown_secs = 300
proxy_base_port = 9150
health_port = 8080
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
| `HG_ACCOUNTS` | — | Multi-account pool: `email1:pass1,email2:pass2` |
| `MAX_DEVICES_PER_ACCOUNT` | `10` | Max devices per honeygain account |
| `HG_EMAIL` | — | Single-account email (fallback if HG_ACCOUNTS empty) |
| `HG_PASS` | — | Single-account password (fallback) |
| `PROXYRISE_ENDPOINT` | — | ProxyRise gateway (host:port) |
| `PROXYRISE_API_KEY` | — | ProxyRise API key (pgw-...) |
| `PROXY_TYPE` | `res` | Proxy type: res, stc, mob, dc |
| `VERIFY_IP` | `true` | Verify egress IP via ipquery.io |
| `HG_DEVICE_POOL` | [50 built-in] | Custom Android model list |
| `HG_INSTANCES` | `1` | Number of instances |
| `TUNNEL_MAX_LIFETIME_SECS` | `86400` | Sticky session hold time (no periodic rotation) |
| `HG_PROXY_BASE_PORT` | `9150` | First proxy port |
| `HG_HEALTH_PORT` | `8080` | Health endpoint port |
| `PROXY_MAX_RETRIES` | `3` | Failures before proxy dead |
| `OVERUSE_COOLDOWN_SECS` | `300` | Cooldown after overuse |
| `HG_BIN_PATH` | `./honeygain` | Path to honeygain binary |
| `HG_LIB_DIR` | — | Path to libs directory |
| `RUST_LOG` | `info` | Log level |

---

## Sticky Sessions (1 Instance = 1 IP)

Each instance gets a unique ProxyRise sticky session:

```
res-{country}-sid-{N}
  ├── res-us-sid-123456  → static US IP
  ├── res-uk-sid-234567  → static UK IP
  ├── res-de-sid-345678  → static DE IP
  └── res-jp-sid-456789  → static JP IP
```

- **No two instances share an SID or egress IP** — 100% isolation
- Session is held until "Network Overused" is detected → kill → rotate SID → NEW IP
- SID range: 10000 - 999999999 (random, avoids collisions)
- Sessions stay alive as long as traffic flows

### Rate Limits & Errors

| Status | Meaning | Action |
|--------|---------|--------|
| `429` | Per-user cap / backconnect rate limit | Exponential backoff (250ms→8s) |
| `502/504` | Transient | Exponential backoff |
| `403` | Data exhausted / SSRF | Don't retry |
| `407` | Wrong credentials | Fix API key |

**Concurrent connections:** Unlimited by default on ProxyRise; per-user caps available on request. Since each instance uses its own `res-{country}-sid-{N}` username, each has its own per-username budget.

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

Response (v3.1):

```json
{
  "status": "ok",
  "timestamp": "2025-07-31 17:12:00",
  "instances": 10,
  "connected": 8,
  "starting": 1,
  "overused": 1,
  "errors": 0,
  "dead": 0,
  "accounts": 2,
  "max_devices_per_account": 10,
  "ip_isolation": "100.0%",
  "unique_ips": 8,
  "verified_instances": 8,
  "session_countries": 40,
  "details": [
    {"id":1, "device":"acct1-1", "model":"Xiaomi 2311DRK48I Android 16", "state":"Connected", "ip":"103.15.xx.xx", "session":"us-sid-123456", "account":"ac***@example.com", "errors":0, "overuses":0, "uptime_secs":3600}
  ]
}
```

> 🔒 Accounts are **masked** (`ac***@example.com`) — passwords are never exposed.

### Instance States

| State | Action Taken |
|---|---|
| `Starting` | Proxy binding... |
| `Connecting` | Awaiting honeygain auth |
| `Connected` | ✅ Earning! |
| `Overused` | ⏸ Cooldown → rotate SID → new IP |
| `AuthError` | ❌ Check credentials |
| `ProxyError` | 🔄 Retry / backoff |
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

## Files

| File | Description |
|------|-------------|
| `src/main.rs` | Complete supervisor (~1420 lines Rust) |
| `Cargo.toml` | Dependencies (tokio, serde, chrono, rand, tracing) |
| `Cargo.lock` | Locked dependency versions |
| `hg-supervisor.toml` | Config template (multi-account) |
| `README.md` | This file |
| `target/release/hg-supervisor` | Compiled binary (4.4MB Windows, ~2.5MB Linux) |
| `../Dockerfile` | Multi-stage build (Render) |
| `../render.yaml` | Render Blueprint (HG_ACCOUNTS secret) |

---

## Legacy: Old rotate-proxy

The old iptables-based transparent proxy is in `../rust-proxy/`. It's **not needed** — sticky sessions provide IP isolation natively. Kept for reference only.
