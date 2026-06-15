use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer as _, Observer, Producer as _, Split};
use ringbuf::HeapRb;

const REMAINDER_CAP: usize = 16384;
const RINGBUF_CAP: usize = 8192; // ~85ms at 48kHz stereo f32

// ---- Public types (identical to filter.rs) ----

pub struct Status {
    pub playback_in: f32,
    pub playback_out: f32,
    pub recording_in: f32,
    pub recording_out: f32,
}

#[derive(Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub description: String,
}

#[derive(Clone)]
pub struct DeviceLists {
    pub sinks: Vec<DeviceInfo>,
    pub sources: Vec<DeviceInfo>,
    pub default_sink: String,
    pub default_source: String,
}

pub enum Cmd {
    SwitchModel(Model),
    EnumerateDevices(Sender<DeviceLists>),
    SetSink(String),
    SetSource(String),
    Shutdown,
}

// ---- AudioBuf (identical to filter.rs) ----

struct AudioBuf {
    data: Vec<f32>,
    pos: usize,
}

impl AudioBuf {
    fn new() -> Self {
        AudioBuf {
            data: Vec::new(),
            pos: 0,
        }
    }

    fn len(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn extend(&mut self, samples: &[f32]) {
        self.data.extend_from_slice(samples);
    }

    fn drain_frames(&mut self, frame_size: usize) -> Option<Vec<f32>> {
        if self.len() < frame_size {
            return None;
        }
        let frame: Vec<f32> = self.data[self.pos..self.pos + frame_size].to_vec();
        self.pos += frame_size;
        if self.pos > REMAINDER_CAP {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        Some(frame)
    }

    fn take_available(&mut self, max: usize) -> Vec<f32> {
        let n = self.len().min(max);
        if n == 0 {
            return vec![];
        }
        let chunk: Vec<f32> = self.data[self.pos..self.pos + n].to_vec();
        self.pos += n;
        if self.pos > REMAINDER_CAP {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        chunk
    }
}

// ---- Helpers ----

fn compute_rms(chunk: &[f32]) -> f32 {
    let sum: f32 = chunk.iter().map(|&s| s * s).sum();
    (sum / chunk.len() as f32).sqrt()
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Unknown".into())
}

fn enumerate_devices() -> Result<DeviceLists> {
    let host = cpal::default_host();
    let default_sink = host
        .default_output_device()
        .as_ref()
        .map(|d| device_name(d))
        .unwrap_or_default();
    let default_source = host
        .default_input_device()
        .as_ref()
        .map(|d| device_name(d))
        .unwrap_or_default();

    let sinks: Vec<DeviceInfo> = host
        .output_devices()
        .context("enumerate output devices")?
        .map(|d| {
            let name = device_name(&d);
            DeviceInfo {
                name: name.clone(),
                description: name,
            }
        })
        .collect();

    let sources: Vec<DeviceInfo> = host
        .input_devices()
        .context("enumerate input devices")?
        .map(|d| {
            let name = device_name(&d);
            DeviceInfo {
                name: name.clone(),
                description: name,
            }
        })
        .collect();

    log::info!(
        "Enumerated {} sinks, {} sources (default sink: {}, default source: {})",
        sinks.len(),
        sources.len(),
        default_sink,
        default_source,
    );

    Ok(DeviceLists {
        sinks,
        sources,
        default_sink,
        default_source,
    })
}

fn device_matches(device: &cpal::Device, name: &str) -> bool {
    device_name(device) == name
}

fn find_output_by_name(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if name == "@DEFAULT_SINK@" || name.is_empty() {
        return host.default_output_device();
    }
    host.output_devices()
        .ok()?
        .find(|d| device_matches(d, name))
}

fn find_input_by_name(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if name == "@DEFAULT_SOURCE@" || name.is_empty() {
        return host.default_input_device();
    }
    host.input_devices()
        .ok()?
        .find(|d| device_matches(d, name))
}

fn create_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
) -> Result<(cpal::Stream, ringbuf::HeapCons<f32>)> {
    let rb = HeapRb::<f32>::new(RINGBUF_CAP);
    let (mut prod, cons) = rb.split();

    let stream = device.build_input_stream(
        config.clone(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let n = prod.push_slice(data);
            if n < data.len() {
                log::warn!(
                    "Input ring buffer overflow, dropped {} samples",
                    data.len() - n
                );
            }
        },
        |err| log::error!("Input stream error: {err}"),
        None,
    )?;

    Ok((stream, cons))
}

fn create_output(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
) -> Result<(cpal::Stream, ringbuf::HeapProd<f32>)> {
    let rb = HeapRb::<f32>::new(RINGBUF_CAP);
    let (prod, mut cons) = rb.split();

    let stream = device.build_output_stream(
        config.clone(),
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let n = cons.pop_slice(data);
            for s in &mut data[n..] {
                *s = 0.0;
            }
        },
        |err| log::error!("Output stream error: {err}"),
        None,
    )?;

    // prod is stored for the caller (it's the processing-thread side)
    // cons is moved into the output callback (CoreAudio reads from it)
    Ok((stream, prod))
}

// ---- Main processing function ----

pub fn run_filter(
    model: Model,
    cmd_rx: Receiver<Cmd>,
    status_tx: Sender<Status>,
    enable: Arc<AtomicBool>,
) -> Result<()> {
    let default_input = cpal::default_host()
        .default_input_device()
        .context("No input device available")?;
    let default_output = cpal::default_host()
        .default_output_device()
        .context("No output device available")?;

    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: 48000,
        buffer_size: cpal::BufferSize::Default,
    };

