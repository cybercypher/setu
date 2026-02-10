#!/usr/bin/env bash
# build_appimage.sh — Build Setu AppImage for Linux using linuxdeploy.
#
# linuxdeploy automatically bundles all shared library dependencies
# (GTK3, libdbus, libxdo, etc.) so the AppImage is self-contained.
#
# Prerequisites:
#   - Rust toolchain (native Linux target)
#   - Build deps: libgtk-3-dev libdbus-1-dev libxdo-dev pkg-config
#   - curl (to download linuxdeploy if not present)
#
# Usage:
#   ./build_appimage.sh

set -euo pipefail

ARCH="x86_64"
RELEASE_DIR="target/release"
APPIMAGE_OUT="Setu-${ARCH}.AppImage"
LINUXDEPLOY="linuxdeploy-${ARCH}.AppImage"
LINUXDEPLOY_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/${LINUXDEPLOY}"
GTK_PLUGIN="linuxdeploy-plugin-gtk.sh"
GTK_PLUGIN_URL="https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/${GTK_PLUGIN}"

echo "==> Building Setu for Linux ${ARCH}..."
cargo build --release

if [ ! -f "${RELEASE_DIR}/setu" ]; then
    echo "ERROR: ${RELEASE_DIR}/setu not found" >&2
    exit 1
fi

# Download linuxdeploy if not present
if [ ! -f "${LINUXDEPLOY}" ]; then
    echo "==> Downloading linuxdeploy..."
    curl -fsSL "${LINUXDEPLOY_URL}" -o "${LINUXDEPLOY}"
    chmod +x "${LINUXDEPLOY}"
fi

# Download GTK plugin if not present
if [ ! -f "${GTK_PLUGIN}" ]; then
    echo "==> Downloading linuxdeploy GTK plugin..."
    curl -fsSL "${GTK_PLUGIN_URL}" -o "${GTK_PLUGIN}"
    chmod +x "${GTK_PLUGIN}"
fi

echo "==> Building AppImage with bundled dependencies..."
rm -rf Setu.AppDir

OUTPUT="${APPIMAGE_OUT}" \
DEPLOY_GTK_VERSION=3 \
./"${LINUXDEPLOY}" \
    --appdir Setu.AppDir \
    --executable "${RELEASE_DIR}/setu" \
    --desktop-file setu.desktop \
    --icon-file assets/setu.png \
    --plugin gtk \
    --output appimage

echo "==> AppImage created: ${APPIMAGE_OUT} ($(du -h "${APPIMAGE_OUT}" | cut -f1))"
echo "==> Done."
