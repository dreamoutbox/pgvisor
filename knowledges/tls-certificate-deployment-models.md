# TLS/SSL Certificate Deployment Models in PgVisor

PgVisor supports two primary certificate deployment strategies: **Shared Cluster (Multi-SAN) Certificates** for simplified operations and renewal, and **Per-Node Certificates** for maximum security isolation.

---

## Model Comparison

| Characteristic | Shared Cluster Certificate (Multi-SAN) | Per-Node Certificates (Discrete) |
|---|---|---|
| **Files** | 1 certificate + 1 private key shared across all nodes | Distinct certificate and private key per node |
| **SAN Requirements** | Must list all node hostnames/IPs or wildcard (`*.cluster.local`) | Only lists that node's hostname/IP |
| **Deployment Complexity** | **Low**: Same volume/secret mounted to all nodes | **Medium**: Unique secret/volume per node |
| **Renewal / Rotation** | **Simple**: Renew 1 certificate once, push to all nodes | Rotate each node certificate independently |
| **Blast Radius** | If the private key is compromised, all nodes are affected | Compromise of one node does not affect others |
| **Recommended For** | Developers, small clusters, simpler ops | High-compliance, multi-tenant, strict isolation |

---

## 1. Shared Cluster Certificate Model (Simple Ops & Renewal)

In this model, every sidecar container mounts the exact same `server.crt` and `server.key`.

### Certificate Requirements
The certificate must include **Subject Alternative Names (SANs)** covering all nodes in the cluster, for example:
```text
subjectAltName = DNS:pgvisor-node, DNS:pgvisor-node1, DNS:pgvisor-node2, DNS:pgvisor-node3, DNS:localhost, IP:127.0.0.1
```
Or a wildcard domain matching your container network (e.g. `DNS:*.pgvisor.local`).

### Generating with `dev-generate-ssl.sh`
The helper script `./dev-generate-ssl.sh` automatically creates this shared certificate under `./certs/server.crt` and `./certs/server.key`:
```bash
./dev-generate-ssl.sh
```

### Docker Compose Configuration
Mount the shared certificate into each node service and set `PGVISOR_TLS_CERT_FILE` and `PGVISOR_TLS_KEY_FILE`:

```yaml
services:
  pgvisor-node1:
    image: pgvisor-test-node:latest
    environment:
      PGVISOR_TLS_ENABLED: "true"
      PGVISOR_TLS_CERT_FILE: /var/lib/postgresql/tls/server.crt
      PGVISOR_TLS_KEY_FILE: /var/lib/postgresql/tls/server.key
    volumes:
      - node1_data:/var/lib/postgresql/data
      - ./certs/server.crt:/var/lib/postgresql/tls/server.crt:ro
      - ./certs/server.key:/var/lib/postgresql/tls/server.key:ro

  pgvisor-node2:
    image: pgvisor-test-node:latest
    environment:
      PGVISOR_TLS_ENABLED: "true"
      PGVISOR_TLS_CERT_FILE: /var/lib/postgresql/tls/server.crt
      PGVISOR_TLS_KEY_FILE: /var/lib/postgresql/tls/server.key
    volumes:
      - node2_data:/var/lib/postgresql/data
      - ./certs/server.crt:/var/lib/postgresql/tls/server.crt:ro
      - ./certs/server.key:/var/lib/postgresql/tls/server.key:ro

  pgvisor-node3:
    image: pgvisor-test-node:latest
    environment:
      PGVISOR_TLS_ENABLED: "true"
      PGVISOR_TLS_CERT_FILE: /var/lib/postgresql/tls/server.crt
      PGVISOR_TLS_KEY_FILE: /var/lib/postgresql/tls/server.key
    volumes:
      - node3_data:/var/lib/postgresql/data
      - ./certs/server.crt:/var/lib/postgresql/tls/server.crt:ro
      - ./certs/server.key:/var/lib/postgresql/tls/server.key:ro
```

### Certificate Renewal Workflow
1. Re-generate or renew `server.crt` and `server.key` (or update your Kubernetes Secret / directory).
2. Ensure the key file maintains `0600` permissions (`chmod 600 certs/server.key`).
3. Restart or reload the cluster containers. Because all nodes share the single secret/path, renewal requires only one file update.

---

## 2. Per-Node Certificate Model (Maximum Isolation)

In this model, each node holds its own unique private key and certificate signed by the Root CA (or self-signed).

### Option A: Automatic In-Container Generation (Zero Configuration)
If `PGVISOR_TLS_ENABLED=true` is set without specifying `PGVISOR_TLS_CERT_FILE`, each sidecar generates its own self-signed key pair on startup inside `$PGDATA/server.crt` and `$PGDATA/server.key`:
```yaml
environment:
  PGVISOR_TLS_ENABLED: "true"
  # No PGVISOR_TLS_CERT_FILE specified -> auto-generated per-node
```

### Option B: Mounting Pre-Generated Distinct Certs
Using `./dev-generate-ssl.sh`, distinct per-node certs are generated in `./certs/node1/`, `./certs/node2/`, and `./certs/node3/`:
```yaml
services:
  pgvisor-node1:
    volumes:
      - ./certs/node1/server.crt:/var/lib/postgresql/tls/server.crt:ro
      - ./certs/node1/server.key:/var/lib/postgresql/tls/server.key:ro
    environment:
      PGVISOR_TLS_ENABLED: "true"
      PGVISOR_TLS_CERT_FILE: /var/lib/postgresql/tls/server.crt
      PGVISOR_TLS_KEY_FILE: /var/lib/postgresql/tls/server.key

  pgvisor-node2:
    volumes:
      - ./certs/node2/server.crt:/var/lib/postgresql/tls/server.crt:ro
      - ./certs/node2/server.key:/var/lib/postgresql/tls/server.key:ro
    environment:
      PGVISOR_TLS_ENABLED: "true"
      PGVISOR_TLS_CERT_FILE: /var/lib/postgresql/tls/server.crt
      PGVISOR_TLS_KEY_FILE: /var/lib/postgresql/tls/server.key
```

---

## Proxy Ingress & Client Verification

The proxy (`pgvisor-proxy`) has its own TLS endpoint facing clients:
- **Proxy Certificate**: Generated into `proxy_tls_data` volume (or mounted from `./certs/proxy.crt` and `./certs/proxy.key`).
- **Enforcing TLS**: Set `PGVISOR_TLS_REQUIRED=true` on the proxy to block unencrypted connections.
- **Client Verification**: Clients can verify the cluster CA:
  ```bash
  psql "sslmode=verify-full sslrootcert=./certs/ca.crt host=localhost port=5432 user=postgres dbname=postgres"
  ```

---

## Important Security Rules
- **Private Key Permissions**: PostgreSQL strictly rejects private key files with permissions more permissive than `0600` (`u=rw,g=,o=`). Always ensure mounted keys are owned by or readable by user `postgres` with mode `0600`.
- **Backend TLS Mode**: The proxy connection pool connects to backends using `sslmode=require` semantics, encrypting all intra-cluster query traffic.
