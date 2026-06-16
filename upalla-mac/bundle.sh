#!/usr/bin/env bash
set -euo pipefail

# Build and bundle upalla-mac into a .app for macOS.
# Run from the workspace root (upalla/).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

APP_NAME="Upalla"
BUNDLE_DIR="${WORKSPACE_DIR}/target/${APP_NAME}.app"
TARGET="aarch64-apple-darwin"
BINARY="${WORKSPACE_DIR}/target/${TARGET}/debug/upalla-mac"

echo "=== Building ${TARGET} ==="
(cd "${WORKSPACE_DIR}" && SDKROOT="${SDKROOT:-${HOME}/Code/MacOSX15.5.sdk}" cargo zigbuild --target "${TARGET}" -p upalla-mac)

echo "=== Creating ${BUNDLE_DIR} ==="
rm -rf "${BUNDLE_DIR}"
mkdir -p "${BUNDLE_DIR}/Contents/MacOS"
mkdir -p "${BUNDLE_DIR}/Contents/Resources"

cp "${BINARY}" "${BUNDLE_DIR}/Contents/MacOS/upalla-mac"
chmod +x "${BUNDLE_DIR}/Contents/MacOS/upalla-mac"

# ---- AppIcon.icns ----
echo "=== Generating AppIcon.icns ==="
python3 -c '
import struct, io
from PIL import Image
from pathlib import Path

src = Path("'"${SCRIPT_DIR}"'/src/icon_256.png")
img = Image.open(src).convert("RGBA")

# Small sizes: raw ARGB (traditional icns format).
# Large sizes: PNG (modern icns format).
ARGB_SIZES = [
    ("icp5", 32),
    ("icp6", 64),
]
PNG_SIZES = [
    ("ic07", 128),
    ("ic08", 256),
]

entries = []
for code, size in ARGB_SIZES:
    resized = img.resize((size, size), Image.LANCZOS)
    # Pack as raw ARGB (A,R,G,B per pixel, row-major, top-first)
    argb = bytearray()
    for y in range(size):
        for x in range(size):
            r, g, b, a = resized.getpixel((x, y))
            argb.extend([a, r, g, b])
    entry_size = 8 + len(argb)
    entries.append(struct.pack(">4sI", code.encode(), entry_size) + bytes(argb))

for code, size in PNG_SIZES:
    if size == 256:
        buf = src.read_bytes()
    else:
        resized = img.resize((size, size), Image.LANCZOS)
        tmp = io.BytesIO()
        resized.save(tmp, format="PNG")
        buf = tmp.getvalue()
    entry_size = 8 + len(buf)
    entries.append(struct.pack(">4sI", code.encode(), entry_size) + buf)

total = 8 + sum(len(e) for e in entries)
icns = struct.pack(">4sI", b"icns", total) + b"".join(entries)

dest = Path("'"${BUNDLE_DIR}"'/Contents/Resources/AppIcon.icns")
dest.parent.mkdir(parents=True, exist_ok=True)
dest.write_bytes(icns)
print(f"  {dest} ({total} bytes, {len(ARGB_SIZES) + len(PNG_SIZES)} sizes)")
'

# ---- Info.plist ----
cat > "${BUNDLE_DIR}/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN"
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>upalla-mac</string>
    <key>CFBundleIdentifier</key>
    <string>ai.upalla.app</string>
    <key>CFBundleName</key>
    <string>Upalla</string>
    <key>CFBundleDisplayName</key>
    <string>Upalla</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Upalla needs microphone access to process and filter your voice in real time.</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
</dict>
</plist>
PLIST

echo "=== Bundle created at ${BUNDLE_DIR} ==="
echo "Copy to Mac and run:  open ${APP_NAME}.app"
echo "Or launch directly:   ${APP_NAME}.app/Contents/MacOS/upalla-mac"
