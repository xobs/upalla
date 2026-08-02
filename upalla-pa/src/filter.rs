#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use libpulse_binding as pulse;
use libpulse_binding::def::{BufferAttr, Retval};
use pulse::callbacks::ListResult;
use pulse::context::{Context, FlagSet as CtxFlags};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, Stream};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

const SINK_NAME: &str = "upalla_sink";
const SRC_SINK_NAME: &str = "upalla_src_sink";
const SRC_VIRTUAL_NAME: &str = "upalla_virtual";
const REMAINDER_CAP: usize = 16384;
/// Maximum number of stereo frames to buffer unprocessed.
/// Beyond this, the oldest frames are dropped to bound latency.
const MAX_BUFFER_FRAMES: usize = 8;
const FRAME_SIZE: usize = CHUNK * 2; // 960 f32 samples = 480 stereo samples = 10ms at 48kHz
/// How often to check whether any app is listening on the virtual source.
const LISTENER_CHECK_INTERVAL: Duration = Duration::from_millis(500);

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
    /// Drop oldest whole frames if total unprocessed samples exceed the max.
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

fn cleanup_stale_modules() {
    let Ok(out) = Command::new("pactl").args(["list", "modules"]).output() else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut current: Option<u32> = None;
    for line in text.lines() {
        if let Some(idx) = line.strip_prefix("Module #") {
            current = idx.trim().parse().ok();
        }
        if line.contains("upalla") {
            if let Some(idx) = current {
                log::info!("Cleaning up stale module {}", idx);
                let _ = Command::new("pactl")
                    .args(["unload-module", &idx.to_string()])
                    .output();
            }
        }
    }
}

fn unload_module(idx: u32) {
    let _ = Command::new("pactl")
        .args(["unload-module", &idx.to_string()])
        .output();
}

#[derive(Default)]
struct RegisteredModules {
    sink: Option<NonZeroU32>,
    source: Option<NonZeroU32>,
    remap: Option<NonZeroU32>,
}

impl Drop for RegisteredModules {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take().map(|s| s.get()) {
            log::debug!("Unloading sink module {sink}");
            unload_module(sink);
        }
        if let Some(source) = self.source.take().map(|s| s.get()) {
            log::debug!("Unloading source module {source}");
            unload_module(source);
        }
        if let Some(remap) = self.remap.take().map(|s| s.get()) {
            log::debug!("Unloading remap module {remap}");
            unload_module(remap);
        }
    }
}

fn pump_read(stream: &mut Stream, buf: &mut AudioBuf) {
    let readable = stream.readable_size().unwrap_or(0);
    if readable < 4 {
        return;
    }
    match stream.peek() {
        Ok(PeekResult::Data(data)) => {
            let n = data.len() / 4;
            let f32s: &[f32] =
                unsafe { std::slice::from_raw_parts(data.as_ptr() as *const f32, n) };
            buf.extend(f32s);
            let _ = stream.discard();
        }
        Ok(PeekResult::Hole(_)) => {
            let _ = stream.discard();
        }
        _ => {}
    }
}

fn pump_write(stream: &mut Stream, buf: &mut AudioBuf) {
    let bps: usize = 4;
    let writable = stream.writable_size().unwrap_or(0);
    let avail = buf.len().min(writable / bps);
    if avail == 0 {
        return;
    }
    let chunk = buf.take_available(avail);
    let data: &[u8] =
        unsafe { std::slice::from_raw_parts(chunk.as_ptr() as *const u8, chunk.len() * bps) };
    let _ = stream.write_copy(data, 0, pulse::stream::SeekMode::Relative);
}

fn compute_rms(chunk: &[f32]) -> f32 {
    let sum: f32 = chunk.iter().map(|&s| s * s).sum();
    (sum / chunk.len() as f32).sqrt()
}

