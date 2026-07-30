# 🍯 Honeygain Multi-Container Docker Setup

Run **up to 8+ Honeygain containers** simultaneously behind a rotating residential proxy to maximize earnings from a single account. Uses a custom Rust transparent proxy (`rotate-proxy`) that tunnels all traffic through ProxyRise residential IPs with automatic rotation.

---

## 📋 Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Usage](#usage)
  - [Deploy All Containers](#deploy-all-containers)
  - [Staggered Startup (Recommended)](#staggered-startup-recommended)
  - [Stop Everything](#stop-everything)
  - [View Logs](#view-logs)
  - [Monitor Traffic](#monitor-traffic)
- [How It Works](#how-it-works)
  - [The Proxy Layer](#the-proxy-layer)
  - [Why Multiple Containers Works](#why-multiple-containers-works)
  - [IP Rotation Mechanism](#ip-rotation-mechanism)
- [Customizing for More Containers](#customizing-for-more-containers)
- [Multiple Exit IPs (Advanced)](#multiple-exit-ips-advanced)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)
- [Files Explained](#files-explained)
- [License](#license)

---

## 🏗 Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                     Docker Host                           │
│                                                          │
│  ┌──────────────┐    ┌──────────────────┐               │
│  │ rotate-proxy │◄───│  honeygain-2..9  │  (7-8 cons)   │
│  │  (Rust TCP    │    │  network_mode:   │               │
│  │   proxy with  │    │  "service:proxy" │               │
│  │   iptables)   │    └──────────────────┘               │
│  └──────┬───────┘                                        │
│         │                                                │
│         ▼ socks5://res-any@...                           │
│  ┌────────────────┐                                      │
│  │   ProxyRise    │  (rotating residential IP pool)      │
│  │  gw.proxyrise  │                                      │
│  └────────────────┘                                      │
│                                                          │
│  ┌──────────────────┐                                    │
│  │ honeygain-2..9   │  all share same exit IP            │
│  │ (same IP)        │  → max 7-8 stable devices          │
│  └──────────────────┘                                    │
└─────────────────────────────────────────────────────────┘
         │
         ▼ api.honeygain.com
```

**Key components:**
- **`rotate-proxy`** — Rust-based transparent TCP proxy with iptables redirect. All outbound TCP from containers is intercepted via iptables and routed through the proxy, which forwards through ProxyRise residential SOCKS5.
- **Honeygain containers** — `honeygain/honeygain:latest` images with `network_mode: "service:rotate-proxy"` so they share the proxy's network namespace.
- **ProxyRise** — upstream residential proxy provider (`res-any` credentials) that assigns rotating exit IPs automatically.

---

## ✅ Prerequisites

| Requirement | Version/Details |
|-------------|-----------------|
| **Docker Desktop** | v24+ (with Docker Compose v2) |
| **ProxyRise account** | Active residential proxy credentials |
| **Honeygain account** | A registered Honeygain account |
| **RAM** | 4GB+ (each container ~200-300MB idle) |
| **Disk** | ~2GB for images + containers |
| **OS** | Windows (Docker Desktop), Linux, or macOS |

---

## ⚡ Quick Start

### 1. Clone the repository

```bash
git clone https://github.com/UNKNOWN052409/honey.git
cd honey
```

### 2. Configure your credentials

Edit `.env` with your actual credentials:

```env
HONEYGAIN_EMAIL=your_email@example.com
HONEYGAIN_PASS=your_honeygain_password
PROXY_URL=http://res-any:your_proxyrise_token@gw.proxyrise.com:443
```

> **Note:** All credentials are also hardcoded in `docker-compose.yml` for now — update both `.env` and `docker-compose.yml`.

### 3. Build the proxy image

```bash
docker compose build --no-cache
```

This builds the `honeygain-rotate-proxy:latest` image from `rust-proxy/`.

### 4. Start the proxy

```bash
docker compose up -d rotate-proxy
```

### 5. Deploy Honeygain containers (staggered)

```bash
# Start containers one by one with 30s delays to avoid "Network Overused"
docker compose up -d honeygain-2 && sleep 30
docker compose up -d honeygain-3 && sleep 30
docker compose up -d honeygain-4
# ... and so on
```

---

## 📝 Configuration

### `.env` file

| Variable | Description |
|----------|-------------|
| `HONEYGAIN_EMAIL` | Your Honeygain account email |
| `HONEYGAIN_PASS` | Your Honeygain account password |
| `PROXY_URL` | ProxyRise SOCKS5 URL with credentials |

### `docker-compose.yml` — Key settings to customize

**Tunnel rotation speed:**
```yaml
environment:
  - TUNNEL_MAX_LIFETIME_SECS=300   # How often the proxy gets a new IP (in seconds)
```
- **Lower values** (e.g. 30): faster IP rotation, helps avoid "Overused" but more reconnect overhead
- **Higher values** (e.g. 300): more stable connections, but same IP stays longer

**Port range for NAT (sysctls):**
```yaml
sysctls:
  - net.ipv4.ip_local_port_range=10000 65535  # More concurrent connections
  - net.ipv4.tcp_fin_timeout=10              # Faster port recycling
```

---

## 🚀 Usage

### Deploy All Containers

```bash
# Start proxy
docker compose up -d rotate-proxy

# Wait for proxy to initialize
sleep 15

# Start containers one by one with delays
docker compose up -d honeygain-2
sleep 30
docker compose up -d honeygain-3
sleep 30
docker compose up -d honeygain-4
sleep 30
docker compose up -d honeygain-5
sleep 30
docker compose up -d honeygain-6
sleep 30
docker compose up -d honeygain-7
sleep 30
docker compose up -d honeygain-8
```

### Staggered Startup (Recommended)

Use the included script approach:
```bash
# Staggered deploy helper
for i in 2 3 4 5 6 7 8; do
  echo "Starting honeygain-$i..."
  docker compose up -d honeygain-$i
  sleep 30
done
```

### Stop Everything

```bash
docker compose down
# Use --remove-orphans to clean up any leftover containers
docker compose down --remove-orphans
```

### View Logs

```bash
# All logs
docker compose logs -f

# Proxy only
docker compose logs -f rotate-proxy

# Specific container
docker compose logs -f honeygain-3

# Last 10 lines
docker compose logs --tail 10 honeygain-3
```

### Monitor Traffic

```bash
# Tunnels established (each = 1 upstream connection)
docker compose logs rotate-proxy | grep "TUNNEL via"

# Data transfer in last 60 seconds
docker compose logs --since=60s rotate-proxy | grep "bytes from client" | \
  awk '{sum+=$2} END {print sum " bytes / " NR " packets"}'

# Container status
docker compose ps --format "table {{.Name}}\t{{.Status}}"

# Auth status
for i in 2 3 4 5 6 7 8; do
  echo -n "honeygain-$i: "
  docker compose logs --tail 5 honeygain-$i 2>&1 | \
    grep -E "connected|Authorisation successful|Error|Overused" | tail -1
done
```

---

## 🔧 How It Works

### The Proxy Layer

The `rotate-proxy` service is a custom Rust binary that:

1. **Sets up iptables `REDSOCKS` chain** — intercepts ALL outbound TCP traffic from its network namespace (except to the upstream proxy IPs, DNS, and private ranges).
2. **Reads `SO_ORIGINAL_DST`** — uses Linux kernel's conntrack to get the original destination IP:port before iptables redirected it.
3. **Opens an upstream SOCKS5/HTTP tunnel** — connects to ProxyRise and requests a tunnel to the original destination.
4. **Relays data** — copies bytes bidirectionally between the local connection and the proxy tunnel.
5. **Rotates periodically** — closes the tunnel after `TUNNEL_MAX_LIFETIME_SECS` (default 300s), forcing a new upstream connection (and thus a new exit IP).

### Why Multiple Containers Works

Honeygain allows **multiple devices per account** but detects when too many devices share the same public IP → "Network Overused". The sweet spot is **5-8 containers per IP** with staggered startup:

- Staggered startup (30s delays) lets each container authenticate before the next one starts
- Each container registers as a unique device (`Pixel-9-Pro-2025-X`)
- The proxy's iptables redirect ensures ALL container traffic exits through the same tunnel → same IP

### IP Rotation Mechanism

```
Container → iptables REDIRECT → rotate-proxy (port 8080)
    → reads original dest → opens SOCKS5 via ProxyRise
    → ProxyRise assigns res-any IP → tunnel established
    → after 300s: tunnel closed → new tunnel → new IP
```

---

## 🎛 Customizing for More Containers

To add more containers:

1. Copy one of the existing honeygain blocks in `docker-compose.yml`:
```yaml
  honeygain-9:
    <<: *proxy-group
    container_name: honeygain-9
    command:
      - -email=your_email@example.com
      - -pass=your_password
      - -device=Pixel-9-Pro-2025-I
      - -tou-accept
```

2. Change `-device` to a unique name (each container must have a unique device name)
3. Run staggered with 30s delay

**Limits:**
- With **1 exit IP** → max **7-8 containers** (beyond that → "Network Overused")
- Traffic is **server-allocated** — actual earnings depend on Honeygain's demand in your region

---

## 🌐 Multiple Exit IPs (Advanced)

For **10+ containers** you need multiple exit IPs. Options:

### Option 1: Multiple Proxy Credentials
Run separate proxy containers, each with different credentials, and split containers across them:
```yaml
  rotate-proxy-1:
    environment:
      - UPSTREAM_PROXY_URL=socks5://credential1@...
  rotate-proxy-2:
    environment:
      - UPSTREAM_PROXY_URL=socks5://credential2@...
  
  honeygain-a:
    network_mode: "service:rotate-proxy-1"
  honeygain-b:
    network_mode: "service:rotate-proxy-2"
```

### Option 2: VPN Container
Add a WireGuard/OpenVPN container as a second network namespace:
```yaml
  wireguard-vpn:
    image: lscr.io/linuxserver/wireguard:latest
    cap_add:
      - NET_ADMIN
    networks:
      - honeygain-net
  
  honeygain-extra:
    network_mode: "service:wireguard-vpn"
```

### Option 3: Public SOCKS5 (Free)
Use free public SOCKS5 proxies as additional upstreams (low reliability but free).

> **⚠️ 10 Mbps is not feasible** with a single residential proxy credential. To achieve high bandwidth you need 3+ diverse exit IPs (different providers, different subnets) and significant Honeygain server demand in your IP regions.

---

## 🔍 Troubleshooting

### ❌ "Network Overused"

**Cause:** Too many containers sharing the same public IP.  
**Fix:**
- Reduce container count per IP (try 5 max)
- Add more exit IPs (see [Multiple Exit IPs](#multiple-exit-ips-advanced))
- Restart all containers: `docker compose down && docker compose up -d`

### ❌ "Error processing authorisation"

**Cause:** Honeygain rejected the IP address assigned to that container. Some residential proxy IPs are already flagged.  
**Fix:**
- Wait for the proxy to rotate IPs
- Try a different proxy geo (e.g. `res-us`, `res-gb`, `res-de`)
- Use `res-any` (auto-selects best available region)

### ❌ "context deadline exceeded" / API timeout

**Cause:** The proxy container's upstream connection is failing.
**Check:**
```bash
docker compose logs rotate-proxy | grep "all upstreams failed"
```
**Fixes:**
- ProxyRise credential may be rate-limited → wait 1-2 hours
- DNS resolution may fail inside container → use direct IP (`172.65.141.106`)
- Check if proxy port is correct (443 = TLS-wrapped SOCKS5)

### ❌ SOCKS5 "early eof" errors in proxy logs

**Cause:** ProxyRise uses TLS-wrapped SOCKS5 on port 443. Raw SOCKS5 bytes get rejected.  
**Fix:** Use both HTTP and SOCKS5 upstream URLs (the proxy tries both):
```yaml
- UPSTREAM_PROXY_URL=http://res-any:...@gw.proxyrise.com:443,socks5://res-any:...@gw.proxyrise.com:443
```

### ❌ "Cannot assign requested address (os error 99)"

**Cause:** Port exhaustion — the container ran out of ephemeral ports for new connections.  
**Fix:** The port range is already expanded in `sysctls:` section. If it persists, reduce `TUNNEL_MAX_LIFETIME_SECS` to close tunnels faster.

### ❌ Container keeps restarting

```bash
# Check container logs
docker compose logs --tail 20 honeygain-3
# Check proxy status
docker compose ps
```

---

## ❓ FAQ

### Is this against Honeygain's ToS?

Honeygain allows one account per user. Running multiple containers from a single account may violate their terms of service. This project is for **educational purposes only**. Use at your own risk.

### How much can I earn?

Earnings depend entirely on Honeygain's server demand in your assigned IP's region. Typical ranges:
- 1 container: ~$0.10-0.50/day
- 8 containers (same IP): ~$0.30-1.50/day
- Multiple IPs with high-demand regions: potentially $1-5/day  
**Not a replacement for income.**

### Why not just use `host` network mode?

Docker Desktop on Windows uses a VM network — the "host" IP seen by Honeygain is the VM's NAT IP, not your real home IP. Plus, your actual home IP may already have Honeygain devices registered (→ "Network Overused").

### ProxyRise credential stopped working

Rate limiting is common after many connection attempts. Solutions:
1. Wait 1-2 hours (auto-recovery)
2. Contact ProxyRise support
3. Get a different proxy provider

### Can I use other proxy providers?

Yes! Any SOCKS5 or HTTP CONNECT proxy works. Change `UPSTREAM_PROXY_URL` in the compose file to point to your provider.

---

## 📁 Files Explained

| File | Purpose |
|------|---------|
| `docker-compose.yml` | Main configuration — defines all services |
| `.env` | Credentials for Honeygain and ProxyRise |
| `rust-proxy/Dockerfile` | Multi-stage build for the Rust proxy binary |
| `rust-proxy/src/main.rs` | The Rust TCP proxy (iptables, SOCKS5, HTTP CONNECT, transparent mode) |
| `rust-proxy/entrypoint.sh` | Container entrypoint — sets up iptables, starts proxy |
| `rust-proxy/Cargo.toml` | Rust dependencies (tokio async runtime) |
| `iptables-init.sh` | Standalone iptables rules (legacy, not currently used) |

---

## 📜 License

MIT — This project is for educational purposes only. Use responsibly.

---

*Built with ❤️ and Rust 🦀*
