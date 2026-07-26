use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer as _, Observer, Producer as _, Split};
use ringbuf::HeapRb;

const REMAINDER_CAP: usize = 16384;
const RINGBUF_CAP: usize = 16384;
const MAX_BUFFER_FRAMES: usize = 8;
const FRAME_SIZE: usize = CHUNK * 2; // 960 f32 = 480 stereo frames = 10ms at 48kHz

pub struct Status {
    pub playback_in: f32,
    pub playback_out: f32,
    pub recording_in: f32,
    pub recording_out: f32,
}

#[derive(Clone)]
pub struct DeviceInfo {
    pub name: String,
}
#[derive(Clone)]
pub struct DeviceLists {
    pub sinks: Vec<DeviceInfo>,
    pub sources: Vec<DeviceInfo>,
}

pub enum Cmd {
    SetSink(String),
    SetSource(String),
    SetBypass(bool),
    EnumerateDevices(Sender<DeviceLists>),
    Shutdown,
}

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
    fn drop_excess(&mut self) {
        let max_samples = MAX_BUFFER_FRAMES * FRAME_SIZE;
        let excess = self.len().saturating_sub(max_samples);
        if excess > 0 {
            let drop_samples = (excess / FRAME_SIZE) * FRAME_SIZE;
            if drop_samples > 0 {
                log::debug!("Dropping {drop_samples} samples to bound latency");
                self.pos += drop_samples;
                if self.pos > REMAINDER_CAP {
                    self.data.drain(..self.pos);
                    self.pos = 0;
                }
            }
        }
    }
}

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

