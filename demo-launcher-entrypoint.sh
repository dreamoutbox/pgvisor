#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# PgVisor: DigitalOcean Droplet Startup & Demo Showcase Launcher
#
# Automates the full setup of a 3-node PostgreSQL HA cluster on a fresh
# DigitalOcean droplet (or any Debian/Ubuntu cloud VM):
#   1. Allocates swap file on smaller droplet sizes to prevent OOM
#   2. Installs base prerequisites (git, curl, ca-certificates, jq)
#   3. Installs Docker Engine & Docker Compose v2 via official script
#   4. Clones the PgVisor repository into /opt/pgvisor
#   5. Pre-pulls container images for fast bootstrap
#   6. Navigates to examples/ and starts cluster via ./pgvisorctl.sh up
#   7. Configures UFW firewall rules if active
#   8. Executes dev-mock-data.sh to seed schema, data, and backup snapshots
#   9. Detects droplet public IP and configures /etc/motd & DEMO_INFO.txt
#
# Suitable for direct use as DigitalOcean Droplet User Data (cloud-init).
# All output is strictly clean plain text (no ANSI escape codes).
# ==============================================================================

# Default configurations with environment variable overrides
REPO_URL="${REPO_URL:-https://github.com/dreamoutbox/pgvisor.git}"
REPO_BRANCH="${REPO_BRANCH:-master}"
PGVISOR_IMAGE="${PGVISOR_IMAGE:-dreamoutbox/pgvisor:latest}"
S3_IMAGE="${S3_IMAGE:-rustfs/rustfs:latest}"
SEED_DEMO_DATA="${SEED_DEMO_DATA:-true}"
START_DEMO_APP="${START_DEMO_APP:-false}"
DO_BUILD="${DO_BUILD:-false}"

# Detect if executing within an existing cloned repository
SCRIPT_DIR="$(dirname "${BASH_SOURCE[0]}")"
SCRIPT_SOURCE_DIR="$(cd "${SCRIPT_DIR}" >/dev/null 2>&1 && pwd)"
SCRIPT_SOURCE_DIR="${SCRIPT_SOURCE_DIR:-$PWD}"
if [ -f "${SCRIPT_SOURCE_DIR}/examples/pgvisorctl.sh" ]; then
    INSTALL_DIR="${INSTALL_DIR:-${SCRIPT_SOURCE_DIR}}"
else
    INSTALL_DIR="${INSTALL_DIR:-/opt/pgvisor}"
fi

# Ensure non-interactive package operations
export DEBIAN_FRONTEND=noninteractive
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"

# ------------------------------------------------------------------------------
# Function: log_info
# Purpose : Print an informational message with a UTC timestamp to stdout.
# Inputs  : $1 - Message string to display
# Example : log_info "Installing Docker Engine..."
# ------------------------------------------------------------------------------
log_info() {
    local message="$1"
    echo "[$(date -u +'%Y-%m-%d %H:%M:%SZ')] [INFO] ${message}"
}

# ------------------------------------------------------------------------------
# Function: log_warn
# Purpose : Print a warning message with a UTC timestamp to stderr.
# Inputs  : $1 - Warning message string to display
# Example : log_warn "Apt lock held by another process; retrying..."
# ------------------------------------------------------------------------------
log_warn() {
    local message="$1"
    echo "[$(date -u +'%Y-%m-%d %H:%M:%SZ')] [WARN] ${message}" >&2
}

# ------------------------------------------------------------------------------
# Function: log_error
# Purpose : Print an error message with a UTC timestamp to stderr.
# Inputs  : $1 - Error message string to display
# Example : log_error "Failed to clone repository from GitHub."
# ------------------------------------------------------------------------------
log_error() {
    local message="$1"
    echo "[$(date -u +'%Y-%m-%d %H:%M:%SZ')] [ERROR] ${message}" >&2
}

# ------------------------------------------------------------------------------
# Function: wait_for_apt_locks
# Purpose : Wait until dpkg and apt frontend locks are released by background
#           processes (such as cloud-init or unattended-upgrades) before proceeding.
# Inputs  : None
# Example : wait_for_apt_locks
# ------------------------------------------------------------------------------
wait_for_apt_locks() {
    if ! command -v fuser >/dev/null 2>&1; then
        return 0
    fi

    local max_wait_seconds=120
    local waited=0

    while fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1 || \
          fuser /var/lib/dpkg/lock >/dev/null 2>&1 || \
          fuser /var/lib/apt/lists/lock >/dev/null 2>&1; do
        if [ "${waited}" -ge "${max_wait_seconds}" ]; then
            log_warn "Timeout waiting for apt locks after ${max_wait_seconds}s. Continuing anyway..."
            break
        fi
        log_info "Waiting for apt/dpkg lock release (${waited}s / ${max_wait_seconds}s)..."
        sleep 3
        waited=$((waited + 3))
    done
}

