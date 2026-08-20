#!/usr/bin/env sh
set -eu

ENABLE_TAILSCALE_SERVE=${COURIER_ENABLE_TAILSCALE_SERVE:-0}
case "${1:-}" in
  --tailscale)
    ENABLE_TAILSCALE_SERVE=1
    ;;
  "")
    ;;
  *)
    printf '%s\n' "Usage: $0 [--tailscale]" >&2
    exit 2
    ;;
esac
case "$ENABLE_TAILSCALE_SERVE" in
  1|true|yes)
    ENABLE_TAILSCALE_SERVE=1
    ;;
  0|false|no|"")
    ENABLE_TAILSCALE_SERVE=0
    ;;
  *)
    printf '%s\n' "COURIER_ENABLE_TAILSCALE_SERVE must be 1, 0, true, false, yes, or no." >&2
    exit 2
    ;;
esac

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

GATEWAY_ADDRESS=$($COMPOSE port courier-gateway 8080 | tail -n 1)
REGISTRY_PORT=${GATEWAY_ADDRESS##*:}
printf '%s\n' "Registry ready: http://127.0.0.1:${REGISTRY_PORT}/docs"

if [ "$ENABLE_TAILSCALE_SERVE" -eq 1 ]; then
  if ! command -v tailscale >/dev/null 2>&1; then
    printf '%s\n' "Tailscale Serve requested, but the tailscale CLI is not installed." >&2
    exit 1
  fi
  if ! tailscale status >/dev/null 2>&1; then
    printf '%s\n' "Tailscale Serve requested, but this host is not connected to a tailnet." >&2
    exit 1
  fi
  VPN_GATEWAY_ADDRESS=$($COMPOSE port courier-gateway 8081 | tail -n 1)
  VPN_REGISTRY_PORT=${VPN_GATEWAY_ADDRESS##*:}
  TAILSCALE_HTTP_PORT=${COURIER_TAILSCALE_HTTP_PORT:-80}
  if ! tailscale serve --bg --http="$TAILSCALE_HTTP_PORT" \
    "http://127.0.0.1:${VPN_REGISTRY_PORT}"; then
    printf '%s\n' "Could not configure Headscale Serve. If elevated access is required, run:" >&2
    printf '  sudo tailscale serve --bg --http=%s http://127.0.0.1:%s\n' \
      "$TAILSCALE_HTTP_PORT" "$VPN_REGISTRY_PORT" >&2
    exit 1
  fi
  tailscale serve status
  printf 'Disable later with: tailscale serve --http=%s off\n' "$TAILSCALE_HTTP_PORT"
fi

printf '%s\n' "Run ./scripts/dev-e2e.sh for a complete manifest and object-upload smoke test."