pub fn enumerate_devices() -> Result<DeviceLists> {
    let host = cpal::default_host();
    let sinks: Vec<DeviceInfo> = host
        .output_devices()
        .context("enumerate output devices")?
        .map(|d| {
            let name = device_name(&d);
            DeviceInfo { name }
        })
        .collect();

    let sources: Vec<DeviceInfo> = host
        .input_devices()
        .context("enumerate input devices")?
        .map(|d| {
            let name = device_name(&d);
            DeviceInfo { name }
        })
        .collect();

    log::info!(
        "Enumerated {} sinks, {} sources",
        sinks.len(),
        sources.len()
    );

    Ok(DeviceLists {
        sinks,
        sources,
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
    host.input_devices().ok()?.find(|d| device_matches(d, name))
}

fn find_blackhole_output() -> Option<cpal::Device> {
    cpal::default_host()
        .output_devices()
        .ok()?
        .find(|d| device_name(d).contains("BlackHole"))
}

fn find_blackhole_input() -> Option<cpal::Device> {
    cpal::default_host()
        .input_devices()
        .ok()?
        .find(|d| device_name(d).contains("BlackHole"))
}

fn create_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tag: &'static str,
) -> Result<(cpal::Stream, ringbuf::HeapCons<f32>)> {
    let rb = HeapRb::<f32>::new(RINGBUF_CAP);
    let (mut prod, cons) = rb.split();

    let stream = device.build_input_stream(
        config.clone(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let n = prod.push_slice(data);
            if n < data.len() {
                static LAST_WARN: AtomicU64 = AtomicU64::new(0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let last = LAST_WARN.swap(now, Ordering::Relaxed);
                if now != last {
                    log::warn!(
                        "Input ring buffer overflow, dropped {} samples",
                        data.len() - n
                    );
                }
            }
            if tag == "rec" {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, Ordering::Relaxed) {
                    let first: Vec<f32> = data.iter().take(8).copied().collect();
                    log::info!("Recording input first 8 samples: {:?}", first);
                }
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

    Ok((stream, prod))
}

fn process_chain(
    audio_in: &mut AudioBuf,
    audio_out: &mut AudioBuf,
    denoiser: &mut Denoiser,
    is_bypass: bool,
    frame_size: usize,
    rms_in_accum: &mut f32,
    rms_out_accum: &mut f32,
    rms_count: &mut u32,
) {
    if let Some(frame) = audio_in.drain_frames(frame_size) {
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
            let r = compute_rms(&sc.left) + compute_rms(&sc.right);
            *rms_in_accum += r;
            *rms_out_accum += r;
            *rms_count += 2;
        } else {
            match denoiser.process_stereo(&sc) {
                Ok(out) => {
                    for i in 0..CHUNK {
                        audio_out.data.push(out.left[i]);
                        audio_out.data.push(out.right[i]);
                    }
                    *rms_in_accum += compute_rms(&sc.left) + compute_rms(&sc.right);
                    *rms_out_accum += compute_rms(&out.left) + compute_rms(&out.right);
                    *rms_count += 2;
                }
                Err(e) => {
                    log::error!("Denoiser error: {e}, bypassing");
                    for i in 0..CHUNK {
                        audio_out.data.push(sc.left[i]);
                        audio_out.data.push(sc.right[i]);
                    }
                    let r = compute_rms(&sc.left) + compute_rms(&sc.right);
                    *rms_in_accum += r;
                    *rms_out_accum += r;
                    *rms_count += 2;
                }
            }
        }
    }
}

fn pump_input(cons: &mut ringbuf::HeapCons<f32>, audio_in: &mut AudioBuf, temp_buf: &mut [f32]) {
    let avail = cons.occupied_len();
    if avail == 0 {
        return;
    }
    let n = avail.min(temp_buf.len());
    let n = cons.pop_slice(&mut temp_buf[..n]);
    audio_in.extend(&temp_buf[..n]);
}

fn pump_output(prod: &mut ringbuf::HeapProd<f32>, audio_out: &mut AudioBuf) {
    let out_avail = audio_out.len();
    if out_avail == 0 {
        return;
    }
    let space = prod.vacant_len();
    if space == 0 {
        return;
    }
    let n = out_avail.min(space);
    let chunk = audio_out.take_available(n);
    prod.push_slice(&chunk);
}

pub fn run_audio_engine(cmd_rx: Receiver<Cmd>, status_tx: Sender<Status>) {
    std::thread::Builder::new()
        .name("upalla-audio".into())
        .spawn(move || {
            if let Err(e) = audio_thread(cmd_rx, status_tx) {
                log::error!("Audio engine error: {e}");
            }
        })
        .expect("spawn audio thread");
}

fn audio_thread(cmd_rx: Receiver<Cmd>, status_tx: Sender<Status>) -> Result<()> {
    let model = Model::default();
    let bypass = AtomicBool::new(false);
    let host = cpal::default_host();
    let default_input = host
        .default_input_device()
        .context("No input device available")?;
    let default_output = host
        .default_output_device()
        .context("No output device available")?;

    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: 48000,
        buffer_size: cpal::BufferSize::Default,
    };

    let bh_output = find_blackhole_output();
    let bh_input = find_blackhole_input();
    let has_playback = bh_input.is_some();

    if has_playback {
        log::info!("BlackHole found — dual-chain active");
    } else {
        log::info!("BlackHole not found — recording-only mode");
    }

    let (pb_stream_in, mut pb_in_cons) = if let Some(ref bh) = bh_input {
        let (s, c) = create_input(bh, &config, "pb")?;
        (Some(s), Some(c))
    } else {
        (None, None)
    };
    let (mut pb_stream_out, mut pb_out_prod) = {
        let (s, p) = create_output(&default_output, &config)?;
        (Some(s), Some(p))
    };
    let mut pb_audio_in = AudioBuf::new();
    let mut pb_audio_out = AudioBuf::new();
    let mut pb_denoiser = Denoiser::new(&model, 2)?;
    let mut pb_rms_in = 0.0f32;
    let mut pb_rms_out = 0.0f32;
    let mut pb_rms_count = 0u32;

    let rec_output_dev = bh_output.as_ref().unwrap_or(&default_output);
    let (mut rec_stream_in, mut rec_in_cons) = create_input(&default_input, &config, "rec")?;
    let (rec_stream_out, mut rec_out_prod) = create_output(rec_output_dev, &config)?;
    let mut rec_audio_in = AudioBuf::new();
    let mut rec_audio_out = AudioBuf::new();
    let mut rec_denoiser = Denoiser::new(&model, 2)?;
    let mut rec_rms_in = 0.0f32;
    let mut rec_rms_out = 0.0f32;
    let mut rec_rms_count = 0u32;

    if let Some(ref s) = pb_stream_in {
        s.play()?;
        log::info!("Playback input stream started");
    }
    if let Some(ref s) = pb_stream_out {
        s.play()?;
        log::info!("Playback output stream started");
    }
    rec_stream_in.play()?;
    log::info!("Recording input stream started");
    rec_stream_out.play()?;
    log::info!("Recording output stream started");
    log::info!("Input device: {}", device_name(&default_input));
    log::info!("Output device: {}", device_name(&default_output));
    if let Some(ref bh) = bh_input {
        log::info!("BlackHole input: {}", device_name(bh));
    }
    if let Some(ref bh) = bh_output {
        log::info!("BlackHole output: {}", device_name(bh));
    }

    let frame_size = CHUNK * 2;
    let mut last_status = Instant::now();
    let mut temp_buf = vec![0.0f32; 4096];
    let mut shutdown = false;

    log::info!(
        "Audio processing loop running (has_playback={})",
        has_playback
    );

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::SetSink(name) => {
                    log::info!("Switching playback output to {name}");
                    if let Some(dev) = find_output_by_name(&name) {
                        if let Ok((new_stream, new_prod)) = create_output(&dev, &config) {
                            if let Some(old) = pb_stream_out.replace(new_stream) {
                                drop(old);
                            }
                            if let Some(ref s) = pb_stream_out {
                                if let Err(e) = s.play() {
                                    log::error!("Failed to start new output stream: {e}");
                                }
                            }
                            pb_out_prod = Some(new_prod);
                            pb_audio_out.data.clear();
                            pb_audio_out.pos = 0;
                        }
                    } else {
                        log::warn!("Output device not found: {name}");
                    }
                }
                Cmd::SetSource(name) => {
                    log::info!("Switching recording input to {name}");
                    if let Some(dev) = find_input_by_name(&name) {
                        if let Ok((new_stream, new_cons)) = create_input(&dev, &config, "rec") {
                            drop(std::mem::replace(&mut rec_stream_in, new_stream));
                            if let Err(e) = rec_stream_in.play() {
                                log::error!("Failed to start new input stream: {e}");
                            }
                            rec_in_cons = new_cons;
                            rec_audio_in.data.clear();
                            rec_audio_in.pos = 0;
                        }
                    } else {
                        log::warn!("Input device not found: {name}");
                    }
                }
                Cmd::SetBypass(val) => {
                    bypass.store(val, Ordering::Relaxed);
                }
                Cmd::EnumerateDevices(tx) => {
                    if let Ok(devices) = enumerate_devices() {
                        let _ = tx.send(devices);
                    }
                }
                Cmd::Shutdown => {
                    log::info!("Audio engine shutting down");
                    shutdown = true;
                }
            }
        }

        if shutdown {
            break;
        }

        let is_bypass = bypass.load(Ordering::Relaxed);

        if has_playback {
            if let Some(ref mut c) = pb_in_cons {
                pump_input(c, &mut pb_audio_in, &mut temp_buf);
            }
            pb_audio_in.drop_excess();
            process_chain(
                &mut pb_audio_in,
                &mut pb_audio_out,
                &mut pb_denoiser,
                is_bypass,
                frame_size,
                &mut pb_rms_in,
                &mut pb_rms_out,
                &mut pb_rms_count,
            );
            if let Some(ref mut p) = pb_out_prod {
                pump_output(p, &mut pb_audio_out);
            }
        }

        pump_input(&mut rec_in_cons, &mut rec_audio_in, &mut temp_buf);
        rec_audio_in.drop_excess();
        process_chain(
            &mut rec_audio_in,
            &mut rec_audio_out,
            &mut rec_denoiser,
            is_bypass,
            frame_size,
            &mut rec_rms_in,
            &mut rec_rms_out,
            &mut rec_rms_count,
        );
        pump_output(&mut rec_out_prod, &mut rec_audio_out);

        if last_status.elapsed() >= Duration::from_millis(100) {
            let playback_in = if pb_rms_count > 0 {
                pb_rms_in / pb_rms_count as f32
            } else {
                0.0
            };
            let playback_out = if pb_rms_count > 0 {
                pb_rms_out / pb_rms_count as f32
            } else {
                0.0
            };
            let recording_in = if rec_rms_count > 0 {
                rec_rms_in / rec_rms_count as f32
            } else {
                0.0
            };
            let recording_out = if rec_rms_count > 0 {
                rec_rms_out / rec_rms_count as f32
            } else {
                0.0
            };
            let _ = status_tx.try_send(Status {
                playback_in,
                playback_out,
                recording_in,
                recording_out,
            });
            {
                static FIRST: AtomicBool = AtomicBool::new(true);
                if FIRST.swap(false, Ordering::Relaxed) {
                    log::info!(
                        "First status sent: pb_in={:.6} pb_out={:.6} rec_in={:.6} rec_out={:.6} pb_count={} rec_count={}",
                        playback_in, playback_out, recording_in, recording_out,
                        pb_rms_count, rec_rms_count
                    );
                }
            }
            pb_rms_in = 0.0;
            pb_rms_out = 0.0;
            pb_rms_count = 0;
            rec_rms_in = 0.0;
            rec_rms_out = 0.0;
            rec_rms_count = 0;
            last_status = Instant::now();
        }

        let pb_idle = !has_playback || (pb_audio_in.len() < frame_size && pb_audio_out.len() == 0);
        let rec_idle = rec_audio_in.len() < frame_size && rec_audio_out.len() == 0;
        if pb_idle && rec_idle {
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    log::info!("Audio engine stopped.");
    drop(pb_stream_in);
    drop(pb_stream_out);
    drop(rec_stream_in);
    drop(rec_stream_out);

    Ok(())
}
