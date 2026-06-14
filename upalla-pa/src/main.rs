//! Upalla PulseAudio filter — noise suppression for system audio.
//!
//! Sink path:  apps → upalla_sink → capture → denoise → @DEFAULT_SINK@
//! Source path: @DEFAULT_SOURCE@ → capture → denoise → upalla_src_sink → upalla_virtual → apps

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use libpulse_binding as pulse;
use libpulse_binding::def::BufferAttr;
use pulse::context::{Context, FlagSet as CtxFlags};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, Stream};
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};

const SINK_NAME: &str = "upalla_sink";
const SRC_SINK_NAME: &str = "upalla_src_sink";
const SRC_VIRTUAL_NAME: &str = "upalla_virtual";
const REMAINDER_CAP: usize = 16384;

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

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Upalla PulseAudio filter starting...");
    let mut registered_modules = RegisteredModules::default();

    cleanup_stale_modules();

    let mut mainloop = Mainloop::new().expect("PA mainloop");
    let mut context = Context::new(&mainloop, "upalla").expect("PA context");
    context.connect(None, CtxFlags::NOFLAGS, None)?;

    while context.get_state() != pulse::context::State::Ready {
        mainloop.iterate(true);
    }
    log::info!("PA context ready");

    // Output sink (visible playback device)
    let sink_args = format!(
        "sink_name={0} sink_properties=\"device.description='Upalla Denoised Output' device.profile.description='Denoised Output'",
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
    let sink_module = *sink_module.borrow();
    log::info!("Output sink loaded (idx={sink_module})");
    registered_modules.sink = NonZeroU32::new(sink_module);

    // Internal sink for source path audio routing
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
    let source_module = *source_module.borrow();
    log::info!("Source sink loaded (idx={source_module})");
    registered_modules.source = NonZeroU32::new(source_module);

    // Remap-source: exposes the source sink's monitor as a proper recording device
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
    let remap_module = *remap_module.borrow();
    log::info!("Remap source loaded (idx={remap_module})");
    registered_modules.remap = NonZeroU32::new(remap_module);

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

    // Sink path: capture from output sink monitor → denoise → play to real sink
    let mut sink_rec = Stream::new(&mut context, "sink-rec", &spec, None).expect("sink rec");
    log::debug!("Connecting playback to denoise...");
    sink_rec.connect_record(
        Some(&format!("{}.monitor", SINK_NAME)),
        record_attr.as_ref(),
        recv_flags,
    )?;
    log::debug!("sink_rec buffer_addr: {:?}", sink_rec.get_buffer_attr());

    let mut sink_play = Stream::new(&mut context, "sink-play", &spec, None).expect("sink play");
    log::debug!("Connecting denoise to playback...");
    sink_play.connect_playback(
        Some("@DEFAULT_SINK@"),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;
    log::debug!("sink_play buffer_addr: {:?}", sink_play.get_buffer_attr());

    // Source path: capture from real mic → denoise → play to internal sink
    let mut src_rec = Stream::new(&mut context, "src-rec", &spec, None).expect("src rec");
    log::debug!("Connecting record to denoise...");
    src_rec.connect_record(Some("@DEFAULT_SOURCE@"), record_attr.as_ref(), recv_flags)?;
    log::debug!("src_rec buffer_addr: {:?}", src_rec.get_buffer_attr());

    let mut src_play = Stream::new(&mut context, "src-play", &spec, None).expect("src play");
    log::debug!("Connecting denoise to record...");
    src_play.connect_playback(
        Some(SRC_SINK_NAME),
        playback_attr.as_ref(),
        play_flags,
        None,
        None,
    )?;
    log::debug!("src_play buffer_addr: {:?}", src_play.get_buffer_attr());

    log::info!("Streams connected. Processing...");

    let mut sink_denoiser = Denoiser::new(&std::path::PathBuf::from("."), 2)?;
    let mut src_denoiser = Denoiser::new(&std::path::PathBuf::from("."), 2)?;
    let mut sink_in = AudioBuf::new();
    let mut sink_out = AudioBuf::new();
    let mut src_in = AudioBuf::new();
    let mut src_out = AudioBuf::new();
    let frame_size = CHUNK * 2;

    let shutting_down = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler({
        let shutting_down = Arc::clone(&shutting_down);
        move || {
            if !shutting_down.swap(true, Ordering::SeqCst) {
                println!("Ctrl-C pressed, shutting down");
            }
        }
    })
    .context("ctrlc")?;

    loop {
        pump_read(&mut sink_rec, &mut sink_in);
        pump_read(&mut src_rec, &mut src_in);

        while let Some(frame) = sink_in.drain_frames(frame_size) {
            let mut sc = StereoChunk {
                left: [0.0; CHUNK],
                right: [0.0; CHUNK],
            };
            for i in 0..CHUNK {
                sc.left[i] = frame[i * 2];
                sc.right[i] = frame[i * 2 + 1];
            }
            if let Ok(out) = sink_denoiser.process_stereo(&sc) {
                for i in 0..CHUNK {
                    sink_out.data.push(out.left[i]);
                    sink_out.data.push(out.right[i]);
                }
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
            if let Ok(out) = src_denoiser.process_stereo(&sc) {
                for i in 0..CHUNK {
                    src_out.data.push(out.left[i]);
                    src_out.data.push(out.right[i]);
                }
            }
        }

        pump_write(&mut sink_play, &mut sink_out);
        pump_write(&mut src_play, &mut src_out);

        match mainloop.iterate(false) {
            IterateResult::Quit(_) | IterateResult::Err(_) => break,
            _ => {}
        }
        if sink_in.len() < frame_size
            && src_in.len() < frame_size
            && sink_out.len() == 0
            && src_out.len() == 0
        {
            std::thread::sleep(std::time::Duration::from_micros(500));
        }

        if shutting_down.load(Ordering::Relaxed) {
            mainloop.quit(libpulse_binding::def::Retval(0));
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