# ------------------------------------------------------------------------------
# Function: setup_swap_if_needed
# Purpose : Check available physical RAM and configure a 2GB swap file on
#           smaller cloud instances (under 2.5GB RAM) to avoid OOM kills during
#           multi-container Docker startup.
# Inputs  : None
# Example : setup_swap_if_needed
# ------------------------------------------------------------------------------
setup_swap_if_needed() {
    if [ ! -r /proc/meminfo ]; then
        return 0
    fi

    local total_mem_kb
    total_mem_kb=$(grep MemTotal /proc/meminfo | awk '{print $2}')
    local swap_total_kb
    swap_total_kb=$(grep SwapTotal /proc/meminfo | awk '{print $2}')

    # If memory is less than ~2.5GB (2621440 KB) and swap is 0 KB, create 2GB swap file
    if [ "${total_mem_kb}" -lt 2621440 ] && [ "${swap_total_kb}" -eq 0 ]; then
        log_info "Detected ${total_mem_kb} KB RAM with no swap configured. Creating 2GB swap file..."
        if command -v fallocate >/dev/null 2>&1; then
            fallocate -l 2G /swapfile 2>/dev/null || dd if=/dev/zero of=/swapfile bs=1M count=2048 status=none
        else
            dd if=/dev/zero of=/swapfile bs=1M count=2048 status=none
        fi
        chmod 600 /swapfile
        mkswap /swapfile >/dev/null 2>&1
        swapon /swapfile >/dev/null 2>&1 || log_warn "Failed to activate swap file."
        if ! grep -q '/swapfile' /etc/fstab 2>/dev/null; then
            echo '/swapfile none swap sw 0 0' >> /etc/fstab
        fi
        log_info "2GB swap file successfully allocated and enabled."
    fi
}

# ------------------------------------------------------------------------------
# Function: install_prerequisites
# Purpose : Ensure essential utility packages (git, curl, ca-certificates, jq,
#           postgresql-client) are installed via apt on Debian/Ubuntu systems.
# Inputs  : None
# Example : install_prerequisites
# ------------------------------------------------------------------------------
install_prerequisites() {
    if command -v git >/dev/null 2>&1 && \
       command -v curl >/dev/null 2>&1 && \
       command -v jq >/dev/null 2>&1 && \
       command -v psql >/dev/null 2>&1; then
        log_info "Prerequisite packages (git, curl, jq, psql) already available."
        return 0
    fi

    log_info "Installing prerequisite packages (git, curl, ca-certificates, jq, postgresql-client)..."
    wait_for_apt_locks

    local max_retries=3
    local attempt=1
    while [ "${attempt}" -le "${max_retries}" ]; do
        if apt-get update -y && \
           apt-get install -y --no-install-recommends git curl ca-certificates jq postgresql-client; then
            log_info "Prerequisites installed successfully."
            return 0
        fi
        log_warn "apt-get install attempt ${attempt}/${max_retries} failed; retrying in 5 seconds..."
        sleep 5
        attempt=$((attempt + 1))
    done

    log_warn "Could not install all packages via apt-get. Continuing with existing tools."
}

# ------------------------------------------------------------------------------
# Function: install_docker
# Purpose : Check for Docker Engine and Docker Compose v2. If missing, download
#           and execute the official Docker setup script, then start the daemon.
# Inputs  : None
# Example : install_docker
# ------------------------------------------------------------------------------
install_docker() {
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
        log_info "Docker Engine and Docker Compose v2 are already installed."
    else
        log_info "Installing Docker Engine and Docker Compose plugin via official script..."
        wait_for_apt_locks
        curl -fsSL https://get.docker.com -o /tmp/get-docker.sh
        sh /tmp/get-docker.sh
        rm -f /tmp/get-docker.sh
        log_info "Docker installation completed."
    fi

    if command -v systemctl >/dev/null 2>&1; then
        systemctl enable --now docker >/dev/null 2>&1 || true
    elif command -v service >/dev/null 2>&1; then
        service docker start >/dev/null 2>&1 || true
    fi

    # Verify Docker daemon is responding
    local retry=0
    while ! docker info >/dev/null 2>&1; do
        if [ "${retry}" -ge 15 ]; then
            log_error "Docker daemon is not responding after 30 seconds."
            return 1
        fi
        sleep 2
        retry=$((retry + 1))
    done
    log_info "Docker daemon is active and responsive."
}

