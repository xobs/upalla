# Upalla

GPU-agnostic real-time noise suppression for Linux using **DeepFilterNet3**.

Works with NVIDIA, AMD, Intel GPUs — or CPU only — via ONNX Runtime.

## Architecture

```
Microphone → PipeWire → Upalla LV2 Plugin → ONNX Runtime → Denoised Audio
                              ↕                    ↕
                         Audio Thread          Worker Thread
                         (real-time)       (GPU inference off-RT)
```

## Building

```bash
# Prerequisites
# Rust 1.92+, libonnxruntime.so (system or vendored)

# Clone
git clone https://github.com/upalla/upalla
cd upalla

# Build core library
cargo build -p upalla-core

# Build LV2 plugin
cargo build -p upalla-lv2 --release

# Install LV2 plugin
mkdir -p ~/.lv2
cp target/release/libupalla_lv2.so ~/.lv2/upalla.lv2/upalla.so
```

## Model Setup

Download the DeepFilterNet3 ONNX model:

```bash
mkdir -p ~/.local/share/upalla
# Download from HuggingFace (choose one):
# Option 1: Serkan007's tarball
wget https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz
tar xzf DeepFilterNet3_onnx.tar.gz -C ~/.local/share/upalla/

# Option 2: Export from PyTorch
pip install deepfilternet
python -c "
from df.scripts.export import main
main()
"
```

Also download the auxiliary data (ERB filterbank + Vorbis window):

```bash
wget https://huggingface.co/soniqo/DeepFilterNet3-ONNX/resolve/main/deepfilter-auxiliary.bin \
  -O ~/.local/share/upalla/auxiliary.bin
```

## PipeWire Setup

Copy the provided config:

```bash
mkdir -p ~/.config/pipewire/pipewire.conf.d
cp config/upalla.conf ~/.config/pipewire/pipewire.conf.d/99-upalla.conf
systemctl restart --user pipewire
```

Your "Upalla Denoiser" virtual microphone should now appear in audio settings.

## Parameters

| Parameter | Range | Default | Description |
|---|---|---|---|
| Suppression | 0-100% | 80% | Noise reduction strength |
| VAD Threshold | 0-100% | 50% | Voice activity sensitivity |
| Bypass | on/off | off | Pass audio through unprocessed |

## GPU Backends

ONNX Runtime auto-detects available providers at runtime:

- **CUDA** (NVIDIA) — requires `libonnxruntime.so` with CUDA EP
- **ROCm** (AMD) — requires `libonnxruntime.so` with ROCm EP
- **OpenVINO** (Intel) — requires `libonnxruntime.so` with OpenVINO EP
- **CPU** — always available fallback (RTF ~0.04 on Core i5)

## Latency

~40ms total:
- STFT analysis: 10ms (1 frame)
- Deep filter lookahead: 20ms (2 frames)
- Worker thread buffer: 10ms (1 frame)

## License

MIT OR Apache-2.0

The DeepFilterNet3 model weights are used under their original Apache-2.0/MIT license.
