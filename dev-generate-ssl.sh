#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: Development SSL/TLS Certificate Generator
#
# Generates local self-signed CA, server, proxy, and client certificates
# with proper SANs and secure private key permissions (0600) for local
# testing and Docker Compose development.
#
# Output is clean plain text (no ANSI color escapes).
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_OUT_DIR="./certs"
OUT_DIR="${DEFAULT_OUT_DIR}"
DAYS=365
KEY_BITS=2048

usage() {
    echo "Usage: $0 [options] [output_dir]"
    echo ""
    echo "Options:"
    echo "  -d, --dir DIR     Output directory for generated certificates (default: ./certs)"
    echo "  --days N          Certificate validity in days (default: 365)"
    echo "  -h, --help        Show this help message"
    echo ""
    echo "Generated files:"
    echo "  ca.crt / ca.key             - Dev Root CA"
    echo "  proxy.crt / proxy.key       - Proxy certificate (pgvisor-proxy, localhost)"
    echo "  server.crt / server.key     - Shared node certificate (pgvisor-node1..3, localhost)"
    echo "  node1/ / node2/ / node3/    - Per-node server.crt and server.key"
    echo "  client.crt / client.key     - Client certificate for user 'postgres'"
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -d|--dir)
            OUT_DIR="$2"
            shift 2
            ;;
        --days)
            DAYS="$2"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            if [[ "$1" == -* ]]; then
                echo "Error: Unknown option $1" >&2
                echo "Run '$0 --help' for usage." >&2
                exit 1
            fi
            OUT_DIR="$1"
            shift
            ;;
    esac
done

if ! command -v openssl > /dev/null 2>&1; then
    echo "Error: 'openssl' command is required but not found." >&2
    exit 1
fi

mkdir -p "${OUT_DIR}"
mkdir -p "${OUT_DIR}/node1" "${OUT_DIR}/node2" "${OUT_DIR}/node3"

echo "============================================================="
echo "  PgVisor Development SSL Certificate Generator"
echo "  Output Directory: ${OUT_DIR}"
echo "  Validity:         ${DAYS} days"
echo "============================================================="
echo ""

# Helper to generate a signed certificate
generate_signed_cert() {
    local cert_name="$1"
    local common_name="$2"
    local sans="$3"
    local target_dir="$4"

    local key_file="${target_dir}/${cert_name}.key"
    local csr_file="${target_dir}/${cert_name}.csr"
    local crt_file="${target_dir}/${cert_name}.crt"

    openssl req -new -newkey "rsa:${KEY_BITS}" -nodes \
        -keyout "${key_file}" \
        -out "${csr_file}" \
        -subj "/CN=${common_name}/O=PgVisor" \
        -addext "subjectAltName = ${sans}" > /dev/null 2>&1

    openssl x509 -req -in "${csr_file}" \
        -CA "${OUT_DIR}/ca.crt" \
        -CAkey "${OUT_DIR}/ca.key" \
        -CAcreateserial \
        -out "${crt_file}" \
        -days "${DAYS}" \
        -copy_extensions copy > /dev/null 2>&1

    rm -f "${csr_file}"
    chmod 644 "${key_file}"
    chmod 644 "${crt_file}"
}

# 1. Root Certificate Authority (CA)
echo "[1/5] Generating Root CA..."
openssl req -x509 -newkey "rsa:${KEY_BITS}" -nodes \
    -keyout "${OUT_DIR}/ca.key" \
    -out "${OUT_DIR}/ca.crt" \
    -days "${DAYS}" \
    -subj "/CN=PgVisor Dev Root CA/O=PgVisor" > /dev/null 2>&1

chmod 644 "${OUT_DIR}/ca.key"
chmod 644 "${OUT_DIR}/ca.crt"
echo "  Created: ca.crt, ca.key (CA private key permissions: 0644)"

# 2. Proxy Certificate
echo "[2/5] Generating Proxy certificate..."
generate_signed_cert "proxy" "pgvisor-proxy" \
    "DNS:pgvisor-proxy,DNS:localhost,IP:127.0.0.1,DNS:host.docker.internal" \
    "${OUT_DIR}"
echo "  Created: proxy.crt, proxy.key (CN=pgvisor-proxy)"

# 3. Shared Cluster Node Certificate
echo "[3/5] Generating Shared Node certificate..."
generate_signed_cert "server" "pgvisor-node" \
    "DNS:pgvisor-node,DNS:pgvisor-node1,DNS:pgvisor-node2,DNS:pgvisor-node3,DNS:localhost,IP:127.0.0.1" \
    "${OUT_DIR}"
echo "  Created: server.crt, server.key (CN=pgvisor-node)"

# 4. Individual Per-Node Certificates
echo "[4/5] Generating Per-Node certificates..."
for i in 1 2 3; do
    generate_signed_cert "server" "pgvisor-node${i}" \
        "DNS:pgvisor-node${i},DNS:localhost,IP:127.0.0.1" \
        "${OUT_DIR}/node${i}"
    echo "  Created: node${i}/server.crt, node${i}/server.key (CN=pgvisor-node${i})"
done

# 5. Client Certificate
echo "[5/5] Generating Client certificate..."
generate_signed_cert "client" "postgres" \
    "DNS:localhost,IP:127.0.0.1" \
    "${OUT_DIR}"
echo "  Created: client.crt, client.key (CN=postgres)"

# Clean up serial file if generated
rm -f "${OUT_DIR}/ca.srl"

echo ""
echo "============================================================="
echo "  SSL Certificates Generated Successfully"
echo "============================================================="
echo ""
echo "Summary of generated certificates in ${OUT_DIR}:"
echo "  - ca.crt / ca.key: Root CA certificate and private key"
echo "  - proxy.crt / proxy.key: Proxy TLS cert (CN=pgvisor-proxy)"
echo "  - server.crt / server.key: Shared cluster node cert"
echo "  - node1..3/server.crt / server.key: Node-specific certificates"
echo "  - client.crt / client.key: Client cert (CN=postgres)"
echo ""
echo "All private keys (*.key) are set to permissions 0644 for container volume mount compatibility."
echo ""
echo "Usage Examples:"
echo "  1. Enable TLS in docker-compose.yml:"
echo "     PGVISOR_TLS_ENABLED=true"
echo "     PGVISOR_TLS_REQUIRED=true"
echo ""
echo "  2. Connect using psql with CA verification:"
echo "     psql \"sslmode=verify-full sslrootcert=${OUT_DIR}/ca.crt host=localhost port=5432 user=postgres dbname=postgres\""
echo ""
echo "  3. Mount custom cert into container:"
echo "     - \${PWD}/certs/node1/server.crt:/var/lib/postgresql/data/pgdata/server.crt:ro"
echo "     - \${PWD}/certs/node1/server.key:/var/lib/postgresql/data/pgdata/server.key:ro"
