#!/usr/bin/env bash
# Installs the Upalla desktop launcher for the current user.
# Places the icon in the hicolor icon theme and the .desktop file
# in the applications directory, then refreshes the desktop database.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

ICON_SRC="$ROOT_DIR/upalla-slint/src/icon_256.png"
DESKTOP_SRC="$ROOT_DIR/packaging/com.upalla.denoiser.desktop"

ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
APPS_DIR="$HOME/.local/share/applications"

mkdir -p "$ICON_DIR" "$APPS_DIR"

cp "$ICON_SRC" "$ICON_DIR/com.upalla.denoiser.png"
cp "$DESKTOP_SRC" "$APPS_DIR/com.upalla.denoiser.desktop"

# Refresh caches if the tooling is available (non-fatal)
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
fi

echo "Installed Upalla launcher to $APPS_DIR"
echo "Icon installed to $ICON_DIR/com.upalla.denoiser.png"
echo
echo "Make sure 'upalla-slint' is on your PATH (e.g. ~/.cargo/bin/upalla-slint)."
