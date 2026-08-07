# Deployment Guide

## Prerequisites

- Docker + Docker Compose v2
- ProxyRise API key with residential proxy access
- Pawns.app account (email + password)
- Honeygain account(s) (email + password)

## Quick Start (Docker Compose)

### 1. Set environment variables

```bash
# Required for both apps
export HG_EMAIL="your@email.com"
export HG_PASS="your_password"
export PAWNS_EMAIL="your_pawns@email.com"
export PAWNS_PASSWORD="your_pawns_password"

# Required for Pawns proxy routing
export PAWNS_PROXYRISE_PASSWORD="your_proxyrise_password"

# Optional (defaults shown)
export PAWNS_COUNTRY="us"
export PROXYRISE_ENDPOINT="gw.proxyrise.com:443"
```

### 2. Generate docker-compose.yml

```bash
# Default: 8 HG + 3 Pawns
python generate-compose.py > docker-compose.yml

# Custom scale
HG_INSTANCES=20 PAWNS_INSTANCES=10 python generate-compose.py > docker-compose.yml
```

### 3. Start

```bash
docker compose up -d
```

### 4. Monitor

```bash
# Dashboard
open http://localhost:9090

# Health endpoint
curl http://localhost:8080/health
```

## Render Deployment

### 1. Push to GitHub

```bash
git add .
git commit -m "Deploy hg-supervisor v4.0"
git push origin main
```

### 2. Create Render Blueprint

1. Go to Render Dashboard → New → Blueprint
2. Connect your GitHub repo
3. Render reads `render.yaml` automatically

### 3. Set secrets in Render

For the `hg-supervisor-1` service:
- `HG_ACCOUNTS` = `email1:pass1,email2:pass2,...`
- `PROXYRISE_API_KEY` = your API key

For the `pawns-supervisor` service:
- `PAWNS_EMAIL` = your Pawns email
- `PAWNS_PASSWORD` = your Pawns password

### 4. Deploy

Render auto-deploys on push.

## Linux Native Deployment

### 1. Build the supervisor

```bash
cd supervisor
cargo build --release
cp target/release/hg-supervisor /usr/local/bin/
```

### 2. Build the transparent proxy

```bash
cd rust-proxy
cargo build --release
cp target/release/rotate-proxy /usr/local/bin/
```

### 3. Install pawns-cli

```bash
wget https://pawns-app.s3.eu-central-1.amazonaws.com/cli/latest/linux_x86_64/pawns-cli
chmod +x pawns-cli
mv pawns-cli /usr/local/bin/
```

### 4. Configure

```bash
cp supervisor/hg-supervisor.toml /etc/hg-supervisor.toml
# Edit /etc/hg-supervisor.toml with your credentials
```

### 5. Run

```bash
# Start the supervisor (manages both HG and Pawns)
hg-supervisor
```

Note: Native deployment uses application-level proxy (SOCKS5/HTTP CONNECT) for Honeygain. For Pawns, you need the iptables transparent proxy setup from the Docker architecture.

## Scaling

See [SCALING.md](SCALING.md) for detailed scaling instructions.

### Quick reference

| Instances | Command |
|-----------|---------|
| 1 HG + 1 Pawns | `HG_INSTANCES=1 PAWNS_INSTANCES=1 python generate-compose.py > docker-compose.yml` |
| 10 HG + 10 Pawns | `HG_INSTANCES=10 PAWNS_INSTANCES=10 python generate-compose.py > docker-compose.yml` |
| 50 HG + 50 Pawns | `HG_INSTANCES=50 PAWNS_INSTANCES=50 python generate-compose.py > docker-compose.yml` |
| 100 HG + 100 Pawns | `HG_INSTANCES=100 PAWNS_INSTANCES=100 python generate-compose.py > docker-compose.yml` |

## Troubleshooting

### Pawns instances not getting unique IPs

Verify each proxy has a unique sticky session:
```bash
docker compose exec rotate-proxy-pawns-1 env | grep UPSTREAM_PROXY_URL
docker compose exec rotate-proxy-pawns-2 env | grep UPSTREAM_PROXY_URL
```

Each should show a different `sid-N` value.

### Health endpoint unreachable

Check the supervisor container is running:
```bash
docker compose ps
docker compose logs hg-supervisor
```

### Honeygain instances stuck in "starting"

Check the rotate-proxy is running and accessible:
```bash
docker compose logs rotate-proxy
curl -x http://localhost:8081 http://httpbin.org/ip
```

### Memory usage

Each proxy container uses ~10-20MB RAM. Each pawns-cli uses ~30-50MB. Each honeygain uses ~50-100MB.

For 100+100 instances, expect ~10-15GB total RAM.
