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
use libpulse_binding::def::BufferAttr;
use pulse::context::{Context, FlagSet as CtxFlags};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, Stream};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

const SINK_NAME: &str = "upalla_sink";
const SRC_SINK_NAME: &str = "upalla_src_sink";
const SRC_VIRTUAL_NAME: &str = "upalla_virtual";
const REMAINDER_CAP: usize = 16384;

pub struct Status {
    pub rms_in: f32,
    pub rms_out: f32,
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
    let readable = stream.readable_size().unwrap_or(0) as usize;
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
    let writable = stream.writable_size().unwrap_or(0) as usize;
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
                if let (Some(name), Some(desc)) =
                    (info.name.as_ref(), info.description.as_ref())
                {
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
                if let (Some(name), Some(desc)) =
                    (info.name.as_ref(), info.description.as_ref())
                {
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
}

pub fn run_filter(
    model: Model,
    cmd_rx: Receiver<Cmd>,
    status_tx: Sender<Status>,
    bypass: Arc<AtomicBool>,
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
        "sink_name={0} sink_properties=\"device.description='Upalla Denoised Input Monitor' device.class='filter' device.profile.description='Denoised Input Monitor'\"",
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
        fragsize: 48,
    });
    let playback_attr = Some(BufferAttr {
        maxlength: u32::MAX,
        tlength: 48,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: u32::MAX,
    });

    let mut sink_rec = Stream::new(&mut context, "sink-rec", &spec, None).context("sink rec")?;
    sink_rec.connect_record(
        Some(&format!("{}.monitor", SINK_NAME)),
        record_attr.as_ref(),
        recv_flags,
    )?;

    let mut sink_play =
        Stream::new(&mut context, "sink-play", &spec, None).context("sink play")?;
    let mut sink_play_dest = "@DEFAULT_SINK@".to_string();
    sink_play.connect_playback(
        Some(&sink_play_dest),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;

    let mut src_rec = Stream::new(&mut context, "src-rec", &spec, None).context("src rec")?;
    let mut src_rec_source = "@DEFAULT_SOURCE@".to_string();
    src_rec.connect_record(Some(&src_rec_source), record_attr.as_ref(), recv_flags)?;

    let mut src_play =
        Stream::new(&mut context, "src-play", &spec, None).context("src play")?;
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
    let frame_size = CHUNK * 2;

    let mut last_status = Instant::now();
    let mut rms_accum = [0.0f32; 4];
    let mut rms_count = 0u32;

    loop {
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
                        Stream::new(&mut context, "sink-play", &spec, None)
                            .context("sink play")?,
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
                }
                Cmd::SetSource(name) => {
                    log::info!("Switching source input to {name}");
                    src_rec_source = name;
                    drop(std::mem::replace(
                        &mut src_rec,
                        Stream::new(&mut context, "src-rec", &spec, None)
                            .context("src rec")?,
                    ));
                    src_rec.connect_record(
                        Some(&src_rec_source),
                        record_attr.as_ref(),
                        recv_flags,
                    )?;
                    src_in.data.clear();
                    src_in.pos = 0;
                }
                Cmd::Shutdown => {
                    log::info!("PA filter received shutdown command");
                    break;
                }
            }
        }

        pump_read(&mut sink_rec, &mut sink_in);
        pump_read(&mut src_rec, &mut src_in);

        let is_bypass = bypass.load(Ordering::Relaxed);

        while let Some(frame) = sink_in.drain_frames(frame_size) {
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
                    sink_out.data.push(sc.left[i]);
                    sink_out.data.push(sc.right[i]);
                }
                rms_accum[0] += compute_rms(&sc.left);
                rms_accum[1] += compute_rms(&sc.right);
                rms_accum[2] += compute_rms(&sc.left);
                rms_accum[3] += compute_rms(&sc.right);
                rms_count += 1;
            } else if let Ok(out) = denoiser_sink.process_stereo(&sc) {
                for i in 0..CHUNK {
                    sink_out.data.push(out.left[i]);
                    sink_out.data.push(out.right[i]);
                }
                rms_accum[0] += compute_rms(&sc.left);
                rms_accum[1] += compute_rms(&sc.right);
                rms_accum[2] += compute_rms(&out.left);
                rms_accum[3] += compute_rms(&out.right);
                rms_count += 1;
            }
        }
        while let Some(frame) = src_in.drain_frames(frame_size) {
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
                    src_out.data.push(sc.left[i]);
                    src_out.data.push(sc.right[i]);
                }
            } else if let Ok(out) = denoiser_src.process_stereo(&sc) {
                for i in 0..CHUNK {
                    src_out.data.push(out.left[i]);
                    src_out.data.push(out.right[i]);
                }
            }
        }

        pump_write(&mut sink_play, &mut sink_out);
        pump_write(&mut src_play, &mut src_out);

        if last_status.elapsed() >= Duration::from_millis(100) {
            let (rms_in, rms_out) = if rms_count > 0 {
                let c = rms_count as f32;
                let in_rms = ((rms_accum[0] + rms_accum[1]) / (2.0 * c)).sqrt();
                let out_rms = ((rms_accum[2] + rms_accum[3]) / (2.0 * c)).sqrt();
                (in_rms, out_rms)
            } else {
                (0.0, 0.0)
            };
            let _ = status_tx.try_send(Status {
                rms_in,
                rms_out,
            });
            rms_accum = [0.0; 4];
            rms_count = 0;
            last_status = Instant::now();
        }

        match mainloop.iterate(false) {
            IterateResult::Quit(_) | IterateResult::Err(_) => break,
            _ => {}
        }
        if sink_in.len() < frame_size
            && src_in.len() < frame_size
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
