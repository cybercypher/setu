#!/usr/bin/env bash
# build_appimage.sh — Build Setu AppImage for Linux.
#
# Prerequisites:
#   - Rust toolchain (native Linux target)
#   - wget or curl (to download appimagetool if not present)
#   - FUSE (for running the resulting AppImage)
#
# Usage:
#   ./build_appimage.sh

set -euo pipefail

VERSION="${VERSION:-0.1.2}"
ARCH="x86_64"
RELEASE_DIR="target/release"
APPDIR="Setu.AppDir"
APPIMAGE_OUT="Setu-${ARCH}.AppImage"
APPIMAGETOOL="appimagetool-${ARCH}.AppImage"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/${APPIMAGETOOL}"

echo "==> Building Setu v${VERSION} for Linux ${ARCH}..."
cargo build --release

if [ ! -f "${RELEASE_DIR}/setu" ]; then
    echo "ERROR: ${RELEASE_DIR}/setu not found" >&2
    exit 1
fi

echo "==> Creating AppDir structure..."
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"

# Copy binary
cp "${RELEASE_DIR}/setu" "${APPDIR}/usr/bin/setu"

# Copy desktop file and icon
cp setu.desktop "${APPDIR}/usr/share/applications/setu.desktop"
cp assets/icon_preview.png "${APPDIR}/usr/share/icons/hicolor/256x256/apps/setu.png"

# AppDir root requires: AppRun, .desktop, icon
ln -sf usr/bin/setu "${APPDIR}/AppRun"
ln -sf usr/share/applications/setu.desktop "${APPDIR}/setu.desktop"
ln -sf usr/share/icons/hicolor/256x256/apps/setu.png "${APPDIR}/setu.png"

# Download appimagetool if not present
if [ ! -f "${APPIMAGETOOL}" ]; then
    echo "==> Downloading appimagetool..."
    if command -v wget &>/dev/null; then
        wget -q "${APPIMAGETOOL_URL}" -O "${APPIMAGETOOL}"
    elif command -v curl &>/dev/null; then
        curl -fsSL "${APPIMAGETOOL_URL}" -o "${APPIMAGETOOL}"
    else
        echo "ERROR: wget or curl required to download appimagetool" >&2
        exit 1
    fi
    chmod +x "${APPIMAGETOOL}"
fi

echo "==> Building AppImage..."
ARCH="${ARCH}" ./"${APPIMAGETOOL}" "${APPDIR}" "${APPIMAGE_OUT}"

echo "==> AppImage created: ${APPIMAGE_OUT} ($(du -h "${APPIMAGE_OUT}" | cut -f1))"
echo "==> Done."
