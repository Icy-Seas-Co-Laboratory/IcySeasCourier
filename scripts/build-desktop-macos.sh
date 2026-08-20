#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop_directory="$repository_root/apps/courier-desktop"

signing_identity="${APPLE_SIGNING_IDENTITY:-}"
if [[ -z "$signing_identity" ]]; then
  signing_identity="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' \
    | sed -n '1p')"
fi

signing_kind="distribution"
if [[ -z "$signing_identity" ]]; then
  signing_identity="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Apple Development:.*\)"/\1/p' \
    | sed -n '1p')"
  signing_kind="development"
fi

if [[ -z "$signing_identity" ]]; then
  cat >&2 <<'EOF'
No usable macOS code-signing identity was found.

A .cer file contains only the public certificate. Import the matching private
key, normally by exporting and importing a password-protected .p12 from the Mac
that created the certificate request. Then verify it with:

  security find-identity -v -p codesigning
EOF
  exit 1
fi

export APPLE_SIGNING_IDENTITY="$signing_identity"
echo "Signing local Courier build with: $APPLE_SIGNING_IDENTITY"
if [[ "$signing_kind" == "development" ]]; then
  cat >&2 <<'EOF'
Warning: this is an Apple Development identity. The resulting build is for
local testing only and is not suitable for external distribution or Apple
notarization. Release builds require a Developer ID Application identity.
EOF
fi

cd "$desktop_directory"
npm run tauri -- build --bundles dmg "$@"
