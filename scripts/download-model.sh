#!/bin/bash
# Download the DeepFilterNet3 ONNX model for Upalla
set -euo pipefail

MODEL_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/upalla"
MODEL_URL="${UPALLA_MODEL_URL:-https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz}"

echo "=== Upalla Model Download ==="
echo "Model dir: $MODEL_DIR"
echo ""

if [ -f "$MODEL_DIR/enc.onnx" ] && [ -f "$MODEL_DIR/erb_dec.onnx" ] && [ -f "$MODEL_DIR/df_dec.onnx" ]; then
    echo "Model already installed (enc.onnx, erb_dec.onnx, df_dec.onnx found)"
    exit 0
fi

mkdir -p "$MODEL_DIR"

echo "Downloading DeepFilterNet3 ONNX model..."
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

if command -v curl &> /dev/null; then
    curl -L --progress-bar "$MODEL_URL" -o "$TMPDIR/model.tar.gz"
elif command -v wget &> /dev/null; then
    wget -q --show-progress "$MODEL_URL" -O "$TMPDIR/model.tar.gz"
else
    echo "Error: curl or wget required to download the model"
    exit 1
fi

echo "Extracting..."
tar xzf "$TMPDIR/model.tar.gz" -C "$TMPDIR"

# Find the .onnx files wherever they are in the extracted tree
while IFS= read -r onnx_file; do
    base=$(basename "$onnx_file")
    cp "$onnx_file" "$MODEL_DIR/$base"
    echo "  Installed: $base"
done < <(find "$TMPDIR" -name "*.onnx")

if [ -f "$MODEL_DIR/enc.onnx" ]; then
    echo ""
    echo "Model installed successfully:"
    ls -lh "$MODEL_DIR"/*.onnx
else
    echo ""
    echo "Warning: No .onnx files found in the download."
    echo "Extracted contents:"
    find "$TMPDIR" -type f -name "*.onnx" -o -name "*.tar.gz" | head -20
fi
