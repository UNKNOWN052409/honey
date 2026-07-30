# Multi-stage build for hg-supervisor v3.0 — Sticky Session Edition
# Stage 1: Build the Rust supervisor
FROM rust:1.81-slim-bookworm AS builder

WORKDIR /build
COPY supervisor/Cargo.toml supervisor/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null || true
COPY supervisor/src/ ./src/
RUN cargo build --release

# Stage 2: Build the deploy image
FROM debian:bookworm-slim

# Install required runtime libraries
RUN apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
        ca-certificates \
        wget \
        libc6 \
    && rm -rf /var/lib/apt/lists/*

# Copy the honeygain binary from pre-extracted build context
COPY honeygain-binary/honeygain /app/honeygain
COPY honeygain-binary/libs/libhg.so.2.0.0 /usr/lib/libhg.so.2.0.0
COPY honeygain-binary/libs/libc.so.6 /usr/lib/x86_64-linux-gnu/libc.so.6
COPY honeygain-binary/libs/ld-linux-x86-64.so.2 /usr/lib64/ld-linux-x86-64.so.2
RUN ldconfig /usr/lib /usr/lib/x86_64-linux-gnu /usr/lib64
RUN chmod +x /app/honeygain

# Copy the supervisor binary from builder stage
COPY --from=builder /build/target/release/hg-supervisor /app/hg-supervisor

WORKDIR /app

# Expose health endpoint
EXPOSE 8080

# Health check via HTTP endpoint
HEALTHCHECK --interval=30s --timeout=10s --start-period=15s --retries=3 \
  CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

# Config via env vars (Render secrets). Start supervisor.
CMD ["/app/hg-supervisor"]
