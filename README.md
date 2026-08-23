# Upalla

GPU-agnostic real-time noise suppression using **DeepFilterNet3** via `tract` (pure Rust ONNX inference — no CUDA, no ROCm, no system GPU runtime needed).

## Quick Start

```bash
# PulseAudio / PipeWire filter (Linux)
cargo build --release -p upalla-pa
./target/release/upalla-pa
```

A "Upalla Denoised Output" sink and "Upalla Denoised Input" source appear. Route applications to the sink, or your microphone through the source, using `pavucontrol`, `helvum`, or `qpwgraph`. Press Ctrl-C to stop.

## GUI Frontends

```bash
# Slint GUI (GTK style, system tray)
cargo run --release -p upalla-slint

# GTK4 GUI
cargo run --release -p upalla-gtk
```

Both GUIs launch to the system tray. They auto-start the PulseAudio filter and show a window for level meters, device selection, and bypass toggles.

## macOS

```bash
# Requires: zigbuild, macOS SDK
SDKROOT=/path/to/MacOSX.sdk/ cargo zigbuild --target aarch64-apple-darwin --release -p upalla-mac
```

The macOS app uses CoreAudio directly (no PulseAudio dependency). It supports BlackHole for system-wide audio capture.

## CLI WAV Denoiser

```bash
cargo run --release -p upalla-core -- input.wav output.wav
```

## Architecture

```
                     ┌─────────────────────────────────┐
                     │         upalla-pa / upalla-mac    │
                     │  ┌──────────┐   ┌──────────────┐ │
  mic/app audio ─────┼─▶│ pump_read │──▶│  process_one  │─┼──▶ denoised out
                     │  └──────────┘   │  frame/iter   │ │
                     │                 │  (if let, not  │ │
                     │                 │   while let)   │ │
                     │                 └──────┬───────┘ │
                     │                        │         │
                     │                 ┌──────▼───────┐ │
                     │                 │  drop_excess  │ │
                     │                 │  (bounds buf  │ │
                     │                 │   at 8 frames)│ │
                     │                 └──────────────┘ │
                     └─────────────────────────────────┘
```

Each crate uses a tight processing loop: `pump_read` → `drop_excess` (shed overload) → `process_one_frame` (at most one ONNX inference per iteration) → `pump_write`. This bounds latency at ~80-90ms under any system load.

## Project Crates

| Crate | Description | Platform |
|---|---|---|
| `upalla-core` | Denoiser engine + CLI WAV tool | All |
| `upalla-pa` | PulseAudio realtime filter | Linux |
| `upalla-slint` | Slint GUI (system tray) | Linux |
| `upalla-gtk` | GTK4 GUI | Linux |
| `upalla-mac` | Native macOS app (CoreAudio) | macOS |
| `upalla-lv2` | LV2 plugin | Linux |

## Building

```bash
# All Linux targets
cargo build --release -p upalla-core -p upalla-pa -p upalla-slint -p upalla-gtk

# macOS (cross-compile)
SDKROOT=/path/to/MacOSX.sdk/ cargo zigbuild --target aarch64-apple-darwin --release -p upalla-mac
```

Requires Rust 1.80+ and PulseAudio development headers (`libpulse-dev` on Debian, `pulseaudio-libs-devel` on Fedora).

The DeepFilterNet3 model is compiled into each binary via `include_bytes!` — no separate download needed.

## Latency

~40ms total pipeline:
- Frame size: 10ms (480 samples at 48 kHz)
- Deep filter model: ~20ms lookahead
- Buffer watermark: up to 80ms under load (oldest frames silently dropped when exceeded)
- `upalla-mac` only: a further 30ms output cushion. The input and output devices
  run on independent clocks and the processing loop is not a real-time callback,
  so without a cushion the output starves and zero-fills, which is audible as
  clipped speech.

The denoiser must run in an optimised build to keep up: release measures around
RTF 0.13, but a debug build is roughly RTF 1.9 — slower than real time, which
drops frames and chops speech.

## License

MIT OR Apache-2.0, except for the macOS app (`upalla-mac/`), which is
GPL-3.0-or-later — see [upalla-mac/LICENSE](upalla-mac/LICENSE).

The DeepFilterNet3 model weights are used under their original Apache-2.0/MIT license.
