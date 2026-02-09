#!/usr/bin/env bash
# build_local_msi.sh — Build Setu MSI installer from WSL.
#
# Prerequisites:
#   sudo apt install msitools    # provides wixl
#   # Optional for signing:
#   sudo apt install osslsigncode
#
# Usage:
#   ./build_local_msi.sh              # build MSI
#   ./build_local_msi.sh --sign       # build + sign with local .pfx

set -euo pipefail

VERSION="${VERSION:-0.1.2}"
TARGET="x86_64-pc-windows-gnu"
RELEASE_DIR="target/${TARGET}/release"
MSI_OUT="setu-${VERSION}.msi"
PFX_FILE="${PFX_FILE:-setu-selfsigned.pfx}"
PFX_PASS="${PFX_PASS:-changeit}"

echo "==> Building Setu v${VERSION} for ${TARGET}..."
cargo build --release --target "${TARGET}"

if [ ! -f "${RELEASE_DIR}/setu.exe" ]; then
    echo "ERROR: ${RELEASE_DIR}/setu.exe not found" >&2
    exit 1
fi

echo "==> Generating MSI: ${MSI_OUT}..."
wixl -v \
    -D "Version=${VERSION}" \
    -o "${MSI_OUT}" \
    setu.wxs

echo "==> MSI created: ${MSI_OUT} ($(du -h "${MSI_OUT}" | cut -f1))"

# ── Optional: self-sign with osslsigncode ────────────────────────
if [ "${1:-}" = "--sign" ]; then
    if ! command -v osslsigncode &>/dev/null; then
        echo "ERROR: osslsigncode not found. Install: sudo apt install osslsigncode" >&2
        exit 1
    fi

    if [ ! -f "${PFX_FILE}" ]; then
        echo "==> No PFX found at ${PFX_FILE}. Generating self-signed certificate..."
        openssl req -x509 -newkey rsa:2048 -keyout /tmp/setu-key.pem -out /tmp/setu-cert.pem \
            -days 365 -nodes -subj "/CN=Setu Dev/O=Setu Dev"
        openssl pkcs12 -export -out "${PFX_FILE}" \
            -inkey /tmp/setu-key.pem -in /tmp/setu-cert.pem -passout "pass:${PFX_PASS}"
        rm -f /tmp/setu-key.pem /tmp/setu-cert.pem
        echo "==> Self-signed PFX created: ${PFX_FILE}"
    fi

    SIGNED_MSI="setu-${VERSION}-signed.msi"
    osslsigncode sign \
        -pkcs12 "${PFX_FILE}" \
        -pass "${PFX_PASS}" \
        -n "Setu" \
        -t http://timestamp.digicert.com \
        -in "${MSI_OUT}" \
        -out "${SIGNED_MSI}"

    echo "==> Signed MSI: ${SIGNED_MSI}"
fi

echo "==> Done."
