#!/usr/bin/env python3
"""
Generate complete docker-compose.yml for hg-supervisor v4.0 — Dual-App Edition.

Supports both Honeygain and Pawns.app with per-instance network isolation.

Architecture:
    Honeygain:  rotate-proxy (shared) -> hg-1..hg-N (all share network namespace)
    Pawns:      rotate-proxy-pawns-i <-> pawns-i (1:1, complete isolation)

Usage:
    # Generate full docker-compose.yml:
    python generate-compose.py > docker-compose.yml

    # Scale with env vars:
    HG_INSTANCES=10 PAWNS_INSTANCES=5 python generate-compose.py > docker-compose.yml

    # Or with hardcoded values in env:
    PAWNS_INSTANCES=10 python generate-compose.py >> docker-compose.yml

Env vars:
    HG_INSTANCES              Number of Honeygain instances (default: 8)
    HG_EMAIL                  Honeygain email (${HG_EMAIL})
    HG_PASS                   Honeygain password (${HG_PASS})
    PAWNS_INSTANCES           Number of Pawns instances (default: 3)
    PAWNS_EMAIL               Pawns email (${PAWNS_EMAIL})
    PAWNS_PASSWORD            Pawns password (${PAWNS_PASSWORD})
    PAWNS_COUNTRY             Sticky session country (default: ${PAWNS_COUNTRY:-us})
    PROXYRISE_ENDPOINT        ProxyRise endpoint (default: ${PROXYRISE_ENDPOINT:-gw.proxyrise.com:443})
    PAWNS_PROXYRISE_PASSWORD  ProxyRise password (${PAWNS_PROXYRISE_PASSWORD})
    HG_PROXY_BASE_PORT        Base port for Honeygain proxy (default: 9150)
    PAWNS_PROXY_BASE_PORT     Base port for Pawns proxy status (default: 8082)
    TUNNEL_MAX_LIFETIME_SECS  Tunnel rotation interval (default: 600)
"""

import io
import os
import sys

if sys.platform == "win32":
    sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")


def li(indent: int, text: str) -> str:
    return "  " * indent + text


