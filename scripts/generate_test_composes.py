#!/usr/bin/env python3
"""
PgVisor: Test Docker Compose Profiles Generator

Reads root docker-compose.yml as canonical template and generates isolated,
port-offset Compose files in composes/ for concurrent test execution.
Enforces 1GB RAM limit on all PostgreSQL services.
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

        # Replace container names with test-scoped names & inject 1GB RAM limit on postgres nodes
        def replace_container_name(match):
            cname = match.group(1)
            scoped = f"pgvisor-{proj}-{cname.replace('pgvisor-', '')}"
            res = f"container_name: {scoped}"
            if "node" in cname:
                res += "\n    mem_limit: 1g"
            return res

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

        print(f"Generated {out_file.name} (proxy:{proxy_p}, dashboard:{dash_p}, minio:{minio_p}, mem_limit: 1g)")

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
    print(f"Generated {node4_file.name} (overlay for 4th node scale-out, mem_limit: 1g)")


if __name__ == "__main__":
    generate_profiles()
