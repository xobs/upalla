#!/bin/bash
# Build and install the Upalla LV2 plugin bundle
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LV2_DIR="${LV2_DIR:-$HOME/.lv2}"
BUNDLE_DIR="$LV2_DIR/upalla.lv2"

echo "=== Upalla LV2 Plugin Install ==="
echo ""

echo "Step 1: Building plugin..."
cargo build -p upalla-lv2 --release

echo ""
echo "Step 2: Installing LV2 bundle to $BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR"

SO_FILE="$PROJECT_DIR/target/release/libupalla_lv2.so"
if [ ! -f "$SO_FILE" ]; then
    echo "Error: built library not found at $SO_FILE"
    exit 1
fi

cp "$SO_FILE" "$BUNDLE_DIR/upalla.so"

echo ""
echo "Step 3: Generating manifest..."

# Find the LV2 URI from the built binary
LV2_URI=$(strings "$BUNDLE_DIR/upalla.so" | grep -E '^https?://|^urn:' | head -1 || echo "")
if [ -z "$LV2_URI" ]; then
    LV2_URI="urn:com.upalla:Upalla"
fi

cat > "$BUNDLE_DIR/manifest.ttl" << TTL
@prefix lv2:  <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

<$LV2_URI>
    a lv2:Plugin ;
    lv2:binary <upalla.so> ;
    rdfs:seeAlso <upalla.ttl> .
TTL

echo ""
echo "=== Installation Complete ==="
echo ""
echo "LV2 URI: $LV2_URI"
echo "Bundle:  $BUNDLE_DIR"
echo ""
echo "To use with PipeWire, update config/upalla.conf:"
echo "  Replace REPLACE_WITH_UPALLA_LV2_URI with:"
echo "    $LV2_URI"
echo ""
echo "Then copy the config and restart PipeWire:"
echo "  cp config/upalla.conf ~/.config/pipewire/pipewire.conf.d/99-upalla.conf"
echo "  systemctl restart --user pipewire"
echo ""
echo "To verify the plugin was found:"
echo "  lv2ls 2>/dev/null | grep -i upalla"
