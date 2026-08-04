use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use ksni::blocking::TrayMethods;
use slint::{ComponentHandle, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use upalla_core::model::Model;
use upalla_pa::PaFilter;

mod icon;

slint::include_modules!();

fn db_val(sample: f32) -> f32 {
    if sample > 0.0 {
        20.0 * sample.log10()
    } else {
        -60.0
    }
}

struct SharedState {
    pa: Arc<PaFilter>,
    status_rx: Receiver<upalla_pa::Status>,
    previous_sink_idx: i32,
    previous_source_idx: i32,
    previous_pb_bypass: bool,
    previous_rec_bypass: bool,
    pb_enabled: Arc<AtomicBool>,
    rec_enabled: Arc<AtomicBool>,
    last_recording_detected: bool,
    sink_descriptions: Vec<String>,
    source_descriptions: Vec<String>,
    window: Option<UpallaWindow>,
}

fn update_levels(state: &mut SharedState) {
    while let Ok(status) = state.status_rx.try_recv() {
        if let Some(ref win) = state.window {
            let pb_in = db_val(status.playback_in);
            let pb_out = db_val(status.playback_out);
            let rec_in = db_val(status.recording_in);
            let rec_out = db_val(status.recording_out);
            win.set_playback_in_level(((pb_in + 60.0) / 60.0).clamp(0.0, 1.0));
            win.set_playback_in_db(format!("{:.1} dB", pb_in.clamp(-60.0, 0.0)).into());
            win.set_playback_out_level(((pb_out + 60.0) / 60.0).clamp(0.0, 1.0));
            win.set_playback_out_db(format!("{:.1} dB", pb_out.clamp(-60.0, 0.0)).into());
            win.set_recording_in_level(((rec_in + 60.0) / 60.0).clamp(0.0, 1.0));
            win.set_recording_in_db(format!("{:.1} dB", rec_in.clamp(-60.0, 0.0)).into());
            win.set_recording_out_level(((rec_out + 60.0) / 60.0).clamp(0.0, 1.0));
            win.set_recording_out_db(format!("{:.1} dB", rec_out.clamp(-60.0, 0.0)).into());
            state.last_recording_detected = status.recording_detected;
        }
    }

    if let Some(ref win) = state.window {
        let sink_idx = win.get_sink_index();
        if sink_idx != state.previous_sink_idx {
            state.previous_sink_idx = sink_idx;
            if sink_idx > 0 && (sink_idx as usize) < state.sink_descriptions.len() {
                state
                    .pa
                    .set_sink(state.sink_descriptions[sink_idx as usize].clone());
            }
        }

        let source_idx = win.get_source_index();
        if source_idx != state.previous_source_idx {
            state.previous_source_idx = source_idx;
            if source_idx > 0 && (source_idx as usize) < state.source_descriptions.len() {
                state
                    .pa
                    .set_source(state.source_descriptions[source_idx as usize].clone());
            }
        }

        // Playback checkbox → playback bypass
        let pb_enabled = win.get_playback_enabled();
        if pb_enabled != !state.previous_pb_bypass {
            state.previous_pb_bypass = !pb_enabled;
            state.pa.set_playback_bypass(!pb_enabled);
            state.pb_enabled.store(pb_enabled, Ordering::Relaxed);
        } else if !pb_enabled != state.pa.playback_bypass() {
            let bypass = state.pa.playback_bypass();
            win.set_playback_enabled(!bypass);
            state.pb_enabled.store(!bypass, Ordering::Relaxed);
            state.previous_pb_bypass = bypass;
        }

        // Recording checkbox → recording bypass
        let rec_enabled = win.get_recording_enabled();
        if rec_enabled != !state.previous_rec_bypass {
            state.previous_rec_bypass = !rec_enabled;
            state.pa.set_recording_bypass(!rec_enabled);
            state.rec_enabled.store(rec_enabled, Ordering::Relaxed);
        } else if !rec_enabled != state.pa.recording_bypass() {
            let bypass = state.pa.recording_bypass();
            win.set_recording_enabled(!bypass);
            state.rec_enabled.store(!bypass, Ordering::Relaxed);
            state.previous_rec_bypass = bypass;
        }

        // Recording "Active" checkbox: reflects capture state; user click overrides.
        let active = win.get_recording_active();
        match state.pa.src_override_mode() {
            0 => {
                // Auto: follow the filter's detected capture state.
                win.set_recording_active(state.last_recording_detected);
            }
            1 | 2 if active == state.last_recording_detected => {
                // Forced: user clicked back to the auto state → release override.
                state.pa.set_src_override(None);
            }
            _ => {}
        }
    }
}

fn show_or_create(state: &mut SharedState) {
    match state.window {
        Some(ref win) => {
            let _ = win.show();
        }
        None => {
            let win = UpallaWindow::new().expect("create window");
            win.set_playback_enabled(true);
            win.set_recording_enabled(true);

            // Close → destroy
            win.window()
                .on_close_requested(move || slint::CloseRequestResponse::HideWindow);

            // Refresh button
            win.on_refresh({
                let pa = state.pa.clone();
                let weak = win.as_weak();
                move || {
                    if let Some(w) = weak.upgrade() {
                        populate_window(&pa, &w);
                    }
                }
            });

            // Recording "Active" checkbox: user click forces capture on/off.
            win.on_recording_active_toggled({
                let pa = state.pa.clone();
                move |checked| {
                    pa.set_src_override(Some(checked));
                }
            });

            populate_window(&state.pa, &win);
            let _ = win.show();
            state.window = Some(win);
        }
    }
}

fn populate_window(pa: &PaFilter, win: &UpallaWindow) {
    let devices = pa.enumerate_devices();
    let mut sinks: Vec<slint::SharedString> = Vec::new();
    let mut sources: Vec<slint::SharedString> = Vec::new();

    let default_sink_display = devices
        .sinks
        .iter()
        .find(|d| d.name == devices.default_sink)
        .map(|d| d.description.clone())
        .unwrap_or_else(|| devices.default_sink.clone());
    let default_source_display = devices
        .sources
        .iter()
        .find(|d| d.name == devices.default_source)
        .map(|d| d.description.clone())
        .unwrap_or_else(|| devices.default_source.clone());

    sinks.push(format!("Default ({})", default_sink_display).into());
    for d in &devices.sinks {
        sinks.push(d.description.clone().into());
    }
    sources.push(format!("Default ({})", default_source_display).into());
    for d in &devices.sources {
        sources.push(d.description.clone().into());
    }

    win.set_sinks(VecModel::from_slice(&sinks));
    win.set_sources(VecModel::from_slice(&sources));
    win.set_sink_index(0);
    win.set_source_index(0);
}

struct UpallaTray {
    pa: Arc<PaFilter>,
    pb_enabled: Arc<AtomicBool>,
    rec_enabled: Arc<AtomicBool>,
    show_tx: Sender<()>,
}

impl ksni::Tray for UpallaTray {
    fn id(&self) -> String {
        "upalla".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![icon::tray_icon()]
    }
    fn title(&self) -> String {
        "Upalla".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.show_tx.send(());
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Show".into(),
                enabled: true,
                activate: {
                    let tx = self.show_tx.clone();
                    Box::new(move |_: &mut Self| {
                        let _ = tx.send(());
                    })
                },
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Enabled".into(),
                checked: self.pb_enabled.load(Ordering::Relaxed)
                    && self.rec_enabled.load(Ordering::Relaxed),
                enabled: true,
                activate: {
                    let pa = self.pa.clone();
                    let pb_flag = self.pb_enabled.clone();
                    let rec_flag = self.rec_enabled.clone();
                    Box::new(move |_: &mut Self| {
                        let new =
                            !(pb_flag.load(Ordering::Relaxed) && rec_flag.load(Ordering::Relaxed));
                        pb_flag.store(new, Ordering::Relaxed);
                        rec_flag.store(new, Ordering::Relaxed);
                        pa.set_playback_bypass(!new);
                        pa.set_recording_bypass(!new);
                    })
                },
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                enabled: true,
                activate: {
                    let pa = self.pa.clone();
                    Box::new(move |_: &mut Self| {
                        pa.shutdown();
                        let _ = slint::quit_event_loop();
                    })
                },
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let pb_enabled = Arc::new(AtomicBool::new(true));
    let rec_enabled = Arc::new(AtomicBool::new(true));
    let src_override = Arc::new(AtomicU8::new(0));
    let pa = Arc::new(PaFilter::new(
        Model::default(),
        Arc::clone(&pb_enabled),
        Arc::clone(&rec_enabled),
        Arc::clone(&src_override),
    )?);
    let status_rx = pa.status_receiver().clone();
    let (show_tx, show_rx) = unbounded();

    let state = Rc::new(RefCell::new(SharedState {
        pa: pa.clone(),
        status_rx,
        previous_sink_idx: 0,
        previous_source_idx: 0,
        previous_pb_bypass: false,
        previous_rec_bypass: false,
        pb_enabled: pb_enabled.clone(),
        rec_enabled: rec_enabled.clone(),
        last_recording_detected: false,
        sink_descriptions: Vec::new(),
        source_descriptions: Vec::new(),
        window: None,
    }));

    // Timers — always running, work whether or not window is open
    let mut _timers: Vec<Timer> = Vec::new();

    // Status timer (250ms): show requests + level updates
    {
        let state = state.clone();
        let rx: Receiver<()> = show_rx;
        let t = Timer::default();
        t.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let mut s = state.borrow_mut();
            // Process show requests (create window if needed)
            while let Ok(()) = rx.try_recv() {
                show_or_create(&mut s);
            }
            update_levels(&mut s);
            // Drop window handle when hidden so next show creates a fresh one
            if let Some(ref win) = s.window {
                if !win.window().is_visible() {
                    s.window = None;
                }
            }
        });
        _timers.push(t);
    }

    // Device refresh timer (1s)
    {
        let state = state.clone();
        let t = Timer::default();
        t.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            let s = state.borrow_mut();
            if let Some(ref win) = s.window {
                populate_window(&s.pa, win);
            }
        });
        _timers.push(t);
    }

    // Tray
    UpallaTray {
        pa: pa.clone(),
        pb_enabled: pb_enabled.clone(),
        rec_enabled: rec_enabled.clone(),
        show_tx,
    }
    .spawn()
    .expect("ksni tray service");

    log::info!("entering event loop");
    slint::run_event_loop_until_quit()?;
    log::info!("exited event loop");

    Ok(())
}