    let (mut input_stream, mut input_cons) = create_input(&default_input, &config)?;
    let (mut output_stream, mut output_prod) = create_output(&default_output, &config)?;

    input_stream.play()?;
    output_stream.play()?;

    let mut denoiser = Denoiser::new(&model, 2)?;
    let mut audio_in = AudioBuf::new();
    let mut audio_out = AudioBuf::new();
    let frame_size = CHUNK * 2; // 20ms at 48kHz

    let mut last_status = Instant::now();
    let mut rms_accum = [0.0f32; 2];
    let mut rms_count = 0u32;

    let mut temp_buf = vec![0.0f32; 4096];
    let mut shutdown = false;

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::SwitchModel(new_model) => {
                    log::info!("Switching model to {}", new_model.label());
                    denoiser = Denoiser::new(&new_model, 2)?;
                }
                Cmd::EnumerateDevices(tx) => {
                    if let Ok(devices) = enumerate_devices() {
                        let _ = tx.send(devices);
                    }
                }
                Cmd::SetSink(name) => {
                    log::info!("Switching output to {name}");
                    if let Some(dev) = find_output_by_name(&name) {
                        if let Ok((new_stream, new_prod)) = create_output(&dev, &config) {
                            drop(std::mem::replace(&mut output_stream, new_stream));
                            output_prod = new_prod;
                            audio_out.data.clear();
                            audio_out.pos = 0;
                        }
                    } else {
                        log::warn!("Output device not found: {name}");
                    }
                }
                Cmd::SetSource(name) => {
                    log::info!("Switching input to {name}");
                    if let Some(dev) = find_input_by_name(&name) {
                        if let Ok((new_stream, new_cons)) = create_input(&dev, &config) {
                            drop(std::mem::replace(&mut input_stream, new_stream));
                            input_cons = new_cons;
                            audio_in.data.clear();
                            audio_in.pos = 0;
                        }
                    } else {
                        log::warn!("Input device not found: {name}");
                    }
                }
                Cmd::Shutdown => {
                    log::info!("CA filter received shutdown command");
                    shutdown = true;
                }
            }
        }

        if shutdown {
            break;
        }

        // Read from input ring buffer
        let avail = input_cons.occupied_len();
        if avail > 0 {
            let n = avail.min(temp_buf.len());
            let n = input_cons.pop_slice(&mut temp_buf[..n]);
            audio_in.extend(&temp_buf[..n]);
        }

        let is_bypass = !enable.load(Ordering::Relaxed);

        // Process frames
        while let Some(frame) = audio_in.drain_frames(frame_size) {
            let mut sc = StereoChunk {
                left: [0.0; CHUNK],
                right: [0.0; CHUNK],
            };
            for i in 0..CHUNK {
                sc.left[i] = frame[i * 2];
                sc.right[i] = frame[i * 2 + 1];
            }
            if is_bypass {
                for i in 0..CHUNK {
                    audio_out.data.push(sc.left[i]);
                    audio_out.data.push(sc.right[i]);
                }
                rms_accum[0] += compute_rms(&sc.left);
                rms_accum[0] += compute_rms(&sc.right);
                rms_accum[1] += compute_rms(&sc.left);
                rms_accum[1] += compute_rms(&sc.right);
                rms_count += 2;
            } else {
                match denoiser.process_stereo(&sc) {
                    Ok(out) => {
                        for i in 0..CHUNK {
                            audio_out.data.push(out.left[i]);
                            audio_out.data.push(out.right[i]);
                        }
                        rms_accum[0] += compute_rms(&sc.left);
                        rms_accum[0] += compute_rms(&sc.right);
                        rms_accum[1] += compute_rms(&out.left);
                        rms_accum[1] += compute_rms(&out.right);
                        rms_count += 2;
                    }
                    Err(e) => {
                        log::error!("Denoiser error: {e}, falling back to bypass");
                        for i in 0..CHUNK {
                            audio_out.data.push(sc.left[i]);
                            audio_out.data.push(sc.right[i]);
                        }
                        rms_accum[0] += compute_rms(&sc.left);
                        rms_accum[0] += compute_rms(&sc.right);
                        rms_accum[1] += compute_rms(&sc.left);
                        rms_accum[1] += compute_rms(&sc.right);
                        rms_count += 2;
                    }
                }
            }
        }

        // Write to output ring buffer
        let out_avail = audio_out.len();
        if out_avail > 0 {
            let space = output_prod.vacant_len();
            if space > 0 {
                let n = out_avail.min(space);
                let chunk = audio_out.take_available(n);
                output_prod.push_slice(&chunk);
            }
        }

        // Status reporting (every 100ms)
        if last_status.elapsed() >= Duration::from_millis(100) {
            let (recording_in, recording_out) = if rms_count > 0 {
                let c = rms_count as f32;
                (rms_accum[0] / c, rms_accum[1] / c)
            } else {
                (0.0, 0.0)
            };
            let _ = status_tx.try_send(Status {
                playback_in: 0.0,
                playback_out: 0.0,
                recording_in,
                recording_out,
            });
            rms_accum = [0.0; 2];
            rms_count = 0;
            last_status = Instant::now();
        }

        // Sleep if idle to avoid busy-looping
        if audio_in.len() < frame_size && audio_out.len() == 0 {
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    log::info!("CA filter stopped.");
    drop(input_stream);
    drop(output_stream);

    Ok(())
}
