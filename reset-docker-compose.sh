#!/usr/bin/env bash
set -euo pipefail

echo "Stopping containers and removing volumes..."
docker compose down -v --remove-orphans

echo "Starting containers..."
docker compose up -d

echo "Done! MinIO API at http://localhost:9000, Console at http://localhost:9001 (minioadmin:minioadmin)"
