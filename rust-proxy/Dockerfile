# Multi-stage build
FROM rust:1.86-bookworm AS builder
WORKDIR /app
COPY Cargo.toml .
COPY src/ src/
RUN cargo build --release && \
    cp target/release/rotate-proxy /usr/local/bin/ && \
    strip /usr/local/bin/rotate-proxy && \
    ls -lh /usr/local/bin/rotate-proxy

# Minimal runtime with iptables
FROM debian:bookworm-slim
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends iptables ca-certificates procps socat openssl && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/rotate-proxy /usr/local/bin/rotate-proxy
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
EXPOSE 8080 8081
ENV LISTEN_ADDR=0.0.0.0:8080
ENV TUNNEL_MAX_LIFETIME_SECS=120
ENTRYPOINT ["/entrypoint.sh"]
