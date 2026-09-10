#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop_directory="$repository_root/apps/courier-desktop"
notarize="${COURIER_NOTARIZE:-0}"

if [[ "$notarize" != "0" && "$notarize" != "1" ]]; then
  echo "COURIER_NOTARIZE must be either 0 or 1." >&2
  exit 1
fi

signing_identity="${APPLE_SIGNING_IDENTITY:-}"
if [[ -z "$signing_identity" ]]; then
  signing_identity="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' \
    | sed -n '1p')"
fi

signing_kind="distribution"
if [[ -z "$signing_identity" ]]; then
  if [[ "$notarize" == "1" ]]; then
    cat >&2 <<'EOF'
No Developer ID Application signing identity was found.

Notarized builds require the Developer ID certificate and its matching private
key to be imported into the login keychain. A .cer file alone is not enough.
EOF
    exit 1
  fi

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

if [[ "$notarize" == "1" ]]; then
  notarization_settings="$repository_root/.signing/apple/notarization.env"
  if [[ -z "${APPLE_API_ISSUER:-}" && -r "$notarization_settings" ]]; then
    APPLE_API_ISSUER="$(sed -n 's/^APPLE_API_ISSUER=//p' "$notarization_settings" | sed -n '1p')"
  fi

  if [[ -z "${APPLE_API_KEY:-}" ]]; then
    api_key_candidates=("$repository_root"/.signing/apple/AuthKey_*.p8)
    if [[ ${#api_key_candidates[@]} -ne 1 || ! -f "${api_key_candidates[0]}" ]]; then
      cat >&2 <<'EOF'
APPLE_API_KEY must be set to the App Store Connect API key ID when there is not
exactly one AuthKey_<key-id>.p8 file in .signing/apple/.
EOF
      exit 1
    fi
    api_key_filename="$(basename "${api_key_candidates[0]}")"
    APPLE_API_KEY="${api_key_filename#AuthKey_}"
    APPLE_API_KEY="${APPLE_API_KEY%.p8}"
  fi
  : "${APPLE_API_ISSUER:?APPLE_API_ISSUER must be set to the App Store Connect API issuer ID.}"

  if [[ -z "${APPLE_API_KEY_PATH:-}" ]]; then
    APPLE_API_KEY_PATH="$repository_root/.signing/apple/AuthKey_${APPLE_API_KEY}.p8"
  fi
  if [[ ! -r "$APPLE_API_KEY_PATH" ]]; then
    cat >&2 <<EOF
The App Store Connect private key is not readable at:
  $APPLE_API_KEY_PATH

Set APPLE_API_KEY_PATH to the AuthKey_<key-id>.p8 file, or place that file in
.signing/apple/. The key file must remain untracked.
EOF
    exit 1
  fi

  export APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
fi

cd "$desktop_directory"
npm run tauri -- build --bundles dmg "$@"

if [[ "$notarize" == "1" ]]; then
  app_path="$desktop_directory/src-tauri/target/release/bundle/macos/Icy Seas Courier.app"
  if [[ ! -d "$app_path" ]]; then
    echo "Expected built app was not found at: $app_path" >&2
    exit 1
  fi

  codesign --verify --deep --strict --verbose=2 "$app_path"
  spctl --assess --type execute --verbose=2 "$app_path"
  xcrun stapler validate "$app_path"
fi
