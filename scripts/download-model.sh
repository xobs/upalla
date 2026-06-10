#!/bin/bash
# Download the DeepFilterNet3 ONNX model for Upalla
set -euo pipefail

MODEL_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/upalla"
MODEL_URL="${UPALLA_MODEL_URL:-https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz}"

echo "=== Upalla Model Download ==="
echo "Model dir: $MODEL_DIR"
echo ""

mkdir -p "$MODEL_DIR"

if [ -f "$MODEL_DIR/deepfilter.onnx" ]; then
    echo "Model already installed at $MODEL_DIR/deepfilter.onnx"
    echo "Remove it to re-download: rm $MODEL_DIR/deepfilter.onnx"
    exit 0
fi

echo "Downloading DeepFilterNet3 ONNX model..."
TMPFILE=$(mktemp)
trap 'rm -f "$TMPFILE"' EXIT

if command -v curl &> /dev/null; then
    curl -L --progress-bar "$MODEL_URL" -o "$TMPFILE"
elif command -v wget &> /dev/null; then
    wget -q --show-progress "$MODEL_URL" -O "$TMPFILE"
else
    echo "Error: curl or wget required to download the model"
    exit 1
fi

echo "Extracting..."
tar xzf "$TMPFILE" -C "$MODEL_DIR"

if [ -f "$MODEL_DIR/deepfilter.onnx" ]; then
    echo "Model installed successfully: $MODEL_DIR/deepfilter.onnx"
else
    echo "Extracted files:"
    find "$MODEL_DIR" -name "*.onnx" -exec echo "  {}" \;
    echo ""
    echo "If no deepfilter.onnx file was found, the tarball may have a different structure."
    echo "Create a symlink or copy the .onnx file to: $MODEL_DIR/deepfilter.onnx"
fi
