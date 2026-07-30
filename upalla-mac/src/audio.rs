use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

use crate::blackhole;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer as _, Observer, Producer as _, Split};
use ringbuf::HeapRb;

const REMAINDER_CAP: usize = 16384;
const RINGBUF_CAP: usize = 16384;
const MAX_BUFFER_FRAMES: usize = 8;
const FRAME_SIZE: usize = CHUNK * 2; // 960 f32 = 480 stereo frames = 10ms at 48kHz

/// Silence pushed into an output ring before its stream starts, as a jitter
/// cushion.
///
/// Without it the ring sits near empty: the processing loop only produces output
/// after input arrives, so any late loop iteration — this is an ordinary
/// priority thread doing polling, not a real-time callback — leaves the output
/// callback short and it zero-fills, which is audible as clipped speech. The
/// input and output devices also run on independent clocks, so their drift eats
/// into the same margin. Costs this much added latency; `drop_excess` still
/// bounds the other end.
const OUTPUT_PREFILL_FRAMES: usize = 3; // 30ms

/// Occupancy below which the cushion is topped back up to
/// [`OUTPUT_PREFILL_FRAMES`]. Two frames rather than one: waiting until a single
/// frame is left means the output callback can starve within the same period,
/// before the processing loop gets a chance to refill.
const OUTPUT_LOW_WATER_FRAMES: usize = 2; // 20ms

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
    #[allow(dead_code)]
    SetMicCapture(bool),
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
                // Dropping here means the denoiser is not keeping up with real
                // time, which is audible as chopped speech — warn, but only once
                // a second so a sustained overload cannot flood the log.
                static LAST_WARN: AtomicU64 = AtomicU64::new(0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if LAST_WARN.swap(now, Ordering::Relaxed) != now {
                    log::warn!(
                        "Audio thread behind real time: dropping {drop_samples} samples to bound latency"
                    );
                }
                self.pos += drop_samples;
                if self.pos > REMAINDER_CAP {
                    self.data.drain(..self.pos);
                    self.pos = 0;
                }
            }
        }
    }
}

/// Input and output levels accumulated over one status interval, driving a
/// chain's pair of VU meters.
#[derive(Default)]
struct RmsMeter {
    input: f32,
    output: f32,
    count: u32,
}

impl RmsMeter {
    /// Records the input and output level of one processed frame. Both figures
    /// are per-frame sums across the two channels, so the count advances by two
    /// and the means come out per channel.
    fn push(&mut self, input: f32, output: f32) {
        self.input += input;
        self.output += output;
        self.count += 2;
    }

    fn mean_input(&self) -> f32 {
        self.mean(self.input)
    }

    fn mean_output(&self) -> f32 {
        self.mean(self.output)
    }

