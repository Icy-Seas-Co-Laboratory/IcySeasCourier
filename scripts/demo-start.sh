#!/usr/bin/env sh
set -eu

REPOSITORY_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SAMPLE_DIR="$REPOSITORY_DIR/.courier/demo-dataset"
LAUNCH=1

if [ "${1:-}" = "--no-launch" ]; then
  LAUNCH=0
elif [ -n "${1:-}" ]; then
  printf '%s\n' "Usage: ./scripts/demo-start.sh [--no-launch]" >&2
  exit 2
fi

command -v docker >/dev/null 2>&1 || { printf '%s\n' "Docker is required." >&2; exit 1; }
command -v npm >/dev/null 2>&1 || { printf '%s\n' "npm is required." >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { printf '%s\n' "Cargo is required." >&2; exit 1; }

mkdir -p "$SAMPLE_DIR/metadata" "$SAMPLE_DIR/raw" "$SAMPLE_DIR/derived"
if [ ! -f "$SAMPLE_DIR/README.md" ]; then
  cp "$REPOSITORY_DIR/scripts/demo-data/README.md" "$SAMPLE_DIR/README.md"
  cp "$REPOSITORY_DIR/scripts/demo-data/station-metadata.csv" "$SAMPLE_DIR/metadata/station-metadata.csv"
  cp "$REPOSITORY_DIR/scripts/demo-data/ctd-cast.csv" "$SAMPLE_DIR/raw/ctd-cast.csv"
  cp "$REPOSITORY_DIR/scripts/demo-data/profile-summary.csv" "$SAMPLE_DIR/derived/profile-summary.csv"
fi

"$REPOSITORY_DIR/scripts/dev-up.sh"
INVITATION=$("$REPOSITORY_DIR/scripts/dev-invite.sh" | tail -n 1)

printf '\n%s\n' "============================================================"
printf '%s\n' " ICY SEAS COURIER — LIVE DEMO READY"
printf '%s\n' "============================================================"
printf '%s\n' "Invitation:  $INVITATION"
printf '%s\n' "Sample data: $SAMPLE_DIR"
printf '%s\n' "Registry:    http://127.0.0.1:${REGISTRY_PORT:-8010}/docs"
printf '%s\n' "Admin:       http://127.0.0.1:${REGISTRY_PORT:-8010}/admin/"
printf '%s\n' "Admin key:   ${REGISTRY_ADMIN_API_KEY:-development-only-change-me} (local demo only)"
printf '\n%s\n' "Presenter flow (about 3 minutes):"
printf '%s\n' "  1. Paste the invitation above and click Continue."
printf '%s\n' "  2. Keep project P26014 and browse to the sample data path."
printf '%s\n' "  3. Review the inventory, provenance, and SHA-256 language."
printf '%s\n' "  4. Upload; point out durable progress and pause/resume."
printf '%s\n' "  5. Watch Finalizing -> Verifying -> Complete."
printf '%s\n' "  6. Open Transfers to show the durable local history and Registry ID."
printf '\n%s\n' "Talking point: complete means the Registry independently reconstructed"
printf '%s\n' "and matched every logical file to the immutable manifest."
printf '%s\n' "============================================================"

if [ "$LAUNCH" -eq 0 ]; then
  printf '\n%s\n' "Launch later with: cd apps/courier-desktop && npm run tauri dev"
  exit 0
fi

cd "$REPOSITORY_DIR/apps/courier-desktop"
if [ ! -d node_modules ]; then
  npm install
fi
exec npm run tauri dev
