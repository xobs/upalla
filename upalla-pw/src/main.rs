//! Upalla — GPU-agnostic real-time noise suppression.
//! Usage:  upalla [--passthrough]

mod pw_ffi;
mod denoiser;

use crate::denoiser::Denoiser;
use std::ffi::{c_void, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_queue::ArrayQueue;
use pw_ffi::*;

const CHUNK: usize = 480;
const QCAP: usize = 64;

static PW_API: OnceLock<PwApi> = OnceLock::new();
static mut QUIT_LOOP: *mut c_void = ptr::null_mut();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn find_model_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(&home).join(".local/share/upalla");
    if has_model(&dir) { return Some(dir); }
    for p in &["/usr/local/share/upalla", "/usr/share/upalla"] {
        let d = PathBuf::from(p);
        if has_model(&d) { return Some(d); }
    }
    None
}
fn has_model(dir: &PathBuf) -> bool {
    dir.join("enc.onnx").exists() && dir.join("erb_dec.onnx").exists() && dir.join("df_dec.onnx").exists()
}
fn to_c(s: &str) -> CString { CString::new(s).unwrap() }

// Worker thread: runs denoiser off the RT audio thread.
// Uses polling (thread::sleep) so the RT callback never does syscalls.
struct Worker {
    in_q: Arc<ArrayQueue<[f32; CHUNK]>>,
    out_q: Arc<ArrayQueue<[f32; CHUNK]>>,
    done: Arc<AtomicBool>,
    reset_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    fn new(model_dir: &PathBuf) -> Self {
        let model_dir = model_dir.clone();
        let in_q: Arc<ArrayQueue<[f32; CHUNK]>> = Arc::new(ArrayQueue::new(QCAP));
        let out_q: Arc<ArrayQueue<[f32; CHUNK]>> = Arc::new(ArrayQueue::new(QCAP));
        let done = Arc::new(AtomicBool::new(false));
        let reset_flag = Arc::new(AtomicBool::new(false));
        let iq = in_q.clone(); let oq = out_q.clone();
        let d = done.clone(); let r = reset_flag.clone();
        let thread = thread::Builder::new().name("upalla-worker".into()).spawn(move || {
            let mut denoiser = match Denoiser::new(&model_dir) {
                Ok(d) => d,
                Err(e) => { log::error!("Failed to create denoiser: {e}"); return; }
            };
            let mut out_buf = [0.0f32; CHUNK];
            while !d.load(Ordering::Relaxed) {
                if r.swap(false, Ordering::Relaxed) {
                    denoiser.reset();
                    while iq.pop().is_some() {}
                    while oq.pop().is_some() {}
                }
                match iq.pop() {
                    Some(input) => {
                        match denoiser.process(&input, &mut out_buf) {
                            Ok(n) => {
                                if n > 0 {
                                    let _ = oq.push(out_buf);
                                    static COUNT: AtomicU64 = AtomicU64::new(0);
                                    let c = COUNT.fetch_add(1, Ordering::Relaxed);
                                    if c < 3 {
                                        let rms: f32 = (out_buf.iter().map(|&s| s*s).sum::<f32>() / n as f32).sqrt();
                                        let in_rms: f32 = (input.iter().map(|&s| s*s).sum::<f32>() / input.len() as f32).sqrt();
                                        log::info!("chunk #{c}: rms_in={in_rms:.4} rms_out={rms:.4}");
                                    }
                                }
                            }
                            Err(e) => log::error!("denoiser: {e}"),
                        }
                    }
                    None => thread::sleep(Duration::from_millis(1)),
                }
            }
        }).expect("spawn");
        Worker { in_q, out_q, done, reset_flag, thread: Some(thread) }
    }
    fn send(&self, chunk: [f32; CHUNK]) { let _ = self.in_q.push(chunk); }
    fn recv(&self) -> Option<[f32; CHUNK]> { self.out_q.pop() }
    fn reset(&self) { self.reset_flag.store(true, Ordering::Relaxed); }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() { let _ = h.join(); }
    }
}

struct AppData {
    worker: Option<Worker>,
    in_port: *mut c_void,
    out_port: *mut c_void,
    api: &'static PwApi,
    remainder_in: [f32; CHUNK],
    remainder_in_len: usize,
    remainder_out: [f32; 16384],
    remainder_out_len: usize,
    passthrough: bool,
    needs_reset: bool,
}

unsafe extern "C" fn on_state_changed(
    userdata: *mut c_void, _old: i32, state: i32, _err: *const std::ffi::c_char,
) {
    if state == 2 {
        let d = &mut *(userdata as *mut AppData);
        if let Some(ref w) = d.worker { w.reset(); }
        d.needs_reset = true;
    }
}

