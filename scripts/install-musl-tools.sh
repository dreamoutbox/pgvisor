#!/usr/bin/env bash
set -euo pipefail

# Install musl-tools for static x86_64-unknown-linux-musl builds on Ubuntu runners.
sudo apt-get update
sudo apt-get install -y --no-install-recommends musl-tools
