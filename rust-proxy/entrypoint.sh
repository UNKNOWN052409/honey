#!/bin/sh
# entrypoint.sh — Setup iptables transparent proxy, then run the Rust proxy
# Dynamically extracts all upstream proxy IPs from env var to avoid loops

echo "[INIT] Setting up iptables transparent proxy rules..."

# Clean up any existing rules
iptables -t nat -F REDSOCKS 2>/dev/null || true
iptables -t nat -X REDSOCKS 2>/dev/null || true

iptables -t nat -N REDSOCKS

# Skip private / reserved ranges
iptables -t nat -A REDSOCKS -d 0.0.0.0/8 -j RETURN
iptables -t nat -A REDSOCKS -d 10.0.0.0/8 -j RETURN
iptables -t nat -A REDSOCKS -d 127.0.0.0/8 -j RETURN
iptables -t nat -A REDSOCKS -d 169.254.0.0/16 -j RETURN
iptables -t nat -A REDSOCKS -d 172.16.0.0/12 -j RETURN
iptables -t nat -A REDSOCKS -d 192.168.0.0/16 -j RETURN
iptables -t nat -A REDSOCKS -d 224.0.0.0/4 -j RETURN
iptables -t nat -A REDSOCKS -d 240.0.0.0/4 -j RETURN

# Skip DNS resolvers (prevent iptables from intercepting DNS lookups for hostname-based upstreams)
iptables -t nat -A REDSOCKS -d 192.168.65.7 -j RETURN
iptables -t nat -A REDSOCKS -p udp --dport 53 -j RETURN
iptables -t nat -A REDSOCKS -p tcp --dport 53 -j RETURN

# Dynamically skip ALL upstream proxy IPs (prevents loop)
# Parses IPs from: http://user:pass@HOST:port,http://HOST:port,etc
# Supports hostnames (resolved via python3) and raw IPs
for entry in $(echo "${UPSTREAM_PROXY_URL:-}" | tr ',' ' '); do
    host=$(echo "$entry" | sed -n 's|.*@\([^:]*\):.*|\1|p')       # with auth
    if [ -z "$host" ]; then
        host=$(echo "$entry" | sed -n 's|.*//\([^:]*\):.*|\1|p')  # without auth
    fi
    if [ -n "$host" ]; then
        # Check if it's a raw IP (only digits and dots) or a hostname
        if echo "$host" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'; then
            iptables -t nat -A REDSOCKS -d "$host" -j RETURN
            echo "[INIT]  Excluding upstream: $host"
        else
            # Hostname — resolve via Python (available in container)
            echo "[INIT]  Resolving hostname: $host"
            resolved=$(python3 -c "
import socket
try:
    info = socket.gethostbyname_ex('$host')
    for ip in info[2]:
        print(ip)
except:
    pass
" 2>/dev/null)
            for ip in $resolved; do
                iptables -t nat -A REDSOCKS -d "$ip" -j RETURN
                echo "[INIT]  Excluding upstream IP $ip (resolved from $host)"
            done
        fi
    fi
done

# Also skip manully specified IPs
for ip in $(echo "${UPSTREAM_IPS_EXCLUDE:-}" | tr ',' ' '); do
    iptables -t nat -A REDSOCKS -d "$ip" -j RETURN 2>/dev/null
done

# Skip our own proxy port
iptables -t nat -A REDSOCKS -p tcp --dport 8080 -j RETURN

# Redirect ALL OTHER TCP to our proxy on port 8080
iptables -t nat -A REDSOCKS -p tcp -j REDIRECT --to-port 8080

# Apply REDSOCKS chain to the OUTPUT hook (all outbound TCP from this namespace)
iptables -t nat -A OUTPUT -p tcp -j REDSOCKS

# Expand ephemeral port range to handle many concurrent connections through a single NAT
# Default Linux range is 32768-60999, expand to 10000-65535
echo 10000 65535 > /proc/sys/net/ipv4/ip_local_port_range 2>/dev/null || true
# Reduce TIME_WAIT from 60s to 10s so ports recycle faster
echo 10 > /proc/sys/net/ipv4/tcp_fin_timeout 2>/dev/null || true
echo "[INIT] Expanded ephemeral port range to 10000-65535, TCP FIN timeout 10s"

echo "[INIT] iptables rules installed:"
iptables -t nat -L REDSOCKS -v -n 2>&1

echo "[INIT] Starting rotate-proxy..."
exec /usr/local/bin/rotate-proxy