# ------------------------------------------------------------------------------
# Function: clone_or_update_repo
# Purpose : Clone the target repository to destination path, or pull the latest
#           commits if the repository directory already exists.
# Inputs  : $1 - repo_url: Git clone URL
#           $2 - branch: Target branch name
#           $3 - dest_dir: Local filesystem destination directory
# Example : clone_or_update_repo "https://github.com/dreamoutbox/pgvisor.git" "master" "/opt/pgvisor"
# ------------------------------------------------------------------------------
clone_or_update_repo() {
    local repo_url="$1"
    local branch="$2"
    local dest_dir="$3"

    if [ -d "${dest_dir}/.git" ]; then
        log_info "Repository already present at '${dest_dir}'. Checking out branch '${branch}'..."
        git -C "${dest_dir}" fetch origin "${branch}"
        git -C "${dest_dir}" checkout "${branch}"
        git -C "${dest_dir}" pull origin "${branch}"
    else
        log_info "Cloning repository from '${repo_url}' (branch: '${branch}') into '${dest_dir}'..."
        mkdir -p "$(dirname "${dest_dir}")"
        git clone --depth 1 --branch "${branch}" "${repo_url}" "${dest_dir}"
    fi
}

# ------------------------------------------------------------------------------
# Function: prepull_images
# Purpose : Pre-pull container images from Docker Hub to avoid slow on-droplet
#           Rust compilation and speed up cluster bootstrap.
# Inputs  : $1 - pgvisor_image: Image tag for PgVisor node and proxy
#           $2 - s3_image: Image tag for RustFS S3 storage
# Example : prepull_images "dreamoutbox/pgvisor:latest" "rustfs/rustfs:latest"
# ------------------------------------------------------------------------------
prepull_images() {
    local pgvisor_image="$1"
    local s3_image="$2"

    log_info "Pre-pulling Docker images to ensure fast bootstrap..."
    log_info "Pulling '${pgvisor_image}'..."
    docker pull "${pgvisor_image}" || log_warn "Could not pre-pull '${pgvisor_image}'; pgvisorctl will handle resolution."

    log_info "Pulling '${s3_image}'..."
    docker pull "${s3_image}" || log_warn "Could not pre-pull '${s3_image}'; docker compose will handle resolution."
}

# ------------------------------------------------------------------------------
# Function: configure_firewall
# Purpose : Open necessary PgVisor service ports (5432, 8080, 9000, 9001, 5000)
#           in UFW if the firewall is active on the host.
# Inputs  : None
# Example : configure_firewall
# ------------------------------------------------------------------------------
configure_firewall() {
    if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
        log_info "Active UFW firewall detected. Opening PgVisor service ports..."
        ufw allow 5432/tcp comment "pgvisor-postgres-proxy" >/dev/null 2>&1 || true
        ufw allow 8080/tcp comment "pgvisor-web-dashboard" >/dev/null 2>&1 || true
        ufw allow 9000/tcp comment "pgvisor-s3-api" >/dev/null 2>&1 || true
        ufw allow 9001/tcp comment "pgvisor-s3-console" >/dev/null 2>&1 || true
        ufw allow 5000/tcp comment "pgvisor-demo-app" >/dev/null 2>&1 || true
        log_info "UFW ports allowed: 5432, 8080, 9000, 9001, 5000."
    fi
}

