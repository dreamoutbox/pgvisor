#!/usr/bin/env python3
"""
PgVisor: Test Docker Compose Profiles Generator

Reads root docker-compose.yml as canonical template and generates isolated,
port-offset Compose files in composes/ for concurrent test execution.
Enforces 1GB RAM and 1 CPU limit on all containers.
Uses shared image tags (pgvisor-test-node:latest, pgvisor-test-proxy:latest)
so images are built once and reused across all concurrent test stacks.
"""

import os
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
ROOT_DIR = SCRIPT_DIR.parent
TEMPLATE_FILE = ROOT_DIR / "docker-compose.yml"
OUTPUT_DIR = ROOT_DIR / "composes"

PROFILES = [
    # (name, project_suffix, proxy_port, dashboard_port, minio_api, minio_console)
    ("crud", "crud", 5532, 8180, 9100, 9101),
    ("backup-restore", "backup-restore", 5632, 8280, 9200, 9201),
    ("pitr", "pitr", 5732, 8380, 9300, 9301),
    ("failover", "failover", 5832, 8480, 9400, 9401),
    ("auto-rejoin", "auto-rejoin", 5932, 8580, 9500, 9501),
    ("rejoin-fenced", "rejoin-fenced", 6032, 8680, 9600, 9601),
    ("add-node", "add-node", 6132, 8780, 9700, 9701),
    ("switchover", "switchover", 6232, 8880, 9800, 9801),
    ("users-permissions", "users-permissions", 6332, 8980, 9900, 9901),
    ("transaction", "transaction", 6432, 9080, 10000, 10001),
    ("routing", "routing", 6532, 9180, 10100, 10101),
    ("audit-logs", "audit-logs", 6632, 9280, 10200, 10201),
    ("double-failure", "double-failure", 6732, 9380, 10300, 10301),
    ("proxy-failover", "proxy-failover", 6832, 9480, 10400, 10401),
    ("metrics", "metrics", 6932, 9580, 10500, 10501),
]