fn enumerate_devices(mainloop: &mut Mainloop, context: &Context) -> DeviceLists {
    use libpulse_binding::callbacks::ListResult;

    // Get default device names via server info
    let dsn = Rc::new(RefCell::new(String::new()));
    let dso = Rc::new(RefCell::new(String::new()));
    let done = Rc::new(RefCell::new(false));
    {
        let sn = dsn.clone();
        let so = dso.clone();
        let d = done.clone();
        let intro = context.introspect();
        intro.get_server_info(move |info| {
            *sn.borrow_mut() = info
                .default_sink_name
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default();
            *so.borrow_mut() = info
                .default_source_name
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default();
            *d.borrow_mut() = true;
        });
    }
    while !*done.borrow() {
        mainloop.iterate(true);
    }
    let default_sink = dsn.take();
    let default_source = dso.take();

    // Query sinks
    let sinks = collect_list(mainloop, context, |intro, l, d| {
        intro.get_sink_info_list(move |result| match result {
            ListResult::Item(info) => {
                if let (Some(name), Some(desc)) = (info.name.as_ref(), info.description.as_ref()) {
                    l.borrow_mut().push(DeviceInfo {
                        name: name.to_string(),
                        description: desc.to_string(),
                    });
                }
            }
            ListResult::End | ListResult::Error => *d.borrow_mut() = true,
        });
    });

    // Query sources
    let sources = collect_list(mainloop, context, |intro, l, d| {
        intro.get_source_info_list(move |result| match result {
            ListResult::Item(info) => {
                if let (Some(name), Some(desc)) = (info.name.as_ref(), info.description.as_ref()) {
                    l.borrow_mut().push(DeviceInfo {
                        name: name.to_string(),
                        description: desc.to_string(),
                    });
                }
            }
            ListResult::End | ListResult::Error => *d.borrow_mut() = true,
        });
    });

    log::info!(
        "Enumerated {} sinks, {} sources (default sink: {}, default source: {})",
        sinks.len(),
        sources.len(),
        default_sink,
        default_source,
    );
    DeviceLists {
        sinks,
        sources,
        default_sink,
        default_source,
    }
}

fn collect_list<F>(mainloop: &mut Mainloop, context: &Context, register: F) -> Vec<DeviceInfo>
where
    F: FnOnce(
        pulse::context::introspect::Introspector,
        Rc<RefCell<Vec<DeviceInfo>>>,
        Rc<RefCell<bool>>,
    ),
{
    let list = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(RefCell::new(false));
    let intro = context.introspect();
    register(intro, list.clone(), done.clone());
    while !*done.borrow() {
        mainloop.iterate(true);
    }
    list.take()
        .into_iter()
        .filter(|d| !d.name.contains("upalla"))
        .collect()
}

/// Check whether any source output is connected to the named source
/// using PulseAudio's introspection API.
fn has_source_outputs(mainloop: &mut Mainloop, context: &Context, name: &str) -> bool {
    let src_idx = Rc::new(RefCell::new(0u32));
    let idx_done = Rc::new(RefCell::new(false));
    let _idx_op = context.introspect().get_source_info_by_name(name, {
        let idx = src_idx.clone();
        let done = idx_done.clone();
        move |result| match result {
            ListResult::Item(info) => *idx.borrow_mut() = info.index,
            ListResult::End | ListResult::Error => *done.borrow_mut() = true,
        }
    });
    while !*idx_done.borrow() {
        if matches!(
            mainloop.iterate(true),
            IterateResult::Quit(_) | IterateResult::Err(_)
        ) {
            return false;
        }
    }
    let src_idx = *src_idx.borrow();
    if src_idx == 0 {
        return false;
    }

    let has = Rc::new(RefCell::new(false));
    let out_done = Rc::new(RefCell::new(false));
    let _out_op = context.introspect().get_source_output_info_list({
        let h = has.clone();
        let d = out_done.clone();
        move |result| match result {
            ListResult::Item(info) => {
                if info.source == src_idx {
                    *h.borrow_mut() = true;
                }
            }
            ListResult::End | ListResult::Error => *d.borrow_mut() = true,
        }
    });
    while !*out_done.borrow() {
        if matches!(
            mainloop.iterate(true),
            IterateResult::Quit(_) | IterateResult::Err(_)
        ) {
            return false;
        }
    }
    let active = *has.borrow();
    active
}

