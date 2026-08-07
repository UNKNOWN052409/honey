# Final Project Tree

```
honey/
├── docker-compose.yml              # Full stack: HG + Pawns, per-instance isolation
├── Dockerfile                      # Honeygain supervisor (Rust + honeygain binary)
├── Dockerfile.pawns                # Pawns supervisor (Rust + pawns-cli binary)
├── generate-compose.py             # Auto-generate docker-compose for any scale
├── render.yaml                     # Render Blueprint (HG + Pawns services)
├── README.md                       # Project overview
├── .env                            # Environment variables (secrets)
│
├── supervisor/                     # hg-supervisor v4.0 — Rust binary
│   ├── Cargo.toml                  # Dependencies
│   ├── Cargo.lock                  # Locked versions
│   ├── hg-supervisor.toml          # Default config (dual-app)
│   ├── README.md                   # Supervisor documentation
│   └── src/
│       ├── main.rs                 # Entry point: wires HG + Pawns (247 lines)
│       ├── config.rs               # Config structs, load_config, env overrides
│       ├── constants.rs            # ANDROID_MODELS, SESSION_COUNTRIES
│       ├── instance.rs             # InstanceState, InstanceInfo, HgAppState, PawnsAppState
│       ├── session.rs              # StickySession, SessionManager (HG only)
│       ├── proxy.rs                # SOCKS5/HTTP CONNECT, ExponentialBackoff
│       ├── process_common.rs       # Generic monitor_stdout/Stderr
│       ├── hg_process.rs           # Honeygain spawn, proxy server, instance manager
│       ├── pawns_process.rs        # Pawns spawn, JSON log classifier, instance manager
│       └── health.rs              # Unified health endpoint (both apps)
│
├── rust-proxy/                     # rotate-proxy — iptables transparent proxy
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── Dockerfile
│   ├── Dockerfile.tmp
│   ├── entrypoint.sh               # iptables REDIRECT rules + binary launch
│   ├── entrypoint.sh.bak
│   └── src/
│       └── main.rs                 # Transparent proxy (SO_ORIGINAL_DST) + upstream tunnels
│
└── monitor/
    └── index.html                  # Nginx dashboard (port 9090)
```

## Key Files by Purpose

| Purpose | File | Lines |
|---------|------|-------|
| Entry point | `supervisor/src/main.rs` | 247 |
| Config loading | `supervisor/src/config.rs` | 364 |
| Honeygain process | `supervisor/src/hg_process.rs` | 499 |
| Pawns process | `supervisor/src/pawns_process.rs` | 258 |
| Health endpoint | `supervisor/src/health.rs` | 193 |
| Proxy client | `supervisor/src/proxy.rs` | 410 |
| Session management | `supervisor/src/session.rs` | 180 |
| Transparent proxy | `rust-proxy/src/main.rs` | 1370 |
| Compose generator | `generate-compose.py` | 163 |
| Docker stack | `docker-compose.yml` | 240 |
