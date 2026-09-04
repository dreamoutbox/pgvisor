# Multi-stage build for PgVisor
FROM rust:bookworm AS builder

WORKDIR /app

# Cache layer: copy Cargo definitions and build all crates
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release

# Final runtime image based on official PostgreSQL 16
FROM postgres:16-bookworm

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