/// Check whether any sink input (app) is connected to a named sink
/// using PulseAudio's introspection API.
fn has_sink_inputs(mainloop: &mut Mainloop, context: &Context, name: &str) -> bool {
    let sink_idx = Rc::new(RefCell::new(0u32));
    let idx_done = Rc::new(RefCell::new(false));
    let _idx_op = context.introspect().get_sink_info_by_name(name, {
        let idx = sink_idx.clone();
        let done = idx_done.clone();
        move |result| match result {
            ListResult::Item(info) => *idx.borrow_mut() = info.index,
            ListResult::End | ListResult::Error => *done.borrow_mut() = true,
        }
    });
    while !*idx_done.borrow() {
        if matches!(
            mainloop.iterate(true),
            IterateResult::Quit(_) | IterateResult::Err(_)
        ) {
            return false;
        }
    }
    let sink_idx = *sink_idx.borrow();
    if sink_idx == 0 {
        return false;
    }

    let has = Rc::new(RefCell::new(false));
    let out_done = Rc::new(RefCell::new(false));
    let _out_op = context.introspect().get_sink_input_info_list({
        let h = has.clone();
        let d = out_done.clone();
        move |result| match result {
            ListResult::Item(info) => {
                if info.sink == sink_idx {
                    *h.borrow_mut() = true;
                }
            }
            ListResult::End | ListResult::Error => *d.borrow_mut() = true,
        }
    });
    while !*out_done.borrow() {
        if matches!(
            mainloop.iterate(true),
            IterateResult::Quit(_) | IterateResult::Err(_)
        ) {
            return false;
        }
    }
    let active = *has.borrow();
    active
}

