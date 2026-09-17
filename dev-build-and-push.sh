#!/usr/bin/env bash
set -euo pipefail

TAG="${TAG:-latest}"

if [[ $# -eq 3 ]]; then
    IMAGE="$1"
    DOCKER_USER="$2"
    DOCKER_PAT="$3"
elif [[ $# -eq 0 ]]; then
    read -rp "Docker image (<username>/<image>): " IMAGE
    read -rp "Docker login user: " DOCKER_USER
    read -rsp "Docker PAT: " DOCKER_PAT
    echo
else
    echo "Usage: $0 <docker-username>/<image> <docker-login-user> <docker-PAT>" >&2
    echo "   or: $0   (no args — prompts interactively)" >&2
    exit 1
fi

echo "$DOCKER_PAT" | docker login -u "$DOCKER_USER" --password-stdin

docker build -t "${IMAGE}:${TAG}" .
docker push "${IMAGE}:${TAG}"

docker logout
