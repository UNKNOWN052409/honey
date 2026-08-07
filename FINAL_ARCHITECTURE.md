# Final Architecture — hg-supervisor v4.0

## Overview

Single Rust binary (`hg-supervisor`) manages both Honeygain and Pawns.app simultaneously. Each application gets network-level IP isolation through dedicated iptables transparent proxy containers.

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Docker Host                                  │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  monitor (nginx:alpine)                                       │  │
│  │  Port 9090 → dashboard                                        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  rotate-proxy (iptables transparent proxy)                    │  │
│  │  Shared by all Honeygain instances                            │  │
│  │  iptables REDIRECT → port 8080 → ProxyRise upstream           │  │
│  └───────────────────────────────────────────────────────────────┘  │
│       │ network_mode: service:rotate-proxy                          │
│       ├── hg-1 (honeygain/honeygain:latest)                        │
│       ├── hg-2                                                      │
│       └── hg-N                                                      │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  rotate-proxy-pawns-1 (dedicated)                             │  │
│  │  UPSTREAM_PROXY_URL=socks5://res-us-sid-1:pass@gw:443        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│       │ network_mode: service:rotate-proxy-pawns-1                  │
│       └── pawns-1 (iproyal/pawns-cli:latest)                       │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  rotate-proxy-pawns-2 (dedicated)                             │  │
│  │  UPSTREAM_PROXY_URL=socks5://res-us-sid-2:pass@gw:443        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│       │ network_mode: service:rotate-proxy-pawns-2                  │
│       └── pawns-2                                                   │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  rotate-proxy-pawns-N (dedicated)                             │  │
│  │  UPSTREAM_PROXY_URL=socks5://res-us-sid-N:pass@gw:443        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│       │ network_mode: service:rotate-proxy-pawns-N                  │
│       └── pawns-N                                                   │
└─────────────────────────────────────────────────────────────────────┘
```

## Traffic Flow

### Honeygain

```
honeygain binary
    │
    │ (application-level HTTP_PROXY or QUIC)
    ▼
iptables REDIRECT (rotate-proxy container)
    │
    │ getsockopt(SOL_ORIGINAL_DST) → real destination
    ▼
Rust proxy binary (port 8080)
    │
    │ SOCKS5/HTTP CONNECT with res-{country}-sid-{N} credentials
    ▼
ProxyRise upstream → Residential exit IP
```

### Pawns.app

```
pawns-cli binary (completely proxy-unaware)
    │
    │ plain TCP (no HTTP_PROXY, no proxy flags)
    ▼
iptables REDIRECT (rotate-proxy-pawns-N container)
    │
    │ getsockopt(SOL_ORIGINAL_DST) → real destination
    ▼
Rust proxy binary (port 8080)
    │
    │ SOCKS5 with res-{country}-sid-{N} credentials
    ▼
ProxyRise upstream → Unique residential exit IP
```

## Network Isolation Model

| Application | Proxy Container | Network Namespace | Exit IP |
|-------------|----------------|-------------------|---------|
| Honeygain (all instances) | rotate-proxy (shared) | Shared | Same ProxyRise session |
| Pawns-1 | rotate-proxy-pawns-1 | Isolated | res-us-sid-1 |
| Pawns-2 | rotate-proxy-pawns-2 | Isolated | res-us-sid-2 |
| Pawns-N | rotate-proxy-pawns-N | Isolated | res-us-sid-N |

## Module Structure

| Module | Responsibility |
|--------|---------------|
| `main.rs` | Entry point, wires both apps, spawns instance managers |
| `config.rs` | TOML parsing, env var overrides, legacy format support |
| `constants.rs` | Android model list, session country codes |
| `instance.rs` | State machine, per-instance data, app state containers |
| `session.rs` | ProxyRise sticky session generation (Honeygain only) |
| `proxy.rs` | SOCKS5/HTTP CONNECT client, exponential backoff, IP verification |
| `process_common.rs` | Generic stdout/stderr line monitoring |
| `hg_process.rs` | Honeygain spawn, per-instance proxy server, instance lifecycle |
| `pawns_process.rs` | Pawns spawn, JSON log classifier, instance lifecycle |
| `health.rs` | HTTP health endpoint reporting both apps |

## Key Design Decisions

1. **Single binary, dual app**: One Rust binary manages both Honeygain and Pawns. Backward-compatible with existing Honeygain-only configs.

2. **Per-instance proxy isolation for Pawns**: Each Pawns instance gets a dedicated `rotate-proxy-pawns-N` container with its own ProxyRise sticky session. This ensures unique residential exit IPs without any application-level proxy configuration in pawns-cli.

3. **Shared proxy for Honeygain**: All Honeygain instances share a single `rotate-proxy` container. IP diversity comes from per-instance sticky session credentials, not from separate containers.

4. **Transparent proxy via iptables**: The `rotate-proxy` binary uses `getsockopt(SOL_ORIGINAL_DST)` to recover the real destination after iptables REDIRECT. Applications are completely unaware of the proxy layer.

5. **No application-level proxy in Pawns**: pawns-cli has no HTTP_PROXY, no proxy flags, no proxy env vars. All routing is transparent at the container/network layer.

## Health Endpoint

`GET /health` returns JSON:

```json
{
  "status": "ok",
  "timestamp": "2026-08-07 12:00:00",
  "honeygain": {
    "enabled": true,
    "instances": 50,
    "connected": 45,
    "starting": 3,
    "overused": 1,
    "errors": 1,
    "dead": 0,
    "ip_isolation": "98.0%",
    "unique_ips": 49,
    "verified_instances": 50,
    "session_countries": 40
  },
  "pawns": {
    "enabled": true,
    "instances": 10,
    "connected": 9,
    "starting": 1,
    "errors": 0,
    "dead": 0
  },
  "details": [...]
}
```
