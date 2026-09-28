#!/bin/bash
# sign-macos.sh <binary> <out.zip>: signs the binary with a Developer ID (hardened runtime, secure
# timestamp) and notarizes it when the signing secrets are set (CI), else signs it ad hoc and names
# the zip -unsigned. Locally: MACOS_SIGN_IDENTITY, and NOTARY_PROFILE (a notarytool keychain profile).
set -euo pipefail
bin=$1 out=$2
dir=$(dirname "$bin")
if [ -n "${MACOS_CERTIFICATE:-}" ]; then
  kc="$RUNNER_TEMP/signing.keychain-db" kcpw=$(uuidgen)
  echo "$MACOS_CERTIFICATE" | base64 --decode > "$RUNNER_TEMP/cert.p12"
  security create-keychain -p "$kcpw" "$kc"
  security set-keychain-settings -lut 21600 "$kc"
  security unlock-keychain -p "$kcpw" "$kc"
  security import "$RUNNER_TEMP/cert.p12" -k "$kc" -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
  security set-key-partition-list -S apple-tool:,apple: -k "$kcpw" "$kc" >/dev/null
  security list-keychains -d user -s "$kc" $(security list-keychains -d user | tr -d '"')
fi
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$bin"
  codesign --verify --strict "$bin"
  (cd "$dir" && zip -q "$(basename "$out")" "$(basename "$bin")")
  if [ -n "${APPLE_API_KEY:-}" ]; then
    echo "$APPLE_API_KEY" > "$RUNNER_TEMP/key.p8"
    xcrun notarytool submit "$out" --key "$RUNNER_TEMP/key.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" --wait
  elif [ -n "${NOTARY_PROFILE:-}" ]; then
    xcrun notarytool submit "$out" --keychain-profile "$NOTARY_PROFILE" --wait
  else
    echo "signed, not notarized"
  fi
else
  codesign --force --sign - "$bin"
  (cd "$dir" && zip -q "$(basename "${out%.zip}")-unsigned.zip" "$(basename "$bin")")
  echo "no signing identity: ad hoc (Gatekeeper warns)"
fi
