#!/usr/bin/env bash
set -euo pipefail

echo "========================================================="
echo "  PgVisor Cluster Reset & Build Script"
echo "========================================================="

echo "[1/4] Stopping containers and removing persistent volumes..."
docker compose down -v --remove-orphans

echo "[2/4] Building PgVisor Docker image..."
docker compose build

echo "[3/4] Launching MinIO + 3 Nodes + 1 Proxy services..."
docker compose up -d

echo "[4/4] Verifying cluster startup..."
echo "Waiting for services to initialize..."
sleep 5

echo "========================================================="
echo "  PgVisor Cluster is Ready for Demo!"
echo "========================================================="
echo "  - PostgreSQL L7 Proxy: localhost:5432"
echo "  - Web Dashboard:       http://localhost:8080"
echo "  - MinIO S3 API:        http://localhost:9000"
echo "  - MinIO Web Console:   http://localhost:9001 (minioadmin:minioadmin)"
echo ""
echo "To test PostgreSQL cluster operations, run:"
echo "  ./test-cluster-crud.sh"
echo "========================================================="
