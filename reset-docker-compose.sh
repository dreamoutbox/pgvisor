#!/usr/bin/env bash
set -euo pipefail

echo "========================================================="
echo "  PgVisor Cluster Reset & Build Script"
echo "========================================================="

DO_BUILD=false
for arg in "$@"; do
    case "$arg" in
        --build)
            DO_BUILD=true
            ;;
    esac
done

echo "[1/4] Stopping containers and removing persistent volumes..."
docker compose down -v --remove-orphans

if [ "$DO_BUILD" = true ]; then
    echo "[2/4] Building PgVisor Docker images (--build specified)..."
    docker compose build
else
    echo "[2/4] Skipping Docker image build (pass --build to rebuild images)..."
fi

echo "[3/4] Launching MinIO + 3 Nodes + 1 Proxy services..."
docker compose up -d

echo "[4/4] Verifying cluster startup and waiting for health checks..."
until [ "$(docker inspect -f '{{.State.Health.Status}}' pgvisor-node1 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' pgvisor-node2 2>/dev/null)" = "healthy" ] && \
      [ "$(docker inspect -f '{{.State.Health.Status}}' pgvisor-node3 2>/dev/null)" = "healthy" ]; do
    echo "Waiting for PostgreSQL cluster nodes to report healthy..."
    sleep 1
done


echo "========================================================="
echo "  PgVisor Cluster is Ready!"
echo "========================================================="
echo "  - PostgreSQL L7 Proxy: localhost:5432"
echo "  - Web Dashboard:       http://localhost:8080"
echo "  - MinIO S3 API:        http://localhost:9000"
echo "  - MinIO Web Console:   http://localhost:9001 (minioadmin:minioadmin)"
echo ""
echo "To test PostgreSQL cluster operations, run:"
echo "  ./tests/test-cluster-crud.sh"
echo "========================================================="