def main():
    hg_instances = int(os.environ.get("HG_INSTANCES", "8"))
    hg_email = os.environ.get("HG_EMAIL", "${HG_EMAIL}")
    hg_pass = os.environ.get("HG_PASS", "${HG_PASS}")
    hg_proxy_base = int(os.environ.get("HG_PROXY_BASE_PORT", "9150"))

    pawns_instances = int(os.environ.get("PAWNS_INSTANCES", "3"))
    pawns_email = os.environ.get("PAWNS_EMAIL", "${PAWNS_EMAIL}")
    pawns_password = os.environ.get("PAWNS_PASSWORD", "${PAWNS_PASSWORD}")
    pawns_country = os.environ.get("PAWNS_COUNTRY", "${PAWNS_COUNTRY:-us}")
    proxyrise_endpoint = os.environ.get(
        "PROXYRISE_ENDPOINT", "${PROXYRISE_ENDPOINT:-gw.proxyrise.com:443}"
    )
    proxyrise_password = os.environ.get("PAWNS_PROXYRISE_PASSWORD", "${PAWNS_PROXYRISE_PASSWORD}")
    pawns_proxy_base = int(os.environ.get("PAWNS_PROXY_BASE_PORT", "8082"))
    tunnel_lifetime = int(os.environ.get("TUNNEL_MAX_LIFETIME_SECS", "600"))

    L = []

    # ── Header ──
    L.append(li(0, "services:"))
    L.append("")

    # ── Monitor ──
    L.append(li(1, "monitor:"))
    L.append(li(2, "image: nginx:alpine"))
    L.append(li(2, "container_name: monitor"))
    L.append(li(2, "restart: unless-stopped"))
    L.append(li(2, "ports:"))
    L.append(li(3, '- "9090:80"'))
    L.append(li(2, "volumes:"))
    L.append(li(3, '- ./monitor:/usr/share/nginx/html:ro'))
    L.append("")

    # ── Honeygain proxy (shared) ──
    L.append(li(1, "# ── Honeygain proxy (shared by all HG instances) ──"))
    L.append(li(1, "rotate-proxy:"))
    L.append(li(2, "image: honeygain-rotate-proxy:latest"))
    L.append(li(2, "container_name: rotate-proxy"))
    L.append(li(2, "restart: unless-stopped"))
    L.append(li(2, "ports:"))
    L.append(li(3, '- "8081:8080"'))
    L.append(li(2, "cap_add:"))
    L.append(li(3, "- NET_ADMIN"))
    L.append(li(3, "- NET_RAW"))
    L.append(li(2, "sysctls:"))
    L.append(li(3, "- net.ipv4.ip_local_port_range=10000 65535"))
    L.append(li(3, "- net.ipv4.tcp_fin_timeout=5"))
    L.append(li(2, "environment:"))
    L.append(li(3, "- UPSTREAM_PROXY_URL="))
    L.append(li(3, "- LISTEN_ADDR=0.0.0.0:8080"))
    L.append(li(3, f"- TUNNEL_MAX_LIFETIME_SECS={tunnel_lifetime}"))
    L.append("")

    # ── Honeygain instances ──
    L.append(li(1, f"# ── Honeygain instances ({hg_instances}) ──"))
    for i in range(1, hg_instances + 1):
        L.append(li(1, f"hg-{i}:"))
        L.append(li(2, "image: honeygain/honeygain:latest"))
        L.append(li(2, f"container_name: hg-{i}"))
        L.append(li(2, "restart: unless-stopped"))
        L.append(li(2, "depends_on:"))
        L.append(li(3, "rotate-proxy:"))
        L.append(li(4, "condition: service_started"))
        L.append(li(2, 'network_mode: "service:rotate-proxy"'))
        L.append(li(2, "command:"))
        L.append(li(3, f"- -email={hg_email}"))
        L.append(li(3, f"- -pass={hg_pass}"))
        L.append(li(3, f"- -device=HG-{i}"))
        L.append(li(3, "- -tou-accept"))
        L.append("")

    # ── Pawns proxy + instances (1:1 isolation) ──
    L.append(li(1, f"# ── Pawns.app instances ({pawns_instances}, 1 proxy per instance, complete network isolation) ──"))
    for i in range(1, pawns_instances + 1):
        proxy_name = f"rotate-proxy-pawns-{i}"
        pawns_name = f"pawns-{i}"
        port = pawns_proxy_base + i - 1
        device_name = f"pawns-dev{i}"
        session_user = f"res-{pawns_country}-sid-{i}"

        L.append(li(1, f"{proxy_name}:"))
        L.append(li(2, "image: honeygain-rotate-proxy:latest"))
        L.append(li(2, f"container_name: {proxy_name}"))
        L.append(li(2, "restart: unless-stopped"))
        L.append(li(2, "ports:"))
        L.append(li(3, f'- "{port}:8080"'))
        L.append(li(2, "cap_add:"))
        L.append(li(3, "- NET_ADMIN"))
        L.append(li(3, "- NET_RAW"))
        L.append(li(2, "sysctls:"))
        L.append(li(3, "- net.ipv4.ip_local_port_range=10000 65535"))
        L.append(li(3, "- net.ipv4.tcp_fin_timeout=5"))
        L.append(li(2, "environment:"))
        L.append(li(3, f"- UPSTREAM_PROXY_URL=socks5://{session_user}:{proxyrise_password}@{proxyrise_endpoint}"))
        L.append(li(3, "- LISTEN_ADDR=0.0.0.0:8080"))
        L.append(li(3, f"- TUNNEL_MAX_LIFETIME_SECS={tunnel_lifetime}"))
        L.append("")

        L.append(li(1, f"{pawns_name}:"))
        L.append(li(2, "image: iproyal/pawns-cli:latest"))
        L.append(li(2, f"container_name: {pawns_name}"))
        L.append(li(2, "restart: unless-stopped"))
        L.append(li(2, "depends_on:"))
        L.append(li(3, f"{proxy_name}:"))
        L.append(li(4, "condition: service_started"))
        L.append(li(2, f'network_mode: "service:{proxy_name}"'))
        L.append(li(2, "command:"))
        L.append(li(3, f"- -email={pawns_email}"))
        L.append(li(3, f"- -password={pawns_password}"))
        L.append(li(3, f"- -device-name={device_name}"))
        L.append(li(3, f"- -device-id={i}"))
        L.append(li(3, "- -accept-tos"))
        L.append("")

    sys.stdout.write("\n".join(L))
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
