use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::Result;
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
    pa: PaFilter,
    gui_state: app::AppGuiState,
    status_rx: Receiver<upalla_pa::Status>,
    tray_done: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    prev_sink: String,
    prev_source: String,
}

impl UpallaApp {
    fn new(
        pa: PaFilter,
        status_rx: Receiver<upalla_pa::Status>,
        tray_done: Arc<AtomicBool>,
        tray_ids: Option<tray::TrayMenuIds>,
        enabled: Arc<AtomicBool>,
    ) -> Self {
        UpallaApp {
            pa,
            gui_state: app::AppGuiState::new(tray_ids),
            status_rx,
            tray_done,
            enabled,
            prev_sink: String::new(),
            prev_source: String::new(),
        }
    }
}

impl eframe::App for UpallaApp {
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.gui_state.start_hidden() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        // Sync enabled state into the GUI so the checkbox is correct
        self.gui_state.bypass = !self.enabled.load(Ordering::Relaxed);

        // Poll tray menu events
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            self.gui_state.handle_tray_event(event);
        }

        // Poll PA status
        while let Ok(status) = self.status_rx.try_recv() {
            self.gui_state.rms_in = status.rms_in;
            self.gui_state.rms_out = status.rms_out;
        }

        // Device refresh
        if self.gui_state.refresh_devices {
            self.gui_state.refresh_devices = false;
            self.gui_state
                .set_devices(self.pa.enumerate_devices());
        }

        // Process tray actions BEFORE rendering so the GUI reflects them
        for action in self.gui_state.drain_pending_actions() {
            match action {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                        egui::Vec2::new(400.0, 320.0),
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                }
                TrayAction::Hide => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                TrayAction::ToggleEnabled => {
                    let was = self.enabled.load(Ordering::Relaxed);
                    self.enabled.store(!was, Ordering::Relaxed);
                    self.gui_state.bypass = was;
                    self.pa.set_bypass(was);
                }
                TrayAction::Quit => {
                    self.tray_done.store(true, Ordering::Relaxed);
                    std::process::exit(0);
                }
            }
        }

        // Render GUI (now with up-to-date enabled/bypass state)
        app::render_gui(ctx, &mut self.gui_state);

        // Handle device changes
        if self.gui_state.selected_sink != self.prev_sink {
            self.prev_sink = self.gui_state.selected_sink.clone();
            self.pa.set_sink(self.gui_state.selected_sink.clone());
        }
        if self.gui_state.selected_source != self.prev_source {
            self.prev_source = self.gui_state.selected_source.clone();
            self.pa
                .set_source(self.gui_state.selected_source.clone());
        }

        // Sync GUI checkbox back to enabled, then to PA
        let gui_enabled = !self.gui_state.bypass;
        if gui_enabled != self.enabled.load(Ordering::Relaxed) {
            self.enabled.store(gui_enabled, Ordering::Relaxed);
        }
        self.pa.set_bypass(self.gui_state.bypass);

        // Always request repaints so tray events are polled even when minimized
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Starting Upalla GUI...");

    let pa = PaFilter::new(Model::default())?;
    let status_rx = pa.status_receiver().clone();

    let tray_done = Arc::new(AtomicBool::new(false));
    let enabled = Arc::new(AtomicBool::new(true));
    let (ids_tx, ids_rx) = crossbeam_channel::bounded(1);

    let _tray_handle = {
        let done = tray_done.clone();
        let en = enabled.clone();
        thread::Builder::new()
            .name("upalla-tray".into())
            .spawn(move || tray::run_tray(done, ids_tx, en))
            .expect("spawn tray")
    };

    let tray_ids = ids_rx.recv_timeout(std::time::Duration::from_secs(3)).ok();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 360.0])
            .with_title("Upalla"),
        ..Default::default()
    };

    eframe::run_native(
        "Upalla",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(UpallaApp::new(
                pa,
                status_rx,
                tray_done,
                tray_ids,
                enabled,
            )))
        }),
    )?;

    Ok(())
}
