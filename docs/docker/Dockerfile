# Multi-stage build for minimal production image

# Stage 1: Build
FROM rust:1.75-slim as builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build release binary
RUN cargo build --release --bin guacr-daemon

# Stage 2: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 -s /bin/bash guacr

# Create directories
RUN mkdir -p /etc/guacr /var/lib/guacr/recordings /var/log/guacr && \
    chown -R guacr:guacr /etc/guacr /var/lib/guacr /var/log/guacr

# Copy binary from builder
COPY --from=builder /app/target/release/guacr-daemon /usr/local/bin/guacr-daemon

# Copy default configuration
COPY config/guacr.example.toml /etc/guacr/guacr.toml

# Set permissions
RUN chmod +x /usr/local/bin/guacr-daemon

# Switch to non-root user
USER guacr

# Expose ports
EXPOSE 4822 4823 8080 9090 50051

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -f http://localhost:8080/health || exit 1

# Set entrypoint
ENTRYPOINT ["/usr/local/bin/guacr-daemon"]
CMD ["--config", "/etc/guacr/guacr.toml"]
