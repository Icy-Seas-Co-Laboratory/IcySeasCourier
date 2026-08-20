#!/usr/bin/env sh
set -eu

REPOSITORY_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPOSITORY_DIR"

if docker compose version >/dev/null 2>&1; then
  COMPOSE="docker compose"
else
  COMPOSE="docker-compose"
fi

if docker buildx version >/dev/null 2>&1; then
  $COMPOSE up --build -d postgres seaweedfs data-registry courier-gateway ingest-worker
else
  DOCKER_BUILDKIT=0 $COMPOSE up --build -d postgres seaweedfs data-registry courier-gateway ingest-worker
fi

printf '%s\n' "Waiting for Icy Seas Data Registry..."
attempt=0
until $COMPOSE exec -T data-registry python -c \
  "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8010/ready')" >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 40 ]; then
    $COMPOSE logs --tail=100 data-registry
    exit 1
  fi
  sleep 1
done

REGISTRY_PORT=${REGISTRY_PORT:-8020}
printf '%s\n' "Registry ready: http://127.0.0.1:${REGISTRY_PORT}/docs"
printf '%s\n' "Run ./scripts/dev-e2e.sh for a complete manifest and object-upload smoke test."
