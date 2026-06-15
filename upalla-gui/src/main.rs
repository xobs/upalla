use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use upalla_core::model::Model;
use upalla_pa::PaFilter;

mod app;
mod tray;

#[derive(Clone, Copy, Debug)]
pub enum TrayAction {
    Show,
    Hide,
    ToggleEnabled,
    Quit,
}

struct UpallaApp {
    pa: Arc<PaFilter>,
    gui_state: app::AppGuiState,
    control: Receiver<Control>,
    previous_bypass: bool,
    status_rx: Receiver<upalla_pa::Status>,
    previous_sink: String,
    previous_source: String,
    last_device_refresh: Instant,
    #[cfg(target_os = "macos")]
    enabled_check: Arc<tray_icon::menu::CheckMenuItem>,
}

enum Control {
    /// Open a new window
    Open,
    /// Quit the program
    Quit,
}

impl UpallaApp {
    fn new(
        pa: Arc<PaFilter>,
        status_rx: Receiver<upalla_pa::Status>,
        control: Receiver<Control>,
        #[cfg(target_os = "macos")] enabled_check: Arc<tray_icon::menu::CheckMenuItem>,
    ) -> Self {
        let previous_bypass = pa.bypass();
        UpallaApp {
            pa,
            gui_state: app::AppGuiState::new(),
            status_rx,
            control,
            previous_bypass,
            previous_sink: String::new(),
            previous_source: String::new(),
            last_device_refresh: Instant::now(),
            #[cfg(target_os = "macos")]
            enabled_check,
        }
    }
}

impl eframe::App for UpallaApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(status) = self.status_rx.try_recv() {
            self.gui_state.playback_in = status.playback_in;
            self.gui_state.playback_out = status.playback_out;
            self.gui_state.recording_in = status.recording_in;
            self.gui_state.recording_out = status.recording_out;
        }

        if let Ok(Control::Quit) = self.control.try_recv() {
            log::debug!("Quit message received in logic -- closing viewport");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if self.last_device_refresh.elapsed() >= std::time::Duration::from_secs(1) {
            self.gui_state.refresh_devices = true;
            self.last_device_refresh = Instant::now();
        }

        if self.gui_state.refresh_devices {
            self.gui_state.refresh_devices = false;
            self.gui_state.set_devices(self.pa.enumerate_devices());
        }

        if self.gui_state.selected_sink != self.previous_sink {
            self.previous_sink = self.gui_state.selected_sink.clone();
            self.pa.set_sink(self.gui_state.selected_sink.clone());
        }
        if self.gui_state.selected_source != self.previous_source {
            self.previous_source = self.gui_state.selected_source.clone();
            self.pa.set_source(self.gui_state.selected_source.clone());
        }

        // Two-way sync: GUI checkbox <-> PA bypass <-> tray checkmark
        if self.gui_state.bypass != self.previous_bypass {
            log::trace!(
                "GUI bypass changed -- new state is {}",
                self.gui_state.bypass
            );
            self.pa.set_bypass(self.gui_state.bypass);
        } else if self.gui_state.bypass != self.pa.bypass() {
            self.gui_state.bypass = self.pa.bypass();
            log::trace!(
                "PA bypass changed -- new state is {}",
                self.gui_state.bypass
            );
        }
        self.previous_bypass = self.gui_state.bypass;

        // On macOS the tray checkmark must be updated from the main thread.
        #[cfg(target_os = "macos")]
        {
            let enabled = !self.gui_state.bypass;
            self.enabled_check.set_checked(enabled);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx();
        app::render_gui(ctx, &mut self.gui_state);
    }
}

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Starting Upalla GUI...");

    let enabled = Arc::new(AtomicBool::new(true));
    let pa = Arc::new(PaFilter::new(Model::default(), Arc::clone(&enabled))?);
    let status_rx = pa.status_receiver().clone();

    // ---------- Tray setup ----------
    // On Linux: spawn a GTK thread that runs the tray event loop.
    // On macOS: create the tray on the main thread (NSMenu requirement).
    #[cfg(target_os = "linux")]
    let (tray_ids, _tray_done) = {
        let tray_done = Arc::new(AtomicBool::new(false));
        let (ids_tx, ids_rx) = crossbeam_channel::bounded(1);

        let _handle = thread::Builder::new()
            .name("upalla-tray".into())
            .spawn({
                let tray_done = tray_done.clone();
                let enabled = Arc::clone(&enabled);
                move || tray::run_tray(tray_done, ids_tx, enabled)
            })
            .expect("spawn tray");

        let tray_ids = ids_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .context("Unable to get tray IDs")?;

        (tray_ids, tray_done)
    };

    #[cfg(target_os = "macos")]
    let (tray_ids, enabled_check) = tray::create_tray(&enabled);

    // ---------- eframe options ----------
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 360.0])
            .with_title("Upalla"),
        ..Default::default()
    };

    // ---------- Window / control channels ----------
    let window_open = Arc::new(AtomicBool::new(false));
    let (window_tx, window_rx) = crossbeam_channel::bounded(1);
    let (control_tx, control_rx) = crossbeam_channel::bounded(1);

    ctrlc::set_handler({
        let shutting_down = Arc::new(AtomicBool::new(false));
        let control_tx = control_tx.clone();
        let window_tx = window_tx.clone();
        move || {
            if !shutting_down.swap(true, Ordering::SeqCst) {
                println!("Ctrl-C pressed, shutting down");
                let _ = control_tx.try_send(Control::Quit);
                let _ = window_tx.try_send(Control::Quit);
            }
        }
    })
    .context("ctrlc")?;

    // ---------- Tray menu event polling ----------
    std::thread::spawn({
        let pa = Arc::clone(&pa);
        let window_open = Arc::clone(&window_open);
        move || {
            while let Ok(event) = tray_icon::menu::MenuEvent::receiver().recv() {
                if event.id == tray_ids.show_hide && window_tx.is_empty() {
                    if !window_open.load(Ordering::Relaxed) {
                        let _ = window_tx.send(Control::Open);
                    }
                } else if event.id == tray_ids.enabled {
                    pa.set_bypass(!pa.bypass());
                } else if event.id == tray_ids.quit {
                    let _ = control_tx.send(Control::Quit);
                    break;
                } else {
                    log::debug!("Unrecognized event: {event:?}");
                }
            }
            log::debug!("Tray thread exited");
        }
    });

    // ---------- Window event loop ----------
    while let Ok(Control::Open) = window_rx.recv() {
        window_open.store(true, Ordering::Relaxed);
        eframe::run_native(
            "Upalla",
            options.clone(),
            Box::new({
                let pa = pa.clone();
                let status_rx = status_rx.clone();
                let control_rx = control_rx.clone();
                #[cfg(target_os = "macos")]
                let enabled_check = enabled_check.clone();
                move |_cc| {
                    Ok(Box::new(UpallaApp::new(
                        pa,
                        status_rx,
                        control_rx,
                        #[cfg(target_os = "macos")]
                        enabled_check,
                    )))
                }
            }),
        )?;
        window_open.store(false, Ordering::Relaxed);
    }

    log::debug!("eframe exited -- quitting");
    pa.shutdown();

    Ok(())
}
