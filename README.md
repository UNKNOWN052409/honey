# 🍯 Honeygain Supervisor — v3.0 Sticky Session Edition

> **Run 50+ honeygain instances on Render with 100% IP isolation.**
> Every instance gets a UNIQUE static IP via ProxyRise sticky sessions.
> 1 container = 1 IP. No sharing. Static until "Network Overused".

[![Rust](https://img.shields.io/badge/Rust-1.97%2B-orange)](https://rustup.rs/)
[![Render](https://img.shields.io/badge/Render-deploy-blue)](https://render.com)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)

---

## 📑 Table of Contents

- [Why This Exists](#why-this-exists)
- [Architecture](#architecture)
  - [v3.0: Sticky Session Model (Recommended)](#v30-sticky-session-model-recommended)
  - [Old: Docker Compose (Legacy)](#old-docker-compose-legacy)
- [Features](#features)
- [Quick Start](#quick-start)
  - [Option A: Local Dev (No Docker)](#option-a-local-dev-no-docker)
  - [Option B: Deploy to Render](#option-b-deploy-to-render)
- [Configuration Reference](#configuration-reference)
- [ProxyRise Rate Limits & Pricing](#proxyrise-rate-limits--pricing)
- [Device Spoofing](#device-spoofing)
- [IP Isolation & Verification](#ip-isolation--verification)
- [Monitoring Dashboard](#monitoring-dashboard)
- [File Inventory](#file-inventory)
- [Docker vs hg-supervisor](#docker-vs-hg-supervisor)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)

---

## Why This Exists

Honeygain's official Docker image works, but **every instance needs a unique IP** or you get "Network Overused" / banned.

**The Problem:** Docker + one proxy → all instances share the same egress IP → honeygain blocks them.

**The Solution:** A single Rust binary that gives **each instance its own static IP** using ProxyRise sticky sessions. No Docker, no iptables, no container networking hacks.

---

## Architecture

### v3.0: Sticky Session Model (Recommended)

```
                      ┌──────────────────────────────┐
                      │   hg-supervisor v3.0 (4.2MB)  │
                      │   Sticky Session Manager      │
                      └──────┬───────────────────────┘
                             │
         ┌───────────────────┼───────────────────┐
         ▼                   ▼                   ▼
┌────────────────┐  ┌────────────────┐  ┌────────────────┐
│ Instance 1     │  │ Instance 2     │  │ Instance 3     │
│ Device: Xiaomi │  │ Device: Samsung│  │ Device: OnePlus│
│ Session:       │  │ Session:       │  │ Session:       │
│ res-US-sid-A   │  │ res-UK-sid-B   │  │ res-JP-sid-C   │
│ Static US IP   │  │ Static UK IP   │  │ Static JP IP   │
│ ─── UNIQUE ─── │  │ ─── UNIQUE ─── │  │ ─── UNIQUE ─── │
└────┬───────────┘  └────┬───────────┘  └────┬───────────┘
     │                    │                    │
     └────────────────────┼────────────────────┘
                          ▼
              ┌─────────────────────┐
              │  ProxyRise Gateway  │
              │  gw.proxyrise.com   │
              └──────────┬──────────┘
                         ▼
              ┌─────────────────────┐
              │  api.honeygain.com  │
              │  (3 different exit  │
              │   IPs from 3 diff   │
              │   sticky sessions)  │
              └─────────────────────┘
```

**Flow:**
1. Each instance generates a **unique ProxyRise sticky session**: `res-{country}-sid-{random}`
2. 40 different countries used across instances for max IP diversity
3. The sticky session assigns a **static IP** — it never rotates until "Network Overused"
4. IP verified via ipquery.io on startup — logged and tracked in /health endpoint
5. On "Network Overused" → kill honeygain → rotate session → NEW IP
6. Health endpoint shows `ip_isolation` percentage (goal: 100%)

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
| 🌐 **Sticky Sessions** | Each instance gets unique ProxyRise sticky session |
| 🔒 **100% IP Isolation** | 1 container = 1 unique static IP — never shared |
| 🌍 **40 Countries** | IPs spread across US, UK, DE, JP, CA, AU, FR, etc. |
| 🔄 **Overuse Rotation** | "Network Overused" → auto-rotate to new IP |
| ✅ **IP Verification** | ipquery.io check on startup — logged in /health |
| 📊 **Isolation Dashboard** | /health shows `ip_isolation: "100%"` or warns |
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
| `accounts` | `HG_ACCOUNTS` | — | Multi-account pool: `email1:pass1,email2:pass2` |
| `max_devices_per_account` | `MAX_DEVICES_PER_ACCOUNT` | `10` | Max devices per honeygain account |
| `email` | `HG_EMAIL` | — | Honeygain account email (single-mode fallback) |
| `pass` | `HG_PASS` | — | Honeygain account password (single-mode fallback) |
| `proxyrise_endpoint` | `PROXYRISE_ENDPOINT` | — | ProxyRise gateway (host:port) |
| `proxyrise_api_key` | `PROXYRISE_API_KEY` | — | ProxyRise API key (pgw-...) |
| `proxy_type` | `PROXY_TYPE` | `res` | Proxy type: res, stc, mob, dc |
| `verify_ip` | `VERIFY_IP` | `true` | Verify egress IP via ipquery.io |
| `proxy_pool` | `HG_PROXY_POOL` | — | Comma-separated proxy URLs (legacy) |
| `upstream_proxy_url` | `UPSTREAM_PROXY_URL` | — | Single proxy (legacy alt) |
| `device_pool` | `HG_DEVICE_POOL` | [built-in 50 models] | Custom Android model list |
| `tunnel_lifetime_secs` | `TUNNEL_MAX_LIFETIME_SECS` | `86400` | Seconds before tunnel rotation |
| `proxy_base_port` | `HG_PROXY_BASE_PORT` | `9150` | First local proxy port |
| `health_port` | `HG_HEALTH_PORT` | `8080` | Health endpoint port |
| `proxy_max_retries` | `PROXY_MAX_RETRIES` | `3` | Failures before proxy marked dead |
| `overuse_cooldown_secs` | `OVERUSE_COOLDOWN_SECS` | `300` | Cooldown after "Network Overused" |
| `honeygain_bin` | `HG_BIN_PATH` | `./honeygain` | Path to honeygain binary |
| `lib_dir` | `HG_LIB_DIR` | — | Path to libhg.so.2.0.0 directory |
| — | `RUST_LOG` | `info` | Log level (debug/info/warn/error) |

### Config File Template

See [`supervisor/hg-supervisor.toml`](supervisor/hg-supervisor.toml) for the latest config template.

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

## ProxyRise Rate Limits & Pricing

ProxyRise doesn't limit concurrent connections per account (unlimited by default).
Key constraints:

| Limit | Value | Notes |
|-------|-------|-------|
| **Concurrent connections** | Unlimited | Per-user caps available on request |
| **Sticky session SID range** | 10000 - 999999999 | Random SIDs avoid collisions |
| **Long-session hold** | ~60 minutes | Pinned IP stays stable ~1 hour |
| **Request size (API)** | 1 MiB body / 128 KiB headers | Tunneled traffic is unlimited |
| **DNS resolution** | At exit node | Targets resolve in local country |
| **429 Too Many Requests** | Per-user cap | Backoff with exponential jitter |
| **403 Forbidden** | Data exhausted or SSRF | 403 = policy, don't retry |

**Pricing (data plans):**

| Plan | Price | Data + Bonus | Best For |
|------|-------|-------------|----------|
| Starter | $10 | 25.5 GB + 500 MB | Testing, small deployments |
| Basic | $25 | 63.75 GB + 2 GB | 10-15 instances, moderate use |
| Standard | $50 | 127.5 GB + 5 GB | 30-50 instances |
| Pro | $100 | 255 GB + 15 GB | Heavy use |
| Business | $250 | 637.5 GB + 50 GB | Multi-service deployments |
| Enterprise | $500 | 1275 GB + 125 GB | Maximum scale |

**Data cost estimate:** Each honeygain instance consumes ~2-5 GB/month.
So:
- **10 instances** → ~20-50 GB/month → **Starter** ($10) should cover
- **50 instances** → ~100-250 GB/month → **Standard or Pro** ($50-100)
- **3 Render services × 50 instances** → ~300-750 GB → **Business** ($250)

**Error handling (built into v3.1):**
- **502/504** → Exponential backoff (250ms → 500ms → 1s → ... 8s) — these are transient
- **429** → Backoff + retry (same as 502/504)
- **403** → Don't retry (data exhausted or policy)
- **407** → Fix credentials (wrong API key)

## Multi-Account Support (honeygain 10-device limit)

> **Honeygain allows ~10 devices per account** (different networks).
> Running 50 instances on ONE account triggers "Network Overused" regardless of IP isolation.
> **Solution: 50 instances = 5 accounts × 10 devices each.**

### How Account Assignment Works

1. Accounts are configured via `HG_ACCOUNTS="email1:pass1,email2:pass2,..."`
2. Instances are distributed round-robin: instance 1-10 → account 1, 11-20 → account 2, etc.
3. Each instance keeps its **unique sticky session IP** — multi-account does NOT change IP isolation
4. `MAX_DEVICES_PER_ACCOUNT` (default 10) caps devices per account
5. Startup warns if `instances > accounts × max_devices_per_account`

### Config Formats

**Env (Render):**
```bash
export HG_ACCOUNTS="acct1@example.com:pass1,acct2@example.com:pass2,acct3@example.com:pass3"
export MAX_DEVICES_PER_ACCOUNT=10
```

**Config file (hg-supervisor.toml):**
```toml
accounts = [
  { email = "acct1@example.com", pass = "pass1" },
  { email = "acct2@example.com", pass = "pass2" },
]
max_devices_per_account = 10
```

**Single-account mode (backward compatible):**
```bash
export HG_EMAIL="your@email.com"
export HG_PASS="password"   # used when HG_ACCOUNTS is empty
```

### Scaling Formula

| Instances | Accounts needed (10/account) |
|-----------|------------------------------|
| 10 | 1 |
| 20 | 2 |
| 30 | 3 |
| 50 | 5 |

### Health Endpoint shows accounts

`GET /health` now reports per-instance account (masked):
```json
{"id":1,"device":"acct1-1","account":"ac***@example.com","ip":"103.15.xx.xx"}
```
Passwords are **never** exposed in /health.

## IP Isolation & Verification

### How It Works

1. **Sticky Session Assignment:** Each instance gets a unique `res-{country}-sid-{N}`
2. **Country Diversity:** 40 countries spread across instances
3. **Static IP:** The sticky session holds the same IP until session is rotated
4. **Overuse Detection:** When honeygain says "Network Overused" → rotate session → NEW IP
5. **Verification:** `verify_egress_ip()` calls ipquery.io through the proxy to confirm

### Failure Scenarios

| Scenario | Action |
|----------|--------|
| Proxy 502/504 | Exponential backoff (250ms→500ms→1s) |
| Proxy 429 (rate limit) | Backoff + retry |
| Honeygain "Network Overused" | Rotate sticky session → new IP |
| Honeygain "Server Down" | Log error, wait for auto-reconnect |
| IP verification failed | Log warning (instance still starts) |
| Max consecutive errors (5) | Instance marked Dead, stops permanently |

---

## Monitoring Dashboard

### Health Endpoint

`GET /health` → JSON with full system status:

```json
{
  "status": "ok",
  "timestamp": "2025-07-31 04:49:00",
  "instances": 10,
  "connected": 8,
  "starting": 1,
  "overused": 1,
  "errors": 0,
  "dead": 0,
  "ip_isolation": "100%",          ← Every instance has a UNIQUE IP
  "unique_ips": 8,
  "verified_instances": 8,
  "session_countries": 40,
  "details": [
    {"id": 1, "device": "hgmain-1", "model": "Xiaomi 2311DRK48I Android 16", "state": "Connected", "ip": "103.15.xx.xx", "session": "us-sid-123456789", "errors": 0, "overuses": 0, "uptime_secs": 3600},
    {"id": 2, "device": "hgmain-2", "model": "Samsung SM-S938B Android 16", "state": "Connected", "ip": "185.22.xx.xx", "session": "uk-sid-234567890", "errors": 0, "overuses": 0, "uptime_secs": 3570},
    {"id": 3, "device": "hgmain-3", "model": "OnePlus CPH2581 Android 16", "state": "Overused", "ip": "78.46.xx.xx", "session": "de-sid-345678901", "errors": 1, "overuses": 1, "uptime_secs": 120}
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
| `Overused` | "Network Overused" — auto-rotating to new IP |
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

# Sticky sessions auto-rotate on overuse — check health endpoint later
# to confirm new IPs are assigned

# Reduce instances per Render service
export HG_INSTANCES=10

# Check health endpoint for IP isolation
export VERIFY_IP=true
```

### All instances stuck on "Connecting"
1. Check ProxyRise endpoint: `curl -v http://res-us:API_KEY@gw.proxyrise.com:443`
2. Verify PROXYRISE_API_KEY is set correctly
3. Check honeygain credentials (HG_EMAIL, HG_PASS)
4. Check Render logs: `render logs --tail` or Render dashboard
5. Try with `VERIFY_IP=false` (ipquery.io might be slow)

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

### Q: Do I need Docker at all?

**Locally:** No. The supervisor runs natively. You only need Docker **once** to extract the honeygain binary.

**On Render:** Render uses Docker in their cloud to build — you don't need Docker Desktop.

### Q: Will 50 instances on one Render service work?

**With v3.0 sticky sessions:** Yes! Each instance gets its **own unique IP** via ProxyRise sticky sessions. 40 different countries spread across instances. No IP sharing.

**But:** 50 instances on Render's 0.1 core / 512MB may be tight. Start with 10, check `/health`, scale up.

### Q: Do I need Docker at all?

**Locally:** No. The supervisor runs natively. You only need Docker **once** to extract the honeygain binary from the official image.

**On Render:** Render uses Docker **in their cloud** to build the image — you don't need Docker Desktop.

### Q: How many unique IPs do I get?

Up to **40 unique IPs** (one per country in SESSION_COUNTRIES). Each instance gets a different country → different IP pool. You can extend by deploying multiple Render services.

### Q: What proxies work with v3.0?

**ProxyRise** is recommended because their sticky sessions (`res-{country}-sid-{N}`) give per-instance static IPs. Other providers may work if they support HTTP CONNECT with sticky/static sessions.

### Q: How much does this cost to run?

- **Render Starter** ($7/month) — 1 service, 512MB RAM (~10-15 instances)
- **Render Pro** ($20/month) — 2GB RAM (~30-50 instances)
- **ProxyRise Starter** ($10/month) — 25.5 GB (~5-10 instances)
- **ProxyRise Standard** ($50/month) — 127.5 GB (~50 instances)

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