    fn mean(&self, total: f32) -> f32 {
        if self.count > 0 {
            total / self.count as f32
        } else {
            0.0
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
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

    Ok(DeviceLists { sinks, sources })
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

/// Finds a BlackHole input, preferring one that is *not* `exclude`.
///
/// BlackHole is a loopback: anything written to it reappears on its input. If the
/// playback chain read the same device the recording chain writes the denoised mic
/// into, the user would hear themselves. With two BlackHole devices installed the
/// chains can each have their own; with one they have to take turns.
fn find_blackhole_input_excluding(exclude: Option<&str>) -> Option<cpal::Device> {
    let candidates: Vec<cpal::Device> = cpal::default_host()
        .input_devices()
        .ok()?
        .filter(|d| device_name(d).contains("BlackHole"))
        .collect();
    candidates
        .iter()
        .find(|d| Some(device_name(d).as_str()) != exclude)
        .or_else(|| candidates.first())
        .cloned()
}

fn create_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tag: &'static str,
) -> Result<(cpal::Stream, ringbuf::HeapCons<f32>)> {
    let rb = HeapRb::<f32>::new(RINGBUF_CAP);
    let (mut prod, cons) = rb.split();

    // Per-stream, so one stream's warnings cannot mask another's.
    let mut last_warn = 0u64;

    let stream = device.build_input_stream(
        *config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let n = prod.push_slice(data);
            if n < data.len() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now != last_warn {
                    last_warn = now;
                    log::warn!(
                        "[{tag}] input ring buffer overflow, dropped {} samples",
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
    tag: &'static str,
) -> Result<(cpal::Stream, ringbuf::HeapProd<f32>)> {
    let rb = HeapRb::<f32>::new(RINGBUF_CAP);
    let (prod, mut cons) = rb.split();

    // Per-stream, so a chronically starved stream cannot mask another's warnings.
    let mut last_warn = 0u64;
    // A stream that has never been fed is idle by design, not starved; only report
    // shortfalls once real audio has started flowing through it.
    let mut seen_data = false;

    let stream = device.build_output_stream(
        *config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let n = cons.pop_slice(data);
            for s in &mut data[n..] {
                *s = 0.0;
            }
            seen_data |= n > 0;
            if seen_data && n < data.len() {
                // Zero-filling mid-stream is an underrun: the jitter cushion is
                // gone and the gap is audible. Rate-limited to once a second.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now != last_warn {
                    last_warn = now;
                    log::warn!(
                        "[{tag}] output underrun: {} of {} samples missing",
                        data.len() - n,
                        data.len()
                    );
                }
            }
        },
        |err| log::error!("Output stream error: {err}"),
        None,
    )?;

    Ok((stream, prod))
}

/// A started chain: input stream + its consumer, output stream + its producer.
type Chain = (
    cpal::Stream,
    ringbuf::HeapCons<f32>,
    cpal::Stream,
    ringbuf::HeapProd<f32>,
);

/// Opens the BlackHole input and the speaker output and starts both.
fn start_pb_chain(
    bh_input: &cpal::Device,
    sink: Option<&str>,
    config: &cpal::StreamConfig,
) -> Result<Chain> {
    let output_dev = match sink {
        Some(name) => {
            find_output_by_name(name).with_context(|| format!("Output device not found: {name}"))?
        }
        None => cpal::default_host()
            .default_output_device()
            .context("No output device available")?,
    };
    let (stream_in, cons) = create_input(bh_input, config, "pb")?;
    let (stream_out, mut prod) = create_output(&output_dev, config, "pb")?;
    prod.push_slice(&vec![0.0f32; OUTPUT_PREFILL_FRAMES * FRAME_SIZE]);
    stream_in.play()?;
    stream_out.play()?;
    log::info!(
        "Playback chain started ({} -> {})",
        device_name(bh_input),
        device_name(&output_dev)
    );
    Ok((stream_in, cons, stream_out, prod))
}

/// Opens the mic input and the BlackHole output and starts both.
fn start_rec_chain(
    source: Option<&str>,
    output_dev: &cpal::Device,
    config: &cpal::StreamConfig,
) -> Result<Chain> {
    let input_dev = match source {
        Some(name) => {
            find_input_by_name(name).with_context(|| format!("Input device not found: {name}"))?
        }
        None => cpal::default_host()
            .default_input_device()
            .context("No input device available")?,
    };
    let (stream_in, cons) = create_input(&input_dev, config, "rec")?;
    let (stream_out, mut prod) = create_output(output_dev, config, "rec")?;
    let prefill = vec![0.0f32; OUTPUT_PREFILL_FRAMES * FRAME_SIZE];
    prod.push_slice(&prefill);
    // Input first: starting a device takes tens of milliseconds, and an output
    // that is already draining would burn through the cushion before the first
    // mic buffer ever lands.
    stream_in.play()?;
    stream_out.play()?;
    log::info!(
        "Recording chain started (input: {})",
        device_name(&input_dev)
    );
    Ok((stream_in, cons, stream_out, prod))
}

fn process_chain(
    audio_in: &mut AudioBuf,
    audio_out: &mut AudioBuf,
    denoiser: &mut Denoiser,
    is_bypass: bool,
    frame_size: usize,
    meter: &mut RmsMeter,
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
            meter.push(r, r);
        } else {
            match denoiser.process_stereo(&sc) {
                Ok(out) => {
                    for i in 0..CHUNK {
                        audio_out.data.push(out.left[i]);
                        audio_out.data.push(out.right[i]);
                    }
                    meter.push(
                        compute_rms(&sc.left) + compute_rms(&sc.right),
                        compute_rms(&out.left) + compute_rms(&out.right),
                    );
                }
                Err(e) => {
                    log::error!("Denoiser error: {e}, bypassing");
                    for i in 0..CHUNK {
                        audio_out.data.push(sc.left[i]);
                        audio_out.data.push(sc.right[i]);
                    }
                    let r = compute_rms(&sc.left) + compute_rms(&sc.right);
                    meter.push(r, r);
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
    // Rebuild the jitter cushion when it runs dry. A single late iteration, or
    // slow drift between the input and output device clocks, otherwise leaves the
    // ring permanently at zero margin, so every later hiccup punches another hole
    // in the outgoing audio. Only tops up what we cannot cover with real audio
    // that is already waiting.
    let occupied = prod.occupied_len();
    if occupied < OUTPUT_LOW_WATER_FRAMES * FRAME_SIZE {
        let target = OUTPUT_PREFILL_FRAMES * FRAME_SIZE;
        let deficit = target
            .saturating_sub(occupied + audio_out.len())
            .min(prod.vacant_len());
        if deficit > 0 {
            log::debug!("Output cushion dry, inserting {deficit} samples of silence");
            let silence = vec![0.0f32; deficit];
            prod.push_slice(&silence);
        }
    }

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
    // Prefer a different BlackHole device from the one the recording chain writes
    // to, so the two chains do not loop through each other.
    let rec_bh_name = bh_output.as_ref().map(device_name);
    let bh_input = find_blackhole_input_excluding(rec_bh_name.as_deref());
    let has_playback = bh_input.is_some();

    if has_playback {
        log::info!("BlackHole found — dual-chain active");
    } else {
        log::info!("BlackHole not found — recording-only mode");
    }

    // Both chains share one BlackHole device when only one is installed. Because
    // it is a loopback, running them together would feed the denoised mic straight
    // back to the speakers, so in that case they are mutually exclusive and the
    // recording chain wins.
    let chains_share_device = match (&bh_input, &bh_output) {
        (Some(i), Some(o)) => device_name(i) == device_name(o),
        _ => false,
    };
    if chains_share_device {
        log::info!(
            "Only one BlackHole device ({}) — playback chain will yield to recording",
            bh_input.as_ref().map(device_name).unwrap_or_default()
        );
    }

    // Like the recording chain, the playback chain stays closed until needed:
    // reading BlackHole's input is a capture stream, so holding it open would make
    // macOS report Upalla as recording again.
    let mut pb_stream_in: Option<cpal::Stream> = None;
    let mut pb_in_cons: Option<ringbuf::HeapCons<f32>> = None;
    let mut pb_stream_out: Option<cpal::Stream> = None;
    let mut pb_out_prod: Option<ringbuf::HeapProd<f32>> = None;
    let mut pb_sink: Option<String> = None;
    let mut pb_capture_active = false;
    let mut pb_audio_in = AudioBuf::new();
    let mut pb_audio_out = AudioBuf::new();
    let mut pb_denoiser = Denoiser::new(&model, 2)?;
    let mut pb_meter = RmsMeter::default();

    let rec_output_dev = bh_output.as_ref().unwrap_or(&default_output).clone();
    // Start with the recording chain stopped. Neither the mic input nor the
    // BlackHole output is opened until something else is listening on BlackHole —
    // holding either open makes macOS report Upalla as recording.
    let mut rec_stream_in: Option<cpal::Stream> = None;
    let mut rec_in_cons: Option<ringbuf::HeapCons<f32>> = None;
    let mut rec_stream_out: Option<cpal::Stream> = None;
    let mut rec_out_prod: Option<ringbuf::HeapProd<f32>> = None;
    let mut rec_source: Option<String> = None;
    let mut capture_mic = false;

    // Demand detection: each chain runs only while another process is using its
    // side of BlackHole — capturing from it (wants our denoised mic) or playing to
    // it (wants its audio denoised).
    let detection = blackhole::detection_supported();
    let rec_bh_id = rec_bh_name
        .as_deref()
        .and_then(blackhole::find_device_by_name);
    let pb_bh_id = bh_input
        .as_ref()
        .map(device_name)
        .as_deref()
        .and_then(blackhole::find_device_by_name);
    let auto_capture = rec_bh_id.is_some() && detection;
    let auto_playback = pb_bh_id.is_some() && detection;
    match (rec_bh_id.or(pb_bh_id), detection) {
        (Some(_), true) => {
            log::info!("BlackHole demand detection active — chains open on demand")
        }
        (Some(_), false) => log::info!(
            "BlackHole demand detection unavailable (needs macOS 14.4+) — mic is manual, playback chain off"
        ),
        (None, _) => log::info!("BlackHole device not found — mic is manual"),
    }
    let mut last_listener_poll = Instant::now() - Duration::from_secs(1);
    let mut rec_audio_in = AudioBuf::new();
    let mut rec_audio_out = AudioBuf::new();
    let mut rec_denoiser = Denoiser::new(&model, 2)?;
    let mut rec_meter = RmsMeter::default();

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
                    pb_sink = Some(name);
                    // Restart only if the chain is running; otherwise the new sink
                    // is picked up the next time the chain opens.
                    if pb_capture_active {
                        pb_stream_in = None;
                        pb_in_cons = None;
                        pb_stream_out = None;
                        pb_out_prod = None;
                        pb_capture_active = false;
                        if let Some(ref bh) = bh_input {
                            match start_pb_chain(bh, pb_sink.as_deref(), &config) {
                                Ok((si, ci, so, po)) => {
                                    pb_stream_in = Some(si);
                                    pb_in_cons = Some(ci);
                                    pb_stream_out = Some(so);
                                    pb_out_prod = Some(po);
                                    pb_capture_active = true;
                                }
                                Err(e) => {
                                    log::error!("Failed to restart playback chain: {e}")
                                }
                            }
                        }
                    }
                    pb_audio_out.data.clear();
                    pb_audio_out.pos = 0;
                }
                Cmd::SetSource(name) => {
                    log::info!("Switching recording input to {name}");
                    rec_source = Some(name);
                    // Restart the chain only if it is currently running; otherwise
                    // the new source is picked up the next time a listener appears.
                    if capture_mic {
                        rec_stream_in = None;
                        rec_in_cons = None;
                        rec_stream_out = None;
                        rec_out_prod = None;
                        capture_mic = false;
                        match start_rec_chain(rec_source.as_deref(), &rec_output_dev, &config) {
                            Ok((si, ci, so, po)) => {
                                rec_stream_in = Some(si);
                                rec_in_cons = Some(ci);
                                rec_stream_out = Some(so);
                                rec_out_prod = Some(po);
                                capture_mic = true;
                            }
                            Err(e) => log::error!("Failed to restart recording chain: {e}"),
                        }
                    }
                    rec_audio_in.data.clear();
                    rec_audio_in.pos = 0;
                }
                Cmd::SetBypass(val) => {
                    bypass.store(val, Ordering::Relaxed);
                }
                Cmd::SetMicCapture(enable) => {
                    if enable && !capture_mic {
                        match start_rec_chain(rec_source.as_deref(), &rec_output_dev, &config) {
                            Ok((si, ci, so, po)) => {
                                rec_stream_in = Some(si);
                                rec_in_cons = Some(ci);
                                rec_stream_out = Some(so);
                                rec_out_prod = Some(po);
                                capture_mic = true;
                                log::info!("Mic capture enabled (user)");
                            }
                            Err(e) => log::error!("Failed to start recording chain: {e}"),
                        }
                    } else if !enable && capture_mic {
                        rec_stream_in = None;
                        rec_in_cons = None;
                        rec_stream_out = None;
                        rec_out_prod = None;
                        capture_mic = false;
                        rec_audio_in.data.clear();
                        rec_audio_in.pos = 0;
                        rec_audio_out.data.clear();
                        rec_audio_out.pos = 0;
                        rec_meter.reset();
                        log::info!("Mic capture disabled (user)");
                    }
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

        // Start/stop each chain based on whether anything else is using its side of
        // BlackHole, so macOS only flags us as recording while we are in use.
        let poll_due = (auto_capture || auto_playback)
            && last_listener_poll.elapsed() >= Duration::from_millis(500);
        if poll_due {
            last_listener_poll = Instant::now();
        }

        if poll_due && auto_capture {
            let device = rec_bh_id.expect("auto_capture implies a BlackHole device");
            match blackhole::has_external_user(device, blackhole::Direction::Capture) {
                // The denoiser is deliberately not reset here: its rolling
                // spectrum buffers flush within df_order frames (~50ms) on their
                // own, and rebuilding the model would stall this thread.
                Some(true) if !capture_mic => {
                    match start_rec_chain(rec_source.as_deref(), &rec_output_dev, &config) {
                        Ok((si, ci, so, po)) => {
                            rec_stream_in = Some(si);
                            rec_in_cons = Some(ci);
                            rec_stream_out = Some(so);
                            rec_out_prod = Some(po);
                            capture_mic = true;
                            log::info!("BlackHole listener detected — recording chain started");
                        }
                        Err(e) => log::error!("Failed to start recording chain: {e}"),
                    }
                }
                Some(false) if capture_mic => {
                    rec_stream_in = None;
                    rec_in_cons = None;
                    rec_stream_out = None;
                    rec_out_prod = None;
                    capture_mic = false;
                    rec_audio_in.data.clear();
                    rec_audio_in.pos = 0;
                    rec_audio_out.data.clear();
                    rec_audio_out.pos = 0;
                    rec_meter.reset();
                    log::info!("No BlackHole listener left — recording chain stopped");
                }
                _ => {}
            }
        }

        if poll_due && auto_playback {
            let device = pb_bh_id.expect("auto_playback implies a BlackHole device");
            // With a single BlackHole device the recording chain owns it; running
            // both would loop our own denoised mic back to the speakers.
            let yielded = chains_share_device && capture_mic;
            let wants = if yielded {
                Some(false)
            } else {
                blackhole::has_external_user(device, blackhole::Direction::Playback)
            };
            match wants {
                Some(true) if !pb_capture_active => {
                    if let Some(ref bh) = bh_input {
                        match start_pb_chain(bh, pb_sink.as_deref(), &config) {
                            Ok((si, ci, so, po)) => {
                                pb_stream_in = Some(si);
                                pb_in_cons = Some(ci);
                                pb_stream_out = Some(so);
                                pb_out_prod = Some(po);
                                pb_capture_active = true;
                                log::info!("BlackHole playback detected — playback chain started");
                            }
                            Err(e) => log::error!("Failed to start playback chain: {e}"),
                        }
                    }
                }
                Some(false) if pb_capture_active => {
                    pb_stream_in = None;
                    pb_in_cons = None;
                    pb_stream_out = None;
                    pb_out_prod = None;
                    pb_capture_active = false;
                    pb_audio_in.data.clear();
                    pb_audio_in.pos = 0;
                    pb_audio_out.data.clear();
                    pb_audio_out.pos = 0;
                    pb_meter.reset();
                    if yielded {
                        log::info!("Playback chain stopped — yielding BlackHole to recording");
                    } else {
                        log::info!("Nothing playing to BlackHole — playback chain stopped");
                    }
                }
                _ => {}
            }
        }

        if capture_mic {
            if let Some(ref mut c) = rec_in_cons {
                pump_input(c, &mut rec_audio_in, &mut temp_buf);
            }
            rec_audio_in.drop_excess();
            process_chain(
                &mut rec_audio_in,
                &mut rec_audio_out,
                &mut rec_denoiser,
                is_bypass,
                frame_size,
                &mut rec_meter,
            );
            if let Some(ref mut p) = rec_out_prod {
                pump_output(p, &mut rec_audio_out);
            }
        }

        if has_playback && pb_capture_active {
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
                &mut pb_meter,
            );
            if let Some(ref mut p) = pb_out_prod {
                pump_output(p, &mut pb_audio_out);
            }
        }
        if last_status.elapsed() >= Duration::from_millis(100) {
            let playback_in = pb_meter.mean_input();
            let playback_out = pb_meter.mean_output();
            let recording_in = rec_meter.mean_input();
            let recording_out = rec_meter.mean_output();
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
                        pb_meter.count, rec_meter.count
                    );
                }
            }
            pb_meter.reset();
            rec_meter.reset();
            last_status = Instant::now();
        }

        let pb_idle = !has_playback || (pb_audio_in.len() < frame_size && pb_audio_out.len() == 0);
        let rec_idle =
            !capture_mic || (rec_audio_in.len() < frame_size && rec_audio_out.len() == 0);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The meters report a per-channel mean, so a frame at a constant amplitude
    /// reads back as that amplitude rather than the two-channel sum.
    #[test]
    fn meter_reports_per_channel_mean() {
        let mut meter = RmsMeter::default();
        meter.push(1.0, 0.5); // per-frame sums across two channels
        assert_eq!(meter.count, 2);
        assert_eq!(meter.mean_input(), 0.5);
        assert_eq!(meter.mean_output(), 0.25);

        meter.push(1.0, 0.5);
        assert_eq!(meter.count, 4);
        assert_eq!(
            meter.mean_input(),
            0.5,
            "mean must not drift as frames add up"
        );
    }

    #[test]
    fn empty_meter_reads_zero_rather_than_dividing_by_zero() {
        let meter = RmsMeter::default();
        assert_eq!(meter.count, 0);
        assert_eq!(meter.mean_input(), 0.0);
        assert_eq!(meter.mean_output(), 0.0);
    }

    #[test]
    fn reset_clears_every_field() {
        let mut meter = RmsMeter::default();
        meter.push(3.0, 4.0);
        meter.reset();
        assert_eq!(meter.count, 0);
        assert_eq!(meter.input, 0.0);
        assert_eq!(meter.output, 0.0);
    }

    /// One interleaved stereo frame with each channel at a constant amplitude.
    fn frame_at(left: f32, right: f32) -> Vec<f32> {
        let mut frame = Vec::with_capacity(FRAME_SIZE);
        for _ in 0..CHUNK {
            frame.push(left);
            frame.push(right);
        }
        frame
    }

    /// Drives the real `process_chain` in bypass, where the audio must come out
    /// untouched and both meters must read the input level. compute_rms of a
    /// constant signal is that constant, so the per-channel mean of 0.5 and 0.25
    /// is 0.375.
    #[test]
    fn process_chain_bypass_passes_audio_through_and_meters_it() {
        let mut audio_in = AudioBuf::new();
        let mut audio_out = AudioBuf::new();
        let mut meter = RmsMeter::default();
        let mut denoiser = Denoiser::new(&Model::default(), 2).expect("build denoiser");
        audio_in.extend(&frame_at(0.5, 0.25));

        process_chain(
            &mut audio_in,
            &mut audio_out,
            &mut denoiser,
            true,
            FRAME_SIZE,
            &mut meter,
        );

        assert_eq!(audio_out.len(), FRAME_SIZE, "frame must be copied through");
        assert_eq!(audio_out.data[0], 0.5);
        assert_eq!(audio_out.data[1], 0.25);
        assert_eq!(audio_in.len(), 0, "frame must be consumed");
        assert_eq!(meter.count, 2);
        assert!((meter.mean_input() - 0.375).abs() < 1e-6);
        assert!(
            (meter.mean_output() - meter.mean_input()).abs() < 1e-6,
            "bypass must report identical input and output levels"
        );
    }

    /// The meters must report real levels when audio actually flows through the
    /// model — the claim a live run cannot check without microphone access.
    #[test]
    fn process_chain_meters_real_audio_through_the_denoiser() {
        let mut audio_in = AudioBuf::new();
        let mut audio_out = AudioBuf::new();
        let mut meter = RmsMeter::default();
        let mut denoiser = Denoiser::new(&Model::default(), 2).expect("build denoiser");

        const FRAMES: usize = 8;
        for _ in 0..FRAMES {
            audio_in.extend(&frame_at(0.4, 0.4));
        }
        for _ in 0..FRAMES {
            process_chain(
                &mut audio_in,
                &mut audio_out,
                &mut denoiser,
                false,
                FRAME_SIZE,
                &mut meter,
            );
        }

        assert_eq!(audio_in.len(), 0, "every frame must be consumed");
        assert_eq!(
            audio_out.len(),
            FRAMES * FRAME_SIZE,
            "one frame out per frame in"
        );
        assert_eq!(meter.count, (FRAMES * 2) as u32);
        assert!(
            (meter.mean_input() - 0.4).abs() < 1e-3,
            "input meter must read the true input level, got {}",
            meter.mean_input()
        );
        assert!(
            meter.mean_output() >= 0.0 && meter.mean_output().is_finite(),
            "output meter must stay a sane level, got {}",
            meter.mean_output()
        );
    }

    #[test]
    fn drop_excess_bounds_latency_to_whole_frames() {
        let mut buf = AudioBuf::new();
        buf.extend(&vec![0.1f32; (MAX_BUFFER_FRAMES + 4) * FRAME_SIZE]);
        buf.drop_excess();
        assert_eq!(
            buf.len(),
            MAX_BUFFER_FRAMES * FRAME_SIZE,
            "must drop down to the watermark, in whole frames"
        );
    }
}
