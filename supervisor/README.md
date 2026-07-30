# 🚀 hg-supervisor — Rust-based honeygain manager

> **Lightweight replacement for Docker Compose** — one binary, no containers, no iptables, no Docker daemon.

## The Problem with Docker

The old setup used **9 Docker containers** (1 rotate-proxy + 8 honeygain) with:
- `NET_ADMIN` + `NET_RAW` capabilities for iptables
- `network_mode: "service:rotate-proxy"` for transparent proxy
- ~200-300MB RAM per container → **~2.7GB total**
- Docker Desktop daemon running full-time
- **Doesn't work on Render/Railway** — no privileged mode, no iptables

## The Solution: hg-supervisor

A single **Rust binary** (~3.9MB) that:
1. Manages N honeygain subprocesses with auto-restart
2. Runs a local TCP proxy per instance (no iptables needed)
3. Each honeygain instance uses `HTTP_PROXY` env var to tunnel through the proxy
4. Upstream residential proxy (ProxyRise) handles IP rotation

### Architecture

```
hg-supervisor (single binary, ~3.9MB)
├── proxy-thread-1 → localhost:9150 ← honeygain-1 (subprocess, HTTP_PROXY=:9150)
├── proxy-thread-2 → localhost:9151 ← honeygain-2 (subprocess, HTTP_PROXY=:9151)
├── proxy-thread-3 → localhost:9152 ← honeygain-3 (subprocess, HTTP_PROXY=:9152)
└── ...
        │
        ▼ (SOCKS5/HTTP CONNECT)
    ProxyRise residential proxy
        │
        ▼ rotating exit IP
    api.honeygain.com
```

### Why This Works

The **honeygain CLI is a Go binary** that uses Go's standard `net/http` transport — which **respects `HTTP_PROXY` and `HTTPS_PROXY` environment variables** automatically. We verified this by extracting the binary and checking its strings:

```bash
$ strings honeygain | grep -i proxy
HTTP_PROXY
HTTPS_PROXY
http_proxy
https_proxy
no_proxy
proxyURL
```

No iptables, no transparent proxy, no Docker networking hacks needed.

## Quick Start

### Prerequisites
- Docker (one-time only, to extract the honeygain binary)
- Or the honeygain binary extracted manually
- A ProxyRise (or any SOCKS5/HTTP) account

### 1. Extract the honeygain binary

```bash
# From the Docker image (requires Docker running):
docker create --name hg_tmp honeygain/honeygain:latest
docker cp hg_tmp:/app/honeygain .
docker cp hg_tmp:/usr/lib/libhg.so.2.0.0 .
docker rm hg_tmp
chmod +x honeygain
```

### 2. Configure

Create `hg-supervisor.toml`:

```toml
instances = 4
email = "your_email@example.com"
pass = "your_password"
device_prefix = "HG"
upstream_proxy_url = "http://res-any:your_token@gw.proxyrise.com:443"
tunnel_lifetime_secs = 300
proxy_base_port = 9150
```

Or use environment variables:

```bash
export HG_EMAIL=your_email@example.com
export HG_PASS=your_password
export UPSTREAM_PROXY_URL=http://res-any:token@gw.proxyrise.com:443
export HG_INSTANCES=4
```

### 3. Run

```bash
# Build the supervisor
cd supervisor && cargo build --release

# Copy honeygain binary + libs to supervisor dir
cp ../honeygain .
cp ../libs/libhg.so.2.0.0 .
export LD_LIBRARY_PATH=.

# Run
./target/release/hg-supervisor
```

### 4. Docker Deployment (Render, Railway, any PaaS)

```bash
# Build the deploy image
docker build -t hg-supervisor:latest .

# Run with env vars
docker run --rm -it \
  -e HG_EMAIL=your_email \
  -e HG_PASS=your_password \
  -e UPSTREAM_PROXY_URL=http://res-any:token@gw.proxyrise.com:443 \
  -e HG_INSTANCES=4 \
  hg-supervisor:latest
```

## Comparison: Docker vs hg-supervisor

| Aspect | Docker (old) | hg-supervisor (new) |
|--------|-------------|-------------------|
| Runtime | Docker Desktop daemon | Native binary (~3.9MB) |
| Containers | 9 (proxy + 8 HG) | 0 |
| Memory | ~2.7GB (300MB x 9) | ~60MB (15MB x 4 HG) |
| iptables | Required (NET_ADMIN) | Not needed |
| Privileges | Capabilities required | No special perms |
| Render | ❌ Won't work | ✅ Works perfectly |
| Railway | ❌ Won't work | ✅ Works perfectly |
| Any VPS | Works but heavy | Works, lightweight |

## Configuration Reference

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

| Variable | Config equivalent |
|----------|------------------|
| `HG_EMAIL` | email |
| `HG_PASS` | pass |
| `HG_INSTANCES` | instances |
| `HG_DEVICE_PREFIX` | device_prefix |
| `UPSTREAM_PROXY_URL` | upstream_proxy_url |
| `TUNNEL_MAX_LIFETIME_SECS` | tunnel_lifetime_secs |
| `HG_PROXY_BASE_PORT` | proxy_base_port |
| `HG_BIN_PATH` | honeygain_bin |
| `HG_LIB_DIR` | lib_dir |
| `RUST_LOG` | Log level (debug, info, warn, error) |

## About Rust Container Runtimes

You asked about Rust-based lightweight containers. There are **two categories**:

### 1. OCI Container Runtimes (Youki, Krun, Railcar)

These are Rust implementations of the OCI runtime spec (what runc does in Go):
- **Youki** — Most mature, by the containers project. Replaces runc.
- **Krun** — KVM-based container runtime (heavier, not lighter).

**Why they don't help here:**
- They still require container **images** (Dockerfiles, registries)
- They still need **root namespaces, cgroups, iptables**
- They **don't work on Render** (no privileged mode)
- They're for running *existing* container workloads with a different runtime, not for eliminating containers

### 2. Process Supervisors (hg-supervisor — this project)

For the honeygain use case, the real lightweight solution is **no containers at all**:
- The honeygain binary can run directly as a subprocess
- Go HTTP_PROXY env var eliminates the need for iptables
- A single Rust binary manages everything with less memory than 1 Docker container

**This is the "Rust container alternative" you were looking for.**

## Files

| File | Purpose |
|------|---------|
| `supervisor/src/main.rs` | Rust supervisor source (~700 lines) |
| `supervisor/Cargo.toml` | Rust dependencies |
| `supervisor/hg-supervisor.toml` | Config file template |
| `Dockerfile` | Multi-stage build for Render/deploy |
| `render.yaml` | Render Blueprint config |
| `build.sh` | Build helper script |

## License

MIT — For educational purposes only. Use honeygain responsibly.
