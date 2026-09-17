# Multi-stage build for PgVisor using cargo-chef
FROM lukemathwalker/cargo-chef:0.1.78-rust-1.98.1-bookworm AS chef
WORKDIR /app

# Planner stage: compute dependency recipe
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

# Builder stage: cache dependencies and build binaries
FROM chef AS builder
# Install mold linker for significantly faster link times
RUN apt-get update && apt-get install -y --no-install-recommends mold && rm -rf /var/lib/apt/lists/*
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"

COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - cached Docker layer with Cargo registry/git cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --recipe-path recipe.json

# Build application binaries
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release \
        -p pgvisor-proxy \
        -p pgvisor-sidecar \
        -p pgvisor-dashboard

# Final runtime image based on official PostgreSQL 18.6
FROM postgres:18.6-bookworm

# Install runtime dependencies and prepare data directory in a single layer
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/postgresql/data /var/lib/postgresql/tls /var/lib/pgvisor/tls \
    && chown -R postgres:postgres /var/lib/postgresql /var/lib/pgvisor

# Copy compiled PgVisor binaries
COPY --from=builder /app/target/release/pgvisor-sidecar /usr/local/bin/
COPY --from=builder /app/target/release/pgvisor-proxy /usr/local/bin/
COPY --from=builder /app/target/release/pgvisor-dashboard /usr/local/bin/

USER postgres
ENV PGDATA=/var/lib/postgresql/data/pgdata

EXPOSE 5432 8080

ENTRYPOINT ["pgvisor-sidecar"]
