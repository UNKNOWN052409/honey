# 🍯 Honey Multi-Instance Suite

Two tools, one goal: run many bandwidth-sharing instances with **unique
residential IPs** and **zero cross-talk**.

```
┌─────────────────────────────────────────────────────────┐
│  resibox/  ← ★ CURRENT — Rust isolated container runtime │
│  strict per-container jails, fail-closed, auto-healing   │
└─────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────┐
│  supervisor/  ← LEGACY — sticky-session process manager  │
└─────────────────────────────────────────────────────────┘
```

## 📦 resibox — Rust-Based Isolated Residential IP Container Runtime

One container → one assigned residential proxy → one isolated egress path.
Honeygain + Pawns.app run inside each jail.

- Per-container network namespace + iptables egress jail (root mode)
- Transparent TCP forwarder → SOCKS5 / HTTP-CONNECT assigned proxy
- DNS-over-proxy relay, pre-flight IP verification, host-leak detector
- Watchdog: kill → revalidate → resume only after clean pass
- Network-Overused auto-bypass + persistent device identities
- Sticky-session pinning (keep-alive heartbeat)
- Userspace mode for rootless hosts (Render / Termux / 0.1-core)

**Docs:** [`resibox/README.md`](./resibox/README.md) • Scaling:
[`resibox/SCALING.md`](./resibox/SCALING.md) • Bulk config:
[`resibox/gen-config.py`](./resibox/gen-config.py)

```bash
cd resibox
cargo build --release
cp config.example.toml config.toml    # apne proxies/accounts daalo
./target/release/resibox config.toml
curl localhost:8080/health            # live isolation dashboard
```

## 🧰 supervisor — v3.0 Sticky Session Edition (legacy)

Single Rust binary managing N honeygain processes via ProxyRise sticky
sessions, device spoofing, health endpoint. Superseded by resibox but kept
for Render deployments (`Dockerfile` + `render.yaml`).

```bash
cd supervisor && cargo build --release
HG_EMAIL=you@example.com HG_PASS=secret HG_INSTANCES=4 \
PROXYRISE_ENDPOINT=gw.proxyrise.com:443 PROXYRISE_API_KEY=pgw-xxx \
./target/release/hg-supervisor
```

Config reference: `supervisor/hg-supervisor.toml` • Monitor page: `monitor/`

## 📊 Which one to use?

| | resibox | supervisor |
|---|---|---|
| Isolation | kernel netns jail / verified userspace | proxy-level only |
| Pawns.app support | ✅ alongside honeygain | ❌ honeygain only |
| Fail-closed watchdog | ✅ full state machine | partial |
| Overused auto-bypass | ✅ rotate + re-preflight | manual-ish |
| Bulk scaling kit | ✅ gen-config.py + SCALING.md | ❌ |
| Rootless (Render) | ✅ userspace mode | ✅ |

**New deployments → use resibox.**

## 🔐 Credentials

`.env`, `accounts.txt`, `proxies.txt` are gitignored — create them locally.
Never commit real credentials; if leaked, rotate keys immediately.

## License

MIT — do whatever you want, but no warranty.
