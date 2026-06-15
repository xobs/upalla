// Runtime dynamic loading of libpipewire-0.3 via dlopen.
// No dev headers or compile-time linking required.

use std::ffi::{c_char, c_int, c_void};

use anyhow::{Context, Result};
use libloading::Library;

pub const PW_DIRECTION_INPUT: u32 = 0;
pub const PW_DIRECTION_OUTPUT: u32 = 1;
pub const PW_FILTER_PORT_FLAG_MAP_BUFFERS: u32 = 1;
pub const PW_FILTER_FLAG_RT_PROCESS: u32 = 4;

#[repr(C)]
pub struct spa_io_position {
    pub clock: spa_io_clock,
    pub _pad: [u8; 1528],
}

#[repr(C)]
pub struct spa_io_clock {
    pub flags: u32,
    pub id: u32,
    pub name: [u8; 64],
    pub nsec: u64,
    pub rate: spa_fraction,
    pub position: u64,
    pub duration: u64,
    pub delay: u64,
    pub rate_diff: f64,
    pub next_nsec: u64,
    pub _pad: [u64; 4],
}

#[repr(C)]
pub struct spa_fraction {
    pub num: u32,
    pub denom: u32,
}

#[repr(C)]
pub struct PwFilterEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, i32, i32, *const c_char)>,
    pub io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32>,
    pub process: Option<unsafe extern "C" fn(*mut c_void, *mut spa_io_position)>,
    pub drained: Option<unsafe extern "C" fn(*mut c_void)>,
    pub command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
}

macro_rules! fp {
    ($lib:expr, $name:literal, $($t:tt)*) => {{
        let sym: libloading::Symbol<unsafe extern "C" fn($($t)*)> = unsafe {
            $lib.get($name.as_bytes()).with_context(|| format!("symbol not found: {}", $name))?
        };
        (*sym as *const ()) as usize
    }};
}

macro_rules! fpret {
    ($lib:expr, $name:literal, ($($args:tt)*) -> $ret:ty) => {{
        let sym: libloading::Symbol<unsafe extern "C" fn($($args)*) -> $ret> = unsafe {
            $lib.get($name.as_bytes()).with_context(|| format!("symbol not found: {}", $name))?
        };
        (*sym as *const ()) as usize
    }};
}

pub struct PwApi {
    #[allow(dead_code)]
    lib: Library,
    pub pw_init: usize,
    pub pw_deinit: usize,
    pub pw_main_loop_new: usize,
    pub pw_main_loop_get_loop: usize,
    pub pw_main_loop_run: usize,
    pub pw_main_loop_quit: usize,
    pub pw_main_loop_destroy: usize,
    pub pw_filter_new_simple: usize,
    pub pw_filter_add_port: usize,
    pub pw_filter_connect: usize,
    pub pw_filter_get_dsp_buffer: usize,
    pub pw_filter_destroy: usize,
    pub pw_properties_new_string: usize,
}

// C helper compiled from pw_format.c
extern "C" {
    pub fn upalla_build_format_params() -> *mut PwFormatParams;
    pub fn upalla_free_format_params(p: *mut PwFormatParams);
}

#[repr(C)]
pub struct PwFormatParams {
    pub buffer: [u8; 4096],
    pub params: [*const c_void; 2],
    pub n_params: u32,
}

#[allow(dead_code)]
pub unsafe fn call_pw_init(api: &PwApi, argc: *mut c_int, argv: *mut *mut c_char) {
    let f: unsafe extern "C" fn(*mut c_int, *mut *mut c_char) = std::mem::transmute(api.pw_init);
    f(argc, argv);
}

pub unsafe fn call_pw_deinit(api: &PwApi) {
    let f: unsafe extern "C" fn() = std::mem::transmute(api.pw_deinit);
    f();
}

pub unsafe fn call_pw_main_loop_new(api: &PwApi, props: *const c_void) -> *mut c_void {
    let f: unsafe extern "C" fn(*const c_void) -> *mut c_void =
        std::mem::transmute(api.pw_main_loop_new);
    f(props)
}

pub unsafe fn call_pw_main_loop_get_loop(api: &PwApi, l: *mut c_void) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void) -> *mut c_void =
        std::mem::transmute(api.pw_main_loop_get_loop);
    f(l)
}

pub unsafe fn call_pw_main_loop_run(api: &PwApi, l: *mut c_void) -> c_int {
    let f: unsafe extern "C" fn(*mut c_void) -> c_int = std::mem::transmute(api.pw_main_loop_run);
    f(l)
}

pub unsafe fn call_pw_main_loop_quit(api: &PwApi, l: *mut c_void) {
    let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(api.pw_main_loop_quit);
    f(l);
}