# ------------------------------------------------------------------------------
# Function: detect_public_ip
# Purpose : Discover the host's public IPv4 address via DigitalOcean droplet
#           metadata service, external IP providers, or local network interfaces.
# Inputs  : None
# Example : ip=$(detect_public_ip)
# ------------------------------------------------------------------------------
detect_public_ip() {
    local detected_ip=""

    # 1. DigitalOcean droplet metadata endpoint
    detected_ip=$(curl -s --connect-timeout 2 http://169.254.169.254/metadata/v1/interfaces/public/0/ipv4/address 2>/dev/null || true)

    # 2. External public IP discovery fallbacks
    if [ -z "${detected_ip}" ]; then
        detected_ip=$(curl -s --connect-timeout 2 https://api.ipify.org 2>/dev/null || true)
    fi
    if [ -z "${detected_ip}" ]; then
        detected_ip=$(curl -s --connect-timeout 2 https://ifconfig.me 2>/dev/null || true)
    fi

    # 3. Local interface address fallback
    if [ -z "${detected_ip}" ] && command -v hostname >/dev/null 2>&1; then
        detected_ip=$(hostname -I 2>/dev/null | awk '{print $1}' || true)
    fi

    # 4. Default to localhost if discovery fails
    if [ -z "${detected_ip}" ]; then
        detected_ip="127.0.0.1"
    fi

    echo "${detected_ip}"
}

# ------------------------------------------------------------------------------
# Function: run_mock_data_seed
# Purpose : Execute dev-mock-data.sh to seed schema, insert demo rows, trigger
#           a full basebackup snapshot ('f1'), execute delayed inserts, and
#           create an incremental backup snapshot ('incr2') for demo readiness.
# Inputs  : $1 - base_dir: Path to cloned pgvisor root directory
# Example : run_mock_data_seed "/opt/pgvisor"
# ------------------------------------------------------------------------------
run_mock_data_seed() {
    local base_dir="$1"
    local seed_script="${base_dir}/dev-mock-data.sh"

    if [ ! -f "${seed_script}" ]; then
        log_warn "Demo seed script '${seed_script}' not found. Skipping data seeding."
        return 0
    fi

    log_info "Executing dev-mock-data.sh to seed schema and backup snapshots..."
    chmod +x "${seed_script}"

    if "${seed_script}" --delay=1 \
        --host="127.0.0.1" \
        --port="5432" \
        --dashboard-url="http://127.0.0.1:8080" \
        --token="postgres" \
        --container="pgvisor-node1"; then
        log_info "dev-mock-data.sh completed successfully."
    else
        log_warn "dev-mock-data.sh encountered non-fatal issues; cluster remains online."
    fi
}

# ------------------------------------------------------------------------------
# Function: generate_demo_showcase_summary
# Purpose : Generate formatted plain-text demo connection details, instructions,
#           and scenario walkthroughs. Writes to /etc/motd, DEMO_INFO.txt, and stdout.
# Inputs  : $1 - public_ip: External IPv4 address of the droplet
#           $2 - install_dir: Directory where the repository is cloned
# Example : generate_demo_showcase_summary "198.51.100.1" "/opt/pgvisor"
# ------------------------------------------------------------------------------
generate_demo_showcase_summary() {
    local public_ip="$1"
    local install_dir="$2"
    local info_file="${install_dir}/DEMO_INFO.txt"

    local summary
    summary=$(cat <<EOF
================================================================================
  PgVisor PostgreSQL HA Cluster - Demo Showcase Ready
================================================================================
  Droplet Public IP       : ${public_ip}
  Web Dashboard UI        : http://${public_ip}:8080
  Admin Bearer Token      : postgres
  PostgreSQL Proxy (L7)   : ${public_ip}:5432
  S3 / RustFS Console     : http://${public_ip}:9001
  S3 Root Credentials     : minioadmin / minioadmin

--------------------------------------------------------------------------------
  How to Connect to PostgreSQL (L7 Proxy with Read/Write Splitting)
--------------------------------------------------------------------------------
  psql Command Line:
    psql -h ${public_ip} -p 5432 -U postgres -d postgres

  Application Connection URI:
    postgresql://postgres:postgres@${public_ip}:5432/postgres

--------------------------------------------------------------------------------
  Demo Showcase Scenarios & Commands
--------------------------------------------------------------------------------
  1. Inspect Cluster Health & Topology:
     cd ${install_dir}/examples && ./pgvisorctl.sh status

  2. Stream Live Cluster Logs:
     cd ${install_dir}/examples && ./pgvisorctl.sh logs

  3. Query Seeded Mock Data (read queries split to standby replicas):
     psql -h ${public_ip} -p 5432 -U postgres -d postgres -c "SELECT * FROM pgvisor_demo;"

  4. Test Leader Failover & Zero-Downtime Request Buffering:
     docker stop pgvisor-node1
     (Open Web Dashboard to watch instant OpenRaft election & proxy routing)

  5. Recover / Rejoin the Fenced Node:
     docker start pgvisor-node1
     (Node 1 automatically rejoins the cluster as standby replica)

  6. Launch Python MVC Demo App (Optional):
     cd ${install_dir}/examples && ./run-demo-app.sh up
     (Access live app at http://${public_ip}:5000)

  7. Re-run Mock Data & Snapshot Generation:
     ${install_dir}/dev-mock-data.sh

  8. Stop or Clean the Cluster:
     cd ${install_dir}/examples && ./pgvisorctl.sh down
     cd ${install_dir}/examples && ./pgvisorctl.sh clean -y
================================================================================
EOF
)

    # Output to stdout
    echo ""
    echo "${summary}"
    echo ""

    # Persist summary file inside installation directory
    echo "${summary}" > "${info_file}"
    log_info "Saved demo connection summary to '${info_file}'."

    # Persist summary file in /root if accessible
    if [ -d "/root" ] && [ -w "/root" ]; then
        echo "${summary}" > "/root/DEMO_INFO.txt"
    fi

    # Update system MOTD so SSH login displays the demo information immediately
    if [ -w "/etc/motd" ] || [ ! -e "/etc/motd" ]; then
        echo "${summary}" > /etc/motd 2>/dev/null || true
    fi
}

usage() {
    echo "Usage: ./demo-launcher-entrypoint.sh [options]"
    echo ""
    echo "DigitalOcean Droplet startup and demo showcase launcher for PgVisor."
    echo ""
    echo "Options:"
    echo "  --repo=URL           Git repository URL (default: https://github.com/dreamoutbox/pgvisor.git)"
    echo "  --branch=BRANCH      Git branch to checkout (default: master)"
    echo "  --dir=PATH           Target installation directory (default: /opt/pgvisor)"
    echo "  --image=IMAGE        PgVisor Docker image (default: dreamoutbox/pgvisor:latest)"
    echo "  --skip-seed          Skip running dev-mock-data.sh for mock data and snapshots"
    echo "  --with-demo-app      Also launch Python MVC demo web app on port 5000"
    echo "  --build              Force building Docker images from source"
    echo "  -h, --help           Show this help message"
}

# Parse command line options
while [ $# -gt 0 ]; do
    case "$1" in
        --repo=*)
            REPO_URL="${1#*=}"
            shift
            ;;
        --branch=*)
            REPO_BRANCH="${1#*=}"
            shift
            ;;
        --dir=*)
            INSTALL_DIR="${1#*=}"
            shift
            ;;
        --image=*)
            PGVISOR_IMAGE="${1#*=}"
            shift
            ;;
        --skip-seed)
            SEED_DEMO_DATA=false
            shift
            ;;
        --with-demo-app)
            START_DEMO_APP=true
            shift
            ;;
        --build)
            DO_BUILD=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option '$1'" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# Main bootstrap execution
