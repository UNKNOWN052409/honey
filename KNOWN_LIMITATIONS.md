# Known Limitations

## Architecture Limitations

### 1. Honeygain: Shared Proxy Namespace

All Honeygain instances share a single `rotate-proxy` container and its network namespace. IP diversity relies on ProxyRise sticky session credentials, not network isolation.

**Impact**: If the shared proxy crashes, all Honeygain instances lose connectivity simultaneously.

**Mitigation**: `restart: unless-stopped` policy + health monitoring.

**Future**: Could migrate to per-instance proxy model like Pawns (at higher resource cost).

### 2. Pawns: One Proxy Container Per Instance

Each Pawns instance requires a dedicated `rotate-proxy-pawns-N` container. This doubles the container count for Pawns.

**Impact**: 100 Pawns instances = 200 containers (100 proxies + 100 pawns-cli). Higher memory and orchestration overhead.

**Mitigation**: The iptables proxy is lightweight (~10-20MB RAM). Scale to 100 instances on a 16GB+ host.

### 3. No Application-Level Proxy in Pawns

pawns-cli cannot be configured with proxy settings. All routing must be transparent at the network layer.

**Impact**: Cannot use HTTP_PROXY/HTTPS_PROXY workarounds. Must rely on iptables or Docker network_mode.

**Mitigation**: The `rotate-proxy-pawns-N` + `network_mode: "service:rotate-proxy-pawns-N"` pattern handles this.

### 4. Pawns Output Parsing Is Incomplete

The `classify_pawns_output()` function is based on community-reported JSON output patterns. The full set of pawns-cli log events is not officially documented.

**Impact**: Some error states or status changes may not be detected. Instance may appear "connected" when it's actually in a degraded state.

**Mitigation**: Monitor `error_count` and `last_output` in the health endpoint. Manual log inspection for edge cases.

### 5. Health Endpoint Is Single-Threaded

The health server uses a simple `TcpListener` accept loop. Under heavy polling, it may become a bottleneck.

**Impact**: If monitoring tools poll `/health` every second across 300+ containers, response times may increase.

**Mitigation**: Health endpoint is read-only and fast. Poll at 15-30 second intervals, not every second.

## Deployment Limitations

### 6. Docker Required for Pawns Network Isolation

The per-instance proxy isolation for Pawns requires Docker's `network_mode: "service:..."`. Native Linux deployment cannot achieve the same isolation without manual iptables setup.

**Impact**: Pawns instances on bare metal require manual iptables configuration.

**Mitigation**: Use Docker for production deployments. Native deployment is experimental for Pawns.

### 7. Render Deployment Has Limitations

Render's Blueprint system does not support `network_mode: "service:..."`. The Pawns supervisor runs as a single binary managing all instances, which means all Pawns instances share the same network namespace on Render.

**Impact**: Pawns instances on Render do NOT get per-instance IP isolation. All share the same exit IP.

**Mitigation**: Use Docker Compose on a VPS (Hetzner, DigitalOcean, etc.) for full isolation. Render is suitable for Honeygain-only or low-isolation Pawns deployments.

### 8. No Built-In Bandwidth Monitoring

The supervisor tracks instance state and errors but does not monitor bandwidth consumption per instance.

**Impact**: Cannot detect instances consuming excessive bandwidth or identify underperforming instances.

**Mitigation**: Use external network monitoring (ntopng, iftop) or add bandwidth tracking in a future version.

### 9. ProxyRise Sticky Session Expiry

The `TUNNEL_MAX_LIFETIME_SECS` (default: 600s) forces tunnel rotation. If ProxyRise sessions expire faster than this, instances may get disconnected.

**Impact**: Instance may show "connected" but traffic fails after session expiry.

**Mitigation**: Set `TUNNEL_MAX_LIFETIME_SECS` lower than ProxyRise session timeout. Monitor health endpoint for error spikes.

### 10. No Graceful Drain on Shutdown

When `docker compose stop` is called, containers receive SIGTERM and are killed after the timeout. There is no graceful drain period for in-flight connections.

**Impact**: Active connections are dropped abruptly during shutdown.

**Mitigation**: For zero-downtime deployments, use rolling updates with `docker compose up -d --no-deps <service>`.

## Scalability Limitations

### 11. Docker Compose File Size at Scale

At 100+100 instances, the generated `docker-compose.yml` is 4,300+ lines. Docker Compose may take several seconds to parse.

**Impact**: `docker compose ps`, `docker compose up`, and other commands become slower.

**Mitigation**: Acceptable for static deployments. For dynamic scaling, consider Docker Swarm or Kubernetes.

### 12. Port Exhaustion

Each Pawns proxy exposes a unique host port starting from 8082. At 100 instances, this uses ports 8082-9181.

**Impact**: Port conflicts with other services. Ephemeral port range may be depleted.

**Mitigation**: The `net.ipv4.ip_local_port_range=10000 65535` sysctl expands the ephemeral range. Choose a non-conflicting base port.

### 13. No Auto-Scaling

The system does not auto-scale based on demand. Adding instances requires editing env vars and regenerating docker-compose.yml.

**Impact**: Manual intervention needed to scale up/down.

**Mitigation**: Automate with CI/CD pipeline that runs `generate-compose.py` and `docker compose up -d`.

## Security Limitations

### 14. Credentials in Environment Variables

Credentials (HG_EMAIL, PAWNS_PASSWORD, etc.) are stored in environment variables or `.env` file. This is standard Docker practice but may not meet all security requirements.

**Impact**: Credentials visible in `docker inspect`, process listings, and `.env` file.

**Mitigation**: Use Docker secrets or a secrets manager (Vault, AWS SSM) for production.

### 15. No TLS on Health Endpoint

The health endpoint serves plain HTTP. Anyone with network access can query instance status.

**Impact**: Information disclosure of instance counts, states, and error counts.

**Mitigation**: Restrict network access to the health port. Add TLS in a future version.

### 16. iptables Requires NET_ADMIN Capability

The `rotate-proxy` containers require `cap_add: NET_ADMIN` for iptables rules. This is a privileged capability.

**Impact**: Container can modify network configuration. Security risk in multi-tenant environments.

**Mitigation**: Acceptable in single-tenant Docker hosts. Use rootless Docker or Podman for better isolation.
