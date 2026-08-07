# Changelog

## v4.0.0 — Dual-App Edition (2026-08-07)

### Added

- **Pawns.app support**: Single binary manages both Honeygain and Pawns.app simultaneously
- **Per-instance network isolation for Pawns**: Each Pawns instance gets a dedicated `rotate-proxy-pawns-N` container with its own ProxyRise sticky session and unique residential exit IP
- **Unified health endpoint**: `GET /health` reports status for both Honeygain and Pawns instances
- **Auto-scaling compose generator**: `generate-compose.py` generates complete docker-compose.yml for any number of instances
- **Module refactoring**: Monolithic 3100-line `main.rs` split into 10 focused modules:
  - `config.rs` — Config structs, load_config(), env overrides, legacy format
  - `constants.rs` — ANDROID_MODELS, SESSION_COUNTRIES
  - `instance.rs` — InstanceState, InstanceInfo, HgAppState, PawnsAppState
  - `session.rs` — StickySession, SessionManager (Honeygain only)
  - `proxy.rs` — SOCKS5/HTTP CONNECT, ExponentialBackoff, IP verification
  - `process_common.rs` — Generic monitor_stdout/Stderr
  - `hg_process.rs` — Honeygain process management
  - `pawns_process.rs` — Pawns process management
  - `health.rs` — Health endpoint
  - `main.rs` — Slim entry point (247 lines)

### Architecture

- **Honeygain**: Shared `rotate-proxy` container with iptables transparent proxy. All instances share network namespace. IP diversity from per-instance sticky session credentials.
- **Pawns**: Dedicated `rotate-proxy-pawns-N` per instance. Each Pawns instance has isolated network namespace. pawns-cli is completely proxy-unaware — no HTTP_PROXY, no proxy flags, no application-level config. All routing is transparent via iptables.
- **Network isolation**: Complete isolation between every Pawns instance and between Pawns and Honeygain.

### Configuration

- New `[pawns]` section in `hg-supervisor.toml`
- Backward-compatible with existing flat-format Honeygain configs
- Environment variable overrides: `PAWNS_INSTANCES`, `PAWNS_EMAIL`, `PAWNS_PASSWORD`, `PAWNS_COUNTRY`

### Deployment

- `Dockerfile.pawns` — Pawns supervisor image (Rust + pawns-cli)
- `docker-compose.yml` — Full stack with per-instance proxy isolation
- `render.yaml` — Render Blueprint for both apps
- `generate-compose.py` — Auto-generate compose for any scale

### Verified

- `cargo fmt` — clean
- `cargo clippy -- -D warnings` — zero warnings
- `cargo check` — passes
- `cargo build --release` — builds successfully
- YAML validation at 1, 10, 50, 100 instances — all pass
- Unique sticky sessions verified at all scales
- network_mode 1:1 mapping verified

## v3.0.0 — Sticky Session Edition (previous)

- Application-level proxy per Honeygain instance
- ProxyRise sticky sessions with unique IPs
- Per-instance proxy servers (SOCKS5/HTTP CONNECT)
- Overuse detection and auto-rotation
- Health endpoint

## v2.0.0

- Multi-account support
- IP verification via ipquery.io

## v1.0.0

- Initial release
- Single Honeygain instance management