unsafe extern "C" fn on_process(userdata: *mut c_void, position: *mut spa_io_position) {
    if SHUTTING_DOWN.load(Ordering::Relaxed) { return; }
    let d = &mut *(userdata as *mut AppData);
    let n = (*position).clock.duration as usize;
    let p_in = call_pw_filter_get_dsp_buffer(d.api, d.in_port, n as u32);
    let p_out = call_pw_filter_get_dsp_buffer(d.api, d.out_port, n as u32);
    if p_in.is_null() || p_out.is_null() { return; }

    let input = std::slice::from_raw_parts(p_in, n);
    let output = std::slice::from_raw_parts_mut(p_out, n);
    output.fill(0.0);

    if d.passthrough || d.worker.is_none() {
        output.copy_from_slice(input);
        return;
    }

    if d.needs_reset {
        d.remainder_in_len = 0;
        d.remainder_out_len = 0;
        d.needs_reset = false;
    }

    // Drain worker output
    if let Some(ref w) = d.worker {
        while d.remainder_out_len + CHUNK <= d.remainder_out.len() {
            match w.recv() {
                Some(chunk) => {
                    let start = d.remainder_out_len;
                    d.remainder_out[start..start + CHUNK].copy_from_slice(&chunk);
                    d.remainder_out_len += CHUNK;
                }
                None => break,
            }
        }
    }

    // Feed input to worker
    let mut pos = 0;
    while pos < n || d.remainder_in_len >= CHUNK {
        while d.remainder_in_len < CHUNK && pos < n {
            d.remainder_in[d.remainder_in_len] = input[pos];
            d.remainder_in_len += 1; pos += 1;
        }
        if d.remainder_in_len < CHUNK { break; }
        if d.remainder_in_len >= CHUNK {
            if let Some(ref w) = d.worker { w.send(d.remainder_in); }
            d.remainder_in.copy_within(CHUNK.., 0);
            d.remainder_in_len -= CHUNK;
        }
    }

    // Write output
    let copy = d.remainder_out_len.min(n);
    output[..copy].copy_from_slice(&d.remainder_out[..copy]);
    if copy < d.remainder_out_len {
        d.remainder_out.copy_within(copy..d.remainder_out_len, 0);
    }
    d.remainder_out_len -= copy;
}

fn main() -> Result<()> {
    env_logger::init();
    let passthrough = std::env::args().any(|a| a == "--passthrough");

    let api = PwApi::load().context("libpipewire-0.3")?;

    let worker = if passthrough {
        None
    } else {
        let model_dir = find_model_dir()
            .context("ONNX model not found. Run: ./scripts/download-model.sh\nOr: upalla --passthrough")?;
        log::info!("Loading model from {:?}", model_dir);
        Some(Worker::new(&model_dir))
    };

    PW_API.set(api).map_err(|_| anyhow::anyhow!("API"))?;
    let api_ref = PW_API.get().unwrap();

    unsafe {
        call_pw_init(api_ref, ptr::null_mut(), ptr::null_mut());
        let loop_ = call_pw_main_loop_new(api_ref, ptr::null());
        if loop_.is_null() { anyhow::bail!("loop"); }
        let pw_loop = call_pw_main_loop_get_loop(api_ref, loop_);

        let props = call_pw_properties_new_string(api_ref, to_c(
            r#"{"media.type":"Audio","media.category":"Source","media.class":"Audio/Source","media.role":"DSP","node.name":"upalla_source","node.description":"Upalla Denoiser"}"#).as_ptr());
        if props.is_null() { anyhow::bail!("props"); }

        let mut app = Box::new(AppData {
            worker,
            in_port: ptr::null_mut(), out_port: ptr::null_mut(), api: api_ref,
            remainder_in: [0.0f32; CHUNK], remainder_in_len: 0,
            remainder_out: [0.0f32; 16384], remainder_out_len: 0,
            passthrough, needs_reset: false,
        });

        let events = Box::new(PwFilterEvents {
            version: 2,
            destroy: None,
            state_changed: Some(on_state_changed),
            io_changed: None, param_changed: None,
            add_buffer: None, remove_buffer: None,
            process: Some(on_process), drained: None, command: None,
        });
        let events_ptr = Box::into_raw(events);

        let filter = call_pw_filter_new_simple(api_ref, pw_loop,
            to_c("upalla").as_ptr(), props,
            events_ptr as *const PwFilterEvents,
            app.as_mut() as *mut _ as *mut c_void);
        if filter.is_null() { anyhow::bail!("filter"); }

        let in_props = call_pw_properties_new_string(api_ref,
            to_c(r#"{"format.dsp":"32 bit float mono audio","port.name":"input"}"#).as_ptr());
        let out_props = call_pw_properties_new_string(api_ref,
            to_c(r#"{"format.dsp":"32 bit float mono audio","port.name":"output"}"#).as_ptr());

        app.in_port = call_pw_filter_add_port(api_ref, filter, PW_DIRECTION_INPUT,
            PW_FILTER_PORT_FLAG_MAP_BUFFERS, 4, in_props, ptr::null(), 0);
        app.out_port = call_pw_filter_add_port(api_ref, filter, PW_DIRECTION_OUTPUT,
            PW_FILTER_PORT_FLAG_MAP_BUFFERS, 4, out_props, ptr::null(), 0);

        let fmt = upalla_build_format_params();
        if fmt.is_null() { anyhow::bail!("fmt"); }
        if call_pw_filter_connect(api_ref, filter, PW_FILTER_FLAG_RT_PROCESS,
            (*fmt).params.as_ptr() as *const c_void, (*fmt).n_params) < 0 {
            upalla_free_format_params(fmt); anyhow::bail!("connect");
        }
        upalla_free_format_params(fmt);

        QUIT_LOOP = loop_;
        let h = api_ref;
        ctrlc::set_handler(move || {
            if !SHUTTING_DOWN.swap(true, Ordering::SeqCst) {
                unsafe { if !QUIT_LOOP.is_null() { call_pw_main_loop_quit(h, QUIT_LOOP); } }
            }
        }).context("ctrlc")?;

        if passthrough {
            log::info!("Upalla pass-through running.");
        } else {
            log::info!("Upalla Denoiser running. Route mic→input, output→destination.");
        }
        call_pw_main_loop_run(api_ref, loop_);

        SHUTTING_DOWN.store(true, Ordering::SeqCst);
        call_pw_filter_destroy(api_ref, filter);
        drop(app);
        let _ = Box::from_raw(events_ptr);
        call_pw_main_loop_destroy(api_ref, loop_);
        call_pw_deinit(api_ref);
    }
    Ok(())
}