def generate_profiles():
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    if not TEMPLATE_FILE.is_file():
        print(f"Error: Template file not found: {TEMPLATE_FILE}", file=sys.stderr)
        sys.exit(1)

    with open(TEMPLATE_FILE, "r", encoding="utf-8") as f:
        template = f.read()

    for name, proj, proxy_p, dash_p, minio_p, minio_c_p in PROFILES:
        content = template

        # Adjust relative paths for build context and volumes from composes/
        content = re.sub(r'context:\s*\.', 'context: ..', content)
        content = re.sub(r'-\s*\./scripts:', '- ../scripts:', content)

        # Replace container names with test-scoped names
        def replace_container_name(match):
            cname = match.group(1)
            scoped = f"pgvisor-{proj}-{cname.replace('pgvisor-', '')}"
            return f"container_name: {scoped}"

        content = re.sub(r'container_name:\s*(pgvisor-[a-zA-Z0-9_-]+)', replace_container_name, content)

        # Attach shared image tags to avoid redundant image rebuilding across test projects
        # For nodes: image: pgvisor-test-node:latest
        content = re.sub(
            r'(pgvisor-node[123]:\s*\n\s*build:\s*\n\s*context: \.\.\s*\n\s*dockerfile: Dockerfile)',
            r'\1\n    image: pgvisor-test-node:latest',
            content
        )
        # For proxy: image: pgvisor-test-proxy:latest
        content = re.sub(
            r'(pgvisor-proxy:\s*\n\s*build:\s*\n\s*context: \.\.\s*\n\s*dockerfile: Dockerfile)',
            r'\1\n    image: pgvisor-test-proxy:latest',
            content
        )

        # Replace host ports
        content = re.sub(r'"9000:9000"', f'"{minio_p}:9000"', content)
        content = re.sub(r'"9001:9001"', f'"{minio_c_p}:9001"', content)
        content = re.sub(r'"5432:5432"', f'"{proxy_p}:5432"', content)
        content = re.sub(r'"8080:8080"', f'"{dash_p}:8080"', content)

        out_file = OUTPUT_DIR / f"docker-compose.{name}.yml"
        with open(out_file, "w", encoding="utf-8") as f:
            f.write(f"# Auto-generated test compose file for {name} profile\n")
            f.write(f"# Generated from {TEMPLATE_FILE.name} by scripts/generate_test_composes.py\n")
            f.write(content)

        print(f"Generated {out_file.name} (proxy:{proxy_p}, dashboard:{dash_p}, minio:{minio_p}, limits: 1 CPU, 1GB RAM)")

    # Overlay for node4 dynamic scale-out testing
    add_node4_content = """# Auto-generated overlay for pgvisor-add-node testing
volumes:
  node4_data:

services:
  pgvisor-node4:
    build:
      context: ..
      dockerfile: Dockerfile
    image: pgvisor-test-node:latest
    container_name: pgvisor-add-node-node4
    cpus: 1
    mem_limit: 1g
    restart: "no"
    environment:
      POSTGRES_USER: postgres
      PGPORT: 5432
      PGDATA: /var/lib/postgresql/data/pgdata
      PRIMARY_CONNINFO: ${PRIMARY_CONNINFO:-host=pgvisor-node1 port=5432 user=postgres}
      PGVISOR_CLUSTER_ID: pgvisor-cluster
      PGVISOR_NODE_ID: 4
      PGVISOR_ROLE: standby
      PGVISOR_PEERS: http://pgvisor-node1:8080,http://pgvisor-node2:8080,http://pgvisor-node3:8080
      S3_ENDPOINT: http://minio:9000
      S3_BUCKET: pgvisor-backups
      S3_ACCESS_KEY: minioadmin
      S3_SECRET_KEY: minioadmin
      RUST_LOG: info
    volumes:
      - node4_data:/var/lib/postgresql/data
    healthcheck:
      test: [ "CMD", "pg_isready", "-U", "postgres", "-h", "127.0.0.1" ]
      interval: 3s
      timeout: 3s
      retries: 10
"""
    node4_file = OUTPUT_DIR / "docker-compose.add-node4.yml"
    with open(node4_file, "w", encoding="utf-8") as f:
        f.write(add_node4_content)
    print(f"Generated {node4_file.name} (overlay for 4th node scale-out, limits: 1 CPU, 1GB RAM)")

    # Overlay for proxy2 failover testing
    proxy2_content = """# Auto-generated overlay for pgvisor-proxy-failover testing
services:
  pgvisor-proxy2:
    build:
      context: ..
      dockerfile: Dockerfile
    image: pgvisor-test-proxy:latest
    container_name: pgvisor-proxy-failover-proxy2
    entrypoint: [ "pgvisor-proxy" ]
    restart: unless-stopped
    cpus: 1
    mem_limit: 1g
    ports:
      - "6833:5432"
      - "9481:8080"
    environment:
      PGVISOR_PROXY_LISTEN: 0.0.0.0:5432
      PGVISOR_DASHBOARD_LISTEN: 0.0.0.0:8080
      PGVISOR_LEADER_ADDR: pgvisor-node1:5432
      PGVISOR_STANDBY_ADDRS: pgvisor-node2:5432,pgvisor-node3:5432
      PGVISOR_CLUSTER_ID: pgvisor-cluster
      S3_ENDPOINT: http://minio:9000
      S3_BUCKET: pgvisor-backups
      S3_ACCESS_KEY: minioadmin
      S3_SECRET_KEY: minioadmin
      RUST_LOG: info
      PGVISOR_ADMIN_TOKEN: postgres
    volumes:
      - ../scripts:/scripts:ro
    depends_on:
      pgvisor-node1:
        condition: service_healthy
      pgvisor-node2:
        condition: service_healthy
      pgvisor-node3:
        condition: service_healthy
"""
    proxy2_file = OUTPUT_DIR / "docker-compose.proxy-failover-proxy2.yml"
    with open(proxy2_file, "w", encoding="utf-8") as f:
        f.write(proxy2_content)
    print(f"Generated {proxy2_file.name} (overlay for second proxy, limits: 1 CPU, 1GB RAM)")



if __name__ == "__main__":
    generate_profiles()
