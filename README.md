# Upalla

GPU-agnostic real-time noise suppression for Linux using **DeepFilterNet3**.

Works with NVIDIA, AMD, Intel GPUs — or CPU only — via ONNX Runtime.

## Quick Start

```bash
# 1. Download the model
./scripts/download-model.sh

# 2. Build and run the native filter
cargo build -p upalla-pw --release
./target/release/upalla
```

A "Upalla Denoiser" audio source appears. Route your microphone through it using `pavucontrol`, `helvum`, or `qpwgraph`. Press Ctrl-C to stop.

No PipeWire config files, no LV2 plugin installation needed.

## Architecture

```
Microphone → PipeWire → upalla (native filter) → ONNX Runtime → Denoised Audio
                              ↕                         ↕
                         RT Audio Thread           Main Thread
                         (process callback)    (synchronous ONNX CPU EP)
```

## Building

```bash
# Prerequisites: Rust 1.92+, libonnxruntime.so (system or vendored)

# CLI WAV denoiser
cargo build -p upalla-pw --release
./target/release/upalla input.wav output.wav

# LV2 plugin (requires PipeWire compiled with LV2 support)
cargo build -p upalla-lv2 --release
./scripts/install-lv2.sh
```

## Model Setup

```bash
./scripts/download-model.sh
```

Downloads `enc.onnx`, `erb_dec.onnx`, `df_dec.onnx` to `~/.local/share/upalla/`.

## GPU Setup

Install ONNX Runtime for your GPU, or use the CPU fallback (bundled with most distros):

| GPU | Package |
|---|---|
| AMD ROCm | `onnxruntime-rocm` |
| NVIDIA CUDA | `onnxruntime-cuda` |
| Intel OpenVINO | `onnxruntime-openvino` |
| CPU fallback | `onnxruntime` (works without GPU) |

If ONNX Runtime is installed to a non-standard path (e.g. `/usr/lib64/rocm/lib/`), set:

```bash
export ORT_DYLIB_PATH=/usr/lib64/rocm/lib/libonnxruntime.so.1.22.2
```

Upalla auto-searches common ROCm/CUDA paths. Set `ORT_DYLIB_PATH` if auto-detection fails.

## Parameters (LV2 plugin only)

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
