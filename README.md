# 🍯 hg-supervisor — Run Honeygain Without Docker

> **Single Rust binary** replaces the entire Docker Compose stack.
> No Docker, no containers, no iptables — works on Render, Railway, any VPS.

---

## 🏗 Architecture (New)

```
hg-supervisor (3.8MB Rust binary)
├── proxy:9150 ← honeygain-1 (HTTP_PROXY=:9150, device: HG-1)
├── proxy:9151 ← honeygain-2 (HTTP_PROXY=:9151, device: HG-2)
├── proxy:9152 ← honeygain-3 (HTTP_PROXY=:9152, device: HG-3)
└── proxy:9153 ← honeygain-4 (HTTP_PROXY=:9153, device: HG-4)
        │
        ▼ (SOCKS5 or HTTP CONNECT)
    ProxyRise Residential Proxy
        │
        ▼ rotating exit IP
    api.honeygain.com
```

**Before (Docker):** 9 containers + iptables + NET_ADMIN + ~2.7GB RAM  
**After (hg-supervisor):** 1 binary + 4 subprocesses + ~60MB RAM + **zero privileges**

### Why This Works

The honeygain CLI is a **Go binary** that uses Go's `net/http` transport — it **respects `HTTP_PROXY` and `HTTPS_PROXY` environment variables** automatically. No iptables, no transparent proxy, no Docker networking hacks needed.

---

## ✅ Prerequisites

- The **honeygain binary** and **libhg.so.2.0.0** (pre-extracted from Docker image)
- A ProxyRise (or any SOCKS5/HTTP) account
- Rust toolchain (`cargo 1.97+`) — for local development

---

## 🚀 Quick Start

### 1. Prepare the honeygain binary

```bash
# If you have Docker (one-time):
docker create --name hg_tmp honeygain/honeygain:latest
docker cp hg_tmp:/app/honeygain .
docker cp hg_tmp:/usr/lib/libhg.so.2.0.0 .
docker rm hg_tmp
chmod +x honeygain
```

### 2. Configure

Create `supervisor/hg-supervisor.toml`:

```toml
instances = 4
email = "hgmain.fuldgu@proton.me"
pass = "your_password"
device_prefix = "HG"
upstream_proxy_url = "http://res-any:your_token@gw.proxyrise.com:443"
tunnel_lifetime_secs = 300
proxy_base_port = 9150
honeygain_bin = "./honeygain"
lib_dir = "./libs"
```

Or via environment variables (for Render):

```bash
export HG_EMAIL=your_email
export HG_PASS=your_password
export UPSTREAM_PROXY_URL=http://res-any:token@gw.proxyrise.com:443
export HG_INSTANCES=4
export HG_LIB_DIR=./libs
```

### 3. Build & Run

```bash
cd supervisor
cargo build --release

# Run (make sure honeygain binary + libs are accessible):
cp ../honeygain-binary/honeygain .
cp -r ../honeygain-binary/libs .
export LD_LIBRARY_PATH=./libs
./target/release/hg-supervisor
```

---

## ☁️ Deploy to Render

Render builds and deploys directly from Git — no Docker Desktop needed.

### One-Click Deploy

1. Push this repo to GitHub
2. In Render Dashboard → **New → Blueprint**
3. Connect your repo
4. Set environment secrets (not in repo):
   - `HG_EMAIL`
   - `HG_PASS`
   - `UPSTREAM_PROXY_URL`
5. Deploy ✅

### Manual Deploy

Or create a Web Service on Render:

| Setting | Value |
|---------|-------|
| **Type** | Web Service |
| **Environment** | Docker |
| **Dockerfile Path** | `./Dockerfile` |
| **Build Command** | (auto — multi-stage Dockerfile) |
| **Start Command** | (auto — CMD in Dockerfile) |

**Environment Variables (secret):**

| Variable | Example |
|----------|---------|
| `HG_EMAIL` | your@email.com |
| `HG_PASS` | your_password |
| `UPSTREAM_PROXY_URL` | `http://res-any:token@gw.proxyrise.com:443` |
| `HG_INSTANCES` | `4` |
| `RUST_LOG` | `info` |

---

## 🆚 Docker vs hg-supervisor

| Aspect | Docker (old) | hg-supervisor (new) |
|--------|-------------|-------------------|
| Runtime | Docker Desktop (2GB+) | Native binary (3.8MB) |
| Infrastructure | 9 containers | 0 containers |
| RAM | ~2.7GB | ~60MB |
| CPU overhead | Heavy (WSL2 VM) | Minimal |
| iptables/NET_ADMIN | Required | Not needed |
| Root/Sudo | Required | Not needed |
| Render support | ❌ Won't work | ✅ Works |
| Startup time | Minutes | Instant |

---

## ⚙️ Configuration Reference

### Config File (`hg-supervisor.toml`)

| Field | Default | Description |
|-------|---------|-------------|
| `instances` | `1` | Number of honeygain instances |
| `email` | — | Honeygain account email |
| `pass` | — | Honeygain account password |
| `device_prefix` | `"HG"` | Device name prefix (appends -1, -2, etc.) |
| `upstream_proxy_url` | — | ProxyRise or any SOCKS5/HTTP proxy URL |
| `tunnel_lifetime_secs` | `300` | Seconds before tunnel rotation |
| `proxy_base_port` | `9150` | First local proxy port |
| `honeygain_bin` | `./honeygain` | Path to honeygain binary |
| `lib_dir` | `None` | Path to libs (libhg.so.2.0.0) |

### Environment Variables

| Variable | Config Field | Required |
|----------|-------------|----------|
| `HG_EMAIL` | email | ✅ |
| `HG_PASS` | pass | ✅ |
| `UPSTREAM_PROXY_URL` | upstream_proxy_url | ✅ |
| `HG_INSTANCES` | instances | ❌ (default 1) |
| `HG_DEVICE_PREFIX` | device_prefix | ❌ (default "HG") |
| `TUNNEL_MAX_LIFETIME_SECS` | tunnel_lifetime_secs | ❌ (default 300) |
| `HG_PROXY_BASE_PORT` | proxy_base_port | ❌ (default 9150) |
| `HG_BIN_PATH` | honeygain_bin | ❌ (default ./honeygain) |
| `HG_LIB_DIR` | lib_dir | ❌ (default None) |
| `RUST_LOG` | — | ❌ (default "info") |

---

## 📁 Files

| File | Purpose |
|------|---------|
| `supervisor/src/main.rs` | Rust supervisor (~700 lines) |
| `supervisor/Cargo.toml` | Rust dependencies |
| `supervisor/hg-supervisor.toml` | Config template |
| `Dockerfile` | Multi-stage build for Render |
| `render.yaml` | Render Blueprint |
| `honeygain-binary/honeygain` | Extracted Go binary |
| `honeygain-binary/libs/libhg.so.2.0.0` | Required shared library |

---

## 🔧 Troubleshooting

### "Cannot find libhg.so.2.0.0"

```bash
# Set LD_LIBRARY_PATH correctly:
export LD_LIBRARY_PATH=$PWD/libs
./target/release/hg-supervisor
```

### "Network Overused"

Reduce `instances` to 4-5. Or wait 30 minutes for honeygain to cool down.

### Supervisor keeps spawning and crashing

Check `RUST_LOG=debug` for detailed logs.

---

## 📜 License

MIT — Educational purposes only. Use honeygain responsibly.