pub unsafe fn call_pw_main_loop_destroy(api: &PwApi, l: *mut c_void) {
    let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(api.pw_main_loop_destroy);
    f(l);
}

pub unsafe fn call_pw_properties_new_string(api: &PwApi, str_: *const c_char) -> *mut c_void {
    let f: unsafe extern "C" fn(*const c_char) -> *mut c_void =
        std::mem::transmute(api.pw_properties_new_string);
    f(str_)
}

pub unsafe fn call_pw_filter_new_simple(
    api: &PwApi,
    l: *mut c_void,
    name: *const c_char,
    props: *mut c_void,
    events: *const PwFilterEvents,
    user_data: *mut c_void,
) -> *mut c_void {
    let f: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *mut c_void,
        *const PwFilterEvents,
        *mut c_void,
    ) -> *mut c_void = std::mem::transmute(api.pw_filter_new_simple);
    f(l, name, props, events, user_data)
}

pub unsafe fn call_pw_filter_add_port(
    api: &PwApi,
    filter: *mut c_void,
    direction: u32,
    flags: u32,
    port_data_size: usize,
    port_props: *mut c_void,
    params: *const c_void,
    n_params: u32,
) -> *mut c_void {
    let f: unsafe extern "C" fn(
        *mut c_void,
        u32,
        u32,
        usize,
        *mut c_void,
        *const c_void,
        u32,
    ) -> *mut c_void = std::mem::transmute(api.pw_filter_add_port);
    f(
        filter,
        direction,
        flags,
        port_data_size,
        port_props,
        params,
        n_params,
    )
}

pub unsafe fn call_pw_filter_connect(
    api: &PwApi,
    filter: *mut c_void,
    flags: u32,
    params: *const c_void,
    n_params: u32,
) -> c_int {
    let f: unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32) -> c_int =
        std::mem::transmute(api.pw_filter_connect);
    f(filter, flags, params, n_params)
}

pub unsafe fn call_pw_filter_get_dsp_buffer(
    api: &PwApi,
    port: *mut c_void,
    n_samples: u32,
) -> *mut f32 {
    let f: unsafe extern "C" fn(*mut c_void, u32) -> *mut f32 =
        std::mem::transmute(api.pw_filter_get_dsp_buffer);
    f(port, n_samples)
}

pub unsafe fn call_pw_filter_destroy(api: &PwApi, filter: *mut c_void) {
    let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(api.pw_filter_destroy);
    f(filter);
}

impl PwApi {
    pub fn load() -> Result<Self> {
        let lib_path = std::env::var("LIBPIPEWIRE_PATH")
            .unwrap_or_else(|_| "libpipewire-0.3.so.0".to_string());

        let lib = unsafe { Library::new(&lib_path) }
            .with_context(|| format!("failed to load {}", lib_path))?;

        Ok(PwApi {
            pw_init: fp!(lib, "pw_init", *mut c_int, *mut *mut c_char),
            pw_deinit: fp!(lib, "pw_deinit",),
            pw_main_loop_new: fpret!(lib, "pw_main_loop_new", (*const c_void) -> *mut c_void),
            pw_main_loop_get_loop: fpret!(lib, "pw_main_loop_get_loop", (*mut c_void) -> *mut c_void),
            pw_main_loop_run: fpret!(lib, "pw_main_loop_run", (*mut c_void) -> c_int),
            pw_main_loop_quit: fp!(lib, "pw_main_loop_quit", *mut c_void),
            pw_main_loop_destroy: fp!(lib, "pw_main_loop_destroy", *mut c_void),
            pw_filter_new_simple: fpret!(lib, "pw_filter_new_simple", (*mut c_void, *const c_char, *mut c_void, *const PwFilterEvents, *mut c_void) -> *mut c_void),
            pw_filter_add_port: fpret!(lib, "pw_filter_add_port", (*mut c_void, u32, u32, usize, *mut c_void, *const c_void, u32) -> *mut c_void),
            pw_filter_connect: fpret!(lib, "pw_filter_connect", (*mut c_void, u32, *const c_void, u32) -> c_int),
            pw_filter_get_dsp_buffer: fpret!(lib, "pw_filter_get_dsp_buffer", (*mut c_void, u32) -> *mut f32),
            pw_filter_destroy: fp!(lib, "pw_filter_destroy", *mut c_void),
            pw_properties_new_string: fpret!(lib, "pw_properties_new_string", (*const c_char) -> *mut c_void),
            lib,
        })
    }
}

unsafe impl Send for PwApi {}
unsafe impl Sync for PwApi {}