log_info "================================================================"
log_info "  PgVisor Demo Launcher Initialization"
log_info "  Target Repository: ${REPO_URL} (${REPO_BRANCH})"
log_info "  Installation Path: ${INSTALL_DIR}"
log_info "================================================================"

setup_swap_if_needed
install_prerequisites
install_docker
clone_or_update_repo "${REPO_URL}" "${REPO_BRANCH}" "${INSTALL_DIR}"

if [ "${DO_BUILD}" = false ]; then
    prepull_images "${PGVISOR_IMAGE}" "${S3_IMAGE}"
fi

log_info "Navigating to '${INSTALL_DIR}/examples'..."
cd "${INSTALL_DIR}/examples"

chmod +x pgvisorctl.sh
if [ "${DO_BUILD}" = true ]; then
    log_info "Launching PgVisor cluster with --build..."
    ./pgvisorctl.sh up --build
else
    log_info "Launching PgVisor cluster..."
    ./pgvisorctl.sh up
fi

if [ "${START_DEMO_APP}" = true ]; then
    if [ -f "./run-demo-app.sh" ]; then
        log_info "Launching Python MVC demo web application on port 5000..."
        chmod +x run-demo-app.sh
        ./run-demo-app.sh up
    fi
fi

configure_firewall

if [ "${SEED_DEMO_DATA}" = true ]; then
    run_mock_data_seed "${INSTALL_DIR}"
fi

PUBLIC_IP=$(detect_public_ip)
generate_demo_showcase_summary "${PUBLIC_IP}" "${INSTALL_DIR}"

log_info "PgVisor Demo Showcase setup completed successfully."
