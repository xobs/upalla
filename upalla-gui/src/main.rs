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
mod icon;
mod tray;

enum Control {
    #[cfg(target_os = "linux")]
    Open,
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
    #[cfg(target_os = "macos")]
    show_requested: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    window_visible: Arc<AtomicBool>,
}

impl UpallaApp {
    fn new(
        pa: Arc<PaFilter>,
        status_rx: Receiver<upalla_pa::Status>,
        control: Receiver<Control>,
        #[cfg(target_os = "macos")] enabled_check: Arc<tray_icon::menu::CheckMenuItem>,
        #[cfg(target_os = "macos")] show_requested: Arc<AtomicBool>,
        #[cfg(target_os = "macos")] window_visible: Arc<AtomicBool>,
    ) -> Self {
        let previous_bypass = pa.bypass();
        #[cfg(target_os = "macos")]
        {
            // Hide immediately — eframe may show the window despite
            // with_visible(false). Also ensure Accessory policy persists.
            use objc2::MainThreadMarker;
            use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
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
            #[cfg(target_os = "macos")]
            show_requested,
            #[cfg(target_os = "macos")]
            window_visible,
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        #[cfg(target_os = "macos")]
        {
            // Hide window on first frame — with_visible(false) is not
            // always respected by the platform.
            if !self.window_visible.load(Ordering::Relaxed) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }

            if self.show_requested.swap(false, Ordering::SeqCst) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.window_visible.store(true, Ordering::Relaxed);
            }

            // Intercept native close — hide instead of quitting.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.window_visible.store(false, Ordering::Relaxed);
            }
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

        if self.gui_state.bypass != self.previous_bypass {
            self.pa.set_bypass(self.gui_state.bypass);
        } else if self.gui_state.bypass != self.pa.bypass() {
            self.gui_state.bypass = self.pa.bypass();
        }
        self.previous_bypass = self.gui_state.bypass;

        #[cfg(target_os = "macos")]
        {
            self.enabled_check.set_checked(!self.gui_state.bypass);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        app::render_gui(ui.ctx(), &mut self.gui_state);
    }
}

fn main() -> Result<()> {
    env_logger::init();
    log::info!("Starting Upalla GUI...");

    let enabled = Arc::new(AtomicBool::new(true));
    let pa = Arc::new(PaFilter::new(Model::default(), Arc::clone(&enabled))?);
    let status_rx = pa.status_receiver().clone();

    let (control_tx, control_rx) = crossbeam_channel::unbounded();

    ctrlc::set_handler({
        let shutting_down = Arc::new(AtomicBool::new(false));
        let control_tx = control_tx.clone();
        move || {
            if !shutting_down.swap(true, Ordering::SeqCst) {
                println!("Ctrl-C pressed, shutting down");
                let _ = control_tx.try_send(Control::Quit);
            }
        }
    })
    .context("ctrlc")?;

    // ---------- Platform-specific tray setup and event loop ----------

    #[cfg(target_os = "linux")]
    {
        let tray_done = Arc::new(AtomicBool::new(false));
        let (ids_tx, ids_rx) = crossbeam_channel::bounded(1);
        let (window_tx, window_rx) = crossbeam_channel::bounded(1);

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

        std::thread::spawn({
            let pa = Arc::clone(&pa);
            let window_open = Arc::new(AtomicBool::new(false));
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
                    }
                }
            }
        });

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([420.0, 420.0])
                .with_title("Upalla")
                .with_icon(Arc::new(icon::window_icon())),
            ..Default::default()
        };

        while let Ok(Control::Open) = window_rx.recv() {
            eframe::run_native(
                "Upalla",
                options.clone(),
                Box::new({
                    let pa = pa.clone();
                    let status_rx = status_rx.clone();
                    let control_rx = control_rx.clone();
                    move |_cc| Ok(Box::new(UpallaApp::new(pa, status_rx, control_rx)))
                }),
            )?;
        }
    }

    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

        let mtm = MainThreadMarker::new().expect("must be on main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let (tray_ids, enabled_check) = tray::create_tray(&enabled);

        let show_requested = Arc::new(AtomicBool::new(false));
        let window_visible = Arc::new(AtomicBool::new(false));

        // Menu events → background thread.
        std::thread::spawn({
            let pa = Arc::clone(&pa);
            let show_requested = Arc::clone(&show_requested);
            let window_visible = Arc::clone(&window_visible);
            let control_tx = control_tx.clone();
            move || {
                while let Ok(event) = tray_icon::menu::MenuEvent::receiver().recv() {
                    if event.id == tray_ids.show_hide {
                        if !window_visible.load(Ordering::Relaxed) {
                            show_requested.store(true, Ordering::SeqCst);
                        }
                    } else if event.id == tray_ids.enabled {
                        pa.set_bypass(!pa.bypass());
                    } else if event.id == tray_ids.quit {
                        let _ = control_tx.try_send(Control::Quit);
                        break;
                    }
                }
            }
        });

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([420.0, 420.0])
                .with_title("Upalla")
                .with_icon(Arc::new(icon::window_icon()))
                .with_visible(false),
            ..Default::default()
        };

        // Always run eframe — this keeps [NSApp run] active for CoreAudio.
        eframe::run_native(
            "Upalla",
            options,
            Box::new({
                let pa = pa.clone();
                move |_cc| {
                    Ok(Box::new(UpallaApp::new(
                        pa.clone(),
                        status_rx.clone(),
                        control_rx.clone(),
                        enabled_check.clone(),
                        show_requested.clone(),
                        window_visible.clone(),
                    )))
                }
            }),
        )?;
    }

    log::debug!("quitting");
    pa.shutdown();
    Ok(())
}