pub fn run_filter(
    model: Model,
    cmd_rx: Receiver<Cmd>,
    status_tx: Sender<Status>,
    playback_enable: Arc<AtomicBool>,
    recording_enable: Arc<AtomicBool>,
) -> Result<()> {
    cleanup_stale_modules();

    let mut registered_modules = RegisteredModules::default();

    let mut mainloop = Mainloop::new().context("PA mainloop")?;
    let mut context = Context::new(&mainloop, "upalla").context("PA context")?;
    context.connect(None, CtxFlags::NOFLAGS, None)?;

    while context.get_state() != pulse::context::State::Ready {
        mainloop.iterate(true);
    }
    log::info!("PA context ready");

    let sink_args = format!(
        "sink_name={0} sink_properties=\"device.description='Upalla Denoised Output' device.profile.description='Denoised Output'\"",
        SINK_NAME
    );
    let sink_module = Rc::new(RefCell::new(0u32));
    {
        let sm = sink_module.clone();
        let mut intro = context.introspect();
        intro.load_module("module-null-sink", &sink_args, move |idx: u32| {
            *sm.borrow_mut() = idx;
        });
    }
    while *sink_module.borrow() == 0 {
        mainloop.iterate(true);
    }
    let sink_module_id = *sink_module.borrow();
    log::info!("Output sink loaded (idx={sink_module_id})");
    registered_modules.sink = NonZeroU32::new(sink_module_id);

    let src_sink_args = format!(
        "sink_name={0} sink_properties=\"device.description='Upalla Denoised Input Monitor' device.class='filter' state.ignore=true device.profile.description='Denoised Input Monitor'\"",
        SRC_SINK_NAME
    );
    let source_module = Rc::new(RefCell::new(0u32));
    {
        let sm = source_module.clone();
        let mut intro = context.introspect();
        intro.load_module("module-null-sink", &src_sink_args, move |idx: u32| {
            *sm.borrow_mut() = idx;
        });
    }
    while *source_module.borrow() == 0 {
        mainloop.iterate(true);
    }
    let source_module_id = *source_module.borrow();
    log::info!("Source sink loaded (idx={source_module_id})");
    registered_modules.source = NonZeroU32::new(source_module_id);

    let remap_args = format!(
        "source_name={0} source_properties=\"device.description='Upalla Denoised Input' device.profile.description='Denoised Input'\" master={1}.monitor",
        SRC_VIRTUAL_NAME, SRC_SINK_NAME
    );
    let remap_module = Rc::new(RefCell::new(0u32));
    {
        let rm = remap_module.clone();
        let mut intro = context.introspect();
        intro.load_module("module-remap-source", &remap_args, move |idx: u32| {
            *rm.borrow_mut() = idx;
            log::info!("Loaded remap-source module, index={}", idx);
        });
    }
    while *remap_module.borrow() == 0 {
        mainloop.iterate(true);
    }
    let remap_module_id = *remap_module.borrow();
    log::info!("Remap source loaded (idx={remap_module_id})");
    registered_modules.remap = NonZeroU32::new(remap_module_id);

    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::F32le,
        rate: 48000,
        channels: 2,
    };
    let recv_flags = StreamFlags::ADJUST_LATENCY | StreamFlags::AUTO_TIMING_UPDATE;
    let play_flags = StreamFlags::ADJUST_LATENCY | StreamFlags::AUTO_TIMING_UPDATE;
    let record_attr = Some(BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: 50,
    });
    let playback_attr = Some(BufferAttr {
        maxlength: u32::MAX,
        tlength: 50,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: u32::MAX,
    });

    let mut sink_rec: Option<Stream> = None;
    let mut capture_sink = false;
    let mut sink_play = Stream::new(&mut context, "sink-play", &spec, None).context("sink play")?;
    let sink_play_dest = "@DEFAULT_SINK@".to_string();
    sink_play.connect_playback(
        Some(&sink_play_dest),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;

    let mut sink_play = Stream::new(&mut context, "sink-play", &spec, None).context("sink play")?;
    let mut sink_play_dest = "@DEFAULT_SINK@".to_string();
    sink_play.connect_playback(
        Some(&sink_play_dest),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;

    let mut src_rec: Option<Stream> = None;
    let mut src_rec_source = "@DEFAULT_SOURCE@".to_string();
    let mut capture_src = false;
    let mut listener_check = Instant::now();
    let mut src_play = Stream::new(&mut context, "src-play", &spec, None).context("src play")?;
    src_play.connect_playback(
        Some(SRC_SINK_NAME),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;
    log::info!("Streams connected. Processing...");
    let mut denoiser_sink = Denoiser::new(&model, 2)?;
    let mut denoiser_src = Denoiser::new(&model.clone(), 2)?;
    let mut sink_in = AudioBuf::new();
    let mut sink_out = AudioBuf::new();
    let mut src_in = AudioBuf::new();
    let mut src_out = AudioBuf::new();
    let mut last_status = Instant::now();
    let mut rms_accum = [0.0f32; 8];
    let mut rms_count_sink = 0u32;
    let mut rms_count_src = 0u32;

    loop {
        // Periodically check for active apps on our virtual sink and source
        if listener_check.elapsed() >= LISTENER_CHECK_INTERVAL {
            let has_sink = has_sink_inputs(&mut mainloop, &context, SINK_NAME);
            let has_src = has_source_outputs(&mut mainloop, &context, SRC_VIRTUAL_NAME);

            // Playback chain: capture from output null sink when apps route to it
            if has_sink && !capture_sink {
                let Some(mut new_rec) = Stream::new(&mut context, "sink-rec", &spec, None) else {
                    log::error!("Failed to create sink-rec stream");
                    listener_check = Instant::now();
                    continue;
                };
                match new_rec.connect_record(
                    Some(&format!("{}.monitor", SINK_NAME)),
                    record_attr.as_ref(),
                    recv_flags,
                ) {
                    Ok(()) => {
                        sink_rec = Some(new_rec);
                        sink_in.data.clear();
                        sink_in.pos = 0;
                        capture_sink = true;
                        log::info!("Playback capture started (sink input on {SINK_NAME})");
                    }
                    Err(e) => log::error!("Failed to start playback capture: {e}"),
                }
            } else if !has_sink && capture_sink {
                sink_in.data.clear();
                sink_in.pos = 0;
                sink_out.data.clear();
                sink_out.pos = 0;
                rms_count_sink = 0;
                rms_accum[..4].fill(0.0);
                sink_rec = None;
                capture_sink = false;
                log::info!("Playback capture stopped (no sink inputs on {SINK_NAME})");
            }

            // Recording chain: capture from mic when apps listen on virtual source
            if has_src && !capture_src {
                let Some(mut new_rec) = Stream::new(&mut context, "src-rec", &spec, None) else {
                    log::error!("Failed to create src-rec stream");
                    listener_check = Instant::now();
                    continue;
                };
                match new_rec.connect_record(
                    Some(&src_rec_source),
                    record_attr.as_ref(),
                    recv_flags,
                ) {
                    Ok(()) => {
                        src_rec = Some(new_rec);
                        src_in.data.clear();
                        src_in.pos = 0;
                        capture_src = true;
                        log::info!("Mic capture started (listener on {SRC_VIRTUAL_NAME})");
                    }
                    Err(e) => log::error!("Failed to start mic capture: {e}"),
                }
            } else if !has_src && capture_src {
                src_in.data.clear();
                src_in.pos = 0;
                src_out.data.clear();
                src_out.pos = 0;
                rms_count_src = 0;
                rms_accum[4..8].fill(0.0);
                src_rec = None;
                capture_src = false;
                log::info!("Mic capture stopped (no listeners on {SRC_VIRTUAL_NAME})");
            }

            listener_check = Instant::now();
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::SwitchModel(new_model) => {
                    log::info!("Switching model to {}", new_model.label());
                    denoiser_sink = Denoiser::new(&new_model, 2)?;
                    denoiser_src = Denoiser::new(&new_model, 2)?;
                }
                Cmd::EnumerateDevices(tx) => {
                    let devices = enumerate_devices(&mut mainloop, &context);
                    let _ = tx.send(devices);
                }
                Cmd::SetSink(name) => {
                    log::info!("Switching sink output to {name}");
                    sink_play_dest = name;
                    drop(std::mem::replace(
                        &mut sink_play,
                        Stream::new(&mut context, "sink-play", &spec, None).context("sink play")?,
                    ));
                    sink_play.connect_playback(
                        Some(&sink_play_dest),
                        playback_attr.as_ref(),
                        play_flags,
                        None,
                        None,
                    )?;
                    sink_out.data.clear();
                    sink_out.pos = 0;
                    let Some(mut new_rec) = Stream::new(&mut context, "sink-rec", &spec, None)
                    else {
                        log::error!("Failed to create sink-rec stream");
                        break;
                    };
                    if let Err(e) = new_rec.connect_record(
                        Some(&format!("{}.monitor", SINK_NAME)),
                        record_attr.as_ref(),
                        recv_flags,
                    ) {
                        log::error!("Failed to connect sink-rec: {e}");
                    } else {
                        sink_rec = Some(new_rec);
                        capture_sink = true;
                    }
                }
                Cmd::SetSource(name) => {
                    log::info!("Switching source input to {name}");
                    src_rec_source = name;
                    let Some(mut new_rec) = Stream::new(&mut context, "src-rec", &spec, None)
                    else {
                        break;
                    };
                    if let Err(e) = new_rec.connect_record(
                        Some(&src_rec_source),
                        record_attr.as_ref(),
                        recv_flags,
                    ) {
                        log::error!("Failed to connect src-rec: {e}");
                    } else {
                        src_rec = Some(new_rec);
                        src_in.data.clear();
                        src_in.pos = 0;
                        capture_src = true;
                    }
                }
                Cmd::Shutdown => {
                    log::info!("PA filter received shutdown command");
                    mainloop.quit(Retval(0));
                }
            }
        }

        if let Some(ref mut stream) = sink_rec {
            pump_read(stream, &mut sink_in);
        }
        if let Some(ref mut stream) = src_rec {
            pump_read(stream, &mut src_in);
        }

        // Drop excess input to bound latency when processing falls behind
        sink_in.drop_excess();
        src_in.drop_excess();

        let sink_bypass = !playback_enable.load(Ordering::Relaxed);
        let src_bypass = !recording_enable.load(Ordering::Relaxed);

        if let Some(frame) = sink_in.drain_frames(FRAME_SIZE) {
            let mut sc = StereoChunk {
                left: [0.0; CHUNK],
                right: [0.0; CHUNK],
            };
            for i in 0..CHUNK {
                sc.left[i] = frame[i * 2];
                sc.right[i] = frame[i * 2 + 1];
            }
            if sink_bypass {
                for i in 0..CHUNK {
                    sink_out.data.push(sc.left[i]);
                    sink_out.data.push(sc.right[i]);
                }
                rms_accum[0] += compute_rms(&sc.left);
                rms_accum[1] += compute_rms(&sc.right);
                rms_accum[2] += compute_rms(&sc.left);
                rms_accum[3] += compute_rms(&sc.right);
                rms_count_sink += 1;
            } else {
                match denoiser_sink.process_stereo(&sc) {
                    Ok(out) => {
                        for i in 0..CHUNK {
                            sink_out.data.push(out.left[i]);
                            sink_out.data.push(out.right[i]);
                        }
                        rms_accum[0] += compute_rms(&sc.left);
                        rms_accum[1] += compute_rms(&sc.right);
                        rms_accum[2] += compute_rms(&out.left);
                        rms_accum[3] += compute_rms(&out.right);
                        rms_count_sink += 1;
                    }
                    Err(e) => {
                        log::error!("Denoiser sink error: {e}, falling back to bypass");
                        for i in 0..CHUNK {
                            sink_out.data.push(sc.left[i]);
                            sink_out.data.push(sc.right[i]);
                        }
                        rms_accum[0] += compute_rms(&sc.left);
                        rms_accum[1] += compute_rms(&sc.right);
                        rms_accum[2] += compute_rms(&sc.left);
                        rms_accum[3] += compute_rms(&sc.right);
                        rms_count_sink += 1;
                    }
                }
            }
        }
        if let Some(frame) = src_in.drain_frames(FRAME_SIZE) {
            let mut sc = StereoChunk {
                left: [0.0; CHUNK],
                right: [0.0; CHUNK],
            };
            for i in 0..CHUNK {
                sc.left[i] = frame[i * 2];
                sc.right[i] = frame[i * 2 + 1];
            }
            if src_bypass {
                for i in 0..CHUNK {
                    src_out.data.push(sc.left[i]);
                    src_out.data.push(sc.right[i]);
                }
                rms_accum[4] += compute_rms(&sc.left);
                rms_accum[5] += compute_rms(&sc.right);
                rms_accum[6] += compute_rms(&sc.left);
                rms_accum[7] += compute_rms(&sc.right);
                rms_count_src += 1;
            } else {
                match denoiser_src.process_stereo(&sc) {
                    Ok(out) => {
                        for i in 0..CHUNK {
                            src_out.data.push(out.left[i]);
                            src_out.data.push(out.right[i]);
                        }
                        rms_accum[4] += compute_rms(&sc.left);
                        rms_accum[5] += compute_rms(&sc.right);
                        rms_accum[6] += compute_rms(&out.left);
                        rms_accum[7] += compute_rms(&out.right);
                        rms_count_src += 1;
                    }
                    Err(e) => {
                        log::error!("Denoiser src error: {e}, falling back to bypass");
                        for i in 0..CHUNK {
                            src_out.data.push(sc.left[i]);
                            src_out.data.push(sc.right[i]);
                        }
                        rms_accum[4] += compute_rms(&sc.left);
                        rms_accum[5] += compute_rms(&sc.right);
                        rms_accum[6] += compute_rms(&sc.left);
                        rms_accum[7] += compute_rms(&sc.right);
                        rms_count_src += 1;
                    }
                }
            }
        }

        pump_write(&mut sink_play, &mut sink_out);
        pump_write(&mut src_play, &mut src_out);

        if last_status.elapsed() >= Duration::from_millis(100) {
            let playback_in = if rms_count_sink > 0 {
                (rms_accum[0] + rms_accum[1]) / (2.0 * rms_count_sink as f32)
            } else {
                0.0
            };
            let playback_out = if rms_count_sink > 0 {
                (rms_accum[2] + rms_accum[3]) / (2.0 * rms_count_sink as f32)
            } else {
                0.0
            };
            let recording_in = if rms_count_src > 0 {
                (rms_accum[4] + rms_accum[5]) / (2.0 * rms_count_src as f32)
            } else {
                0.0
            };
            let recording_out = if rms_count_src > 0 {
                (rms_accum[6] + rms_accum[7]) / (2.0 * rms_count_src as f32)
            } else {
                0.0
            };
            let _ = status_tx.try_send(Status {
                playback_in,
                playback_out,
                recording_in,
                recording_out,
            });
            rms_accum = [0.0; 8];
            rms_count_sink = 0;
            rms_count_src = 0;
            last_status = Instant::now();
        }

        match mainloop.iterate(false) {
            IterateResult::Quit(_) | IterateResult::Err(_) => break,
            _ => {}
        }
        if sink_in.len() < FRAME_SIZE
            && src_in.len() < FRAME_SIZE
            && sink_out.len() == 0
            && src_out.len() == 0
        {
            std::thread::sleep(Duration::from_micros(500));
        }
    }

    log::info!("Cleaning up...");
    drop(sink_rec);
    drop(sink_play);
    drop(src_rec);
    drop(src_play);

    context.disconnect();

    log::info!("Upalla PA filter stopped.");
    Ok(())
}
