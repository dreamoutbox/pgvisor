# Multi-stage build for PgVisor using cargo-chef
FROM lukemathwalker/cargo-chef:latest-rust-bookworm AS chef
WORKDIR /app

# Planner stage: compute dependency recipe
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

# Builder stage: cache dependencies and build binaries
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application crates
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release

# Final runtime image based on official PostgreSQL 18.6
FROM postgres:18.6-bookworm

# Install ca-certificates and curl for healthchecks and S3 MinIO communication
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy compiled PgVisor binaries
COPY --from=builder /app/target/release/pgvisor-sidecar /usr/local/bin/
COPY --from=builder /app/target/release/pgvisor-proxy /usr/local/bin/
COPY --from=builder /app/target/release/pgvisor-dashboard /usr/local/bin/

# Prepare data directory with proper ownership
RUN mkdir -p /var/lib/postgresql/data && chown -R postgres:postgres /var/lib/postgresql

USER postgres
ENV PGDATA=/var/lib/postgresql/data/pgdata

EXPOSE 5432 8080

ENTRYPOINT ["pgvisor-sidecar"]
