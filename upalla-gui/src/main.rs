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
    SetEnabled(bool),
    Quit,
}

struct UpallaApp {
    pa: PaFilter,
    gui_state: app::AppGuiState,
    status_rx: Receiver<upalla_pa::Status>,
    tray_done: Arc<AtomicBool>,
}

impl UpallaApp {
    fn new(
        pa: PaFilter,
        status_rx: Receiver<upalla_pa::Status>,
        tray_done: Arc<AtomicBool>,
        tray_ids: Option<tray::TrayMenuIds>,
    ) -> Self {
        UpallaApp {
            pa,
            gui_state: app::AppGuiState::new(tray_ids),
            status_rx,
            tray_done,
        }
    }
}

impl eframe::App for UpallaApp {
    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        log::debug!("eframe::App::ui()");
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        log::debug!("eframe::App::update()");
        // Hide window on first frame — go to tray
        if self.gui_state.start_hidden() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            ctx.request_repaint();
            return;
        }

        // Poll tray menu events
        if let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            self.gui_state.handle_tray_event(event);
        }

        // Poll PA status
        while let Ok(status) = self.status_rx.try_recv() {
            self.gui_state.rms_in = status.rms_in;
            self.gui_state.rms_out = status.rms_out;
        }

        // Render GUI
        let actions = app::render_gui(ctx, &mut self.gui_state);

        // Handle tray actions
        for action in actions {
            log::debug!("Tray Action: {action:?}");
            match action {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::new(
                        400.0, 320.0,
                    )));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                }
                TrayAction::Hide => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                TrayAction::SetEnabled(enabled) => {
                    self.pa.set_bypass(!enabled);
                }
                TrayAction::Quit => {
                    self.tray_done.store(true, Ordering::Relaxed);
                    std::process::exit(0);
                }
            }
        }

        // Sync bypass state
        self.pa.set_bypass(self.gui_state.bypass);

        // Throttle when minimized
        if ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Starting Upalla GUI...");

    let pa = PaFilter::new(Model::default())?;
    let status_rx = pa.status_receiver().clone();

    let tray_done = Arc::new(AtomicBool::new(false));
    let (ids_tx, ids_rx) = crossbeam_channel::bounded(1);

    let _tray_handle = {
        let done = tray_done.clone();
        thread::Builder::new()
            .name("upalla-tray".into())
            .spawn(move || tray::run_tray(done, ids_tx))
            .expect("spawn tray")
    };

    let tray_ids = ids_rx.recv_timeout(std::time::Duration::from_secs(3)).ok();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 320.0])
            .with_title("Upalla"),
        ..Default::default()
    };

    eframe::run_native(
        "Upalla",
        options,
        Box::new(move |_cc| Ok(Box::new(UpallaApp::new(pa, status_rx, tray_done, tray_ids)))),
    )?;

    Ok(())
}
