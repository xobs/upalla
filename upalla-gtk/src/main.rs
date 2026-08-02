#![allow(deprecated)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use gtk4::prelude::*;
use ksni::blocking::TrayMethods;
use upalla_core::model::Model;
use upalla_pa::{PaFilter, Status};

mod icon;

fn db_val(sample: f32) -> f32 {
    if sample > 0.0 {
        20.0 * sample.log10()
    } else {
        -60.0
    }
}

struct AppState {
    status_rx: Receiver<Status>,
    show_rx: Receiver<()>,
    previous_sink_idx: u32,
    previous_source_idx: u32,
    previous_pb_bypass: bool,
    previous_rec_bypass: bool,
    playback_in_bar: gtk4::ProgressBar,
    playback_out_bar: gtk4::ProgressBar,
    recording_in_bar: gtk4::ProgressBar,
    recording_out_bar: gtk4::ProgressBar,
    playback_in_label: gtk4::Label,
    playback_out_label: gtk4::Label,
    recording_in_label: gtk4::Label,
    recording_out_label: gtk4::Label,
    sink_combo: gtk4::ComboBoxText,
    source_combo: gtk4::ComboBoxText,
    playback_enabled: gtk4::CheckButton,
    recording_enabled: gtk4::CheckButton,
}

impl AppState {
    fn update_levels(&mut self, pa: &PaFilter, window: &gtk4::ApplicationWindow) {
        // Handle show requests from tray
        while self.show_rx.try_recv().is_ok() {
            window.present();
        }

        while let Ok(status) = self.status_rx.try_recv() {
            self.update_bar(
                &self.playback_in_bar,
                &self.playback_in_label,
                status.playback_in,
            );
            self.update_bar(
                &self.playback_out_bar,
                &self.playback_out_label,
                status.playback_out,
            );
            self.update_bar(
                &self.recording_in_bar,
                &self.recording_in_label,
                status.recording_in,
            );
            self.update_bar(
                &self.recording_out_bar,
                &self.recording_out_label,
                status.recording_out,
            );
        }

        let sink_idx = self.sink_combo.active().unwrap_or(0u32);
        if sink_idx != self.previous_sink_idx {
            self.previous_sink_idx = sink_idx;
            if sink_idx > 0 {
                if let Some(text) = self.sink_combo.active_text() {
                    pa.set_sink(text.to_string());
                }
            }
        }

        let source_idx = self.source_combo.active().unwrap_or(0u32);
        if source_idx != self.previous_source_idx {
            self.previous_source_idx = source_idx;
            if source_idx > 0 {
                if let Some(text) = self.source_combo.active_text() {
                    pa.set_source(text.to_string());
                }
            }
        }

        // Playback checkbox → playback bypass
        let pb_enabled = self.playback_enabled.is_active();
        if pb_enabled != !self.previous_pb_bypass {
            self.previous_pb_bypass = !pb_enabled;
            pa.set_playback_bypass(!pb_enabled);
        } else if !pb_enabled != pa.playback_bypass() {
            let bypass = pa.playback_bypass();
            self.playback_enabled.set_active(!bypass);
            self.previous_pb_bypass = bypass;
        }

        // Recording checkbox → recording bypass
        let rec_enabled = self.recording_enabled.is_active();
        if rec_enabled != !self.previous_rec_bypass {
            self.previous_rec_bypass = !rec_enabled;
            pa.set_recording_bypass(!rec_enabled);
        } else if !rec_enabled != pa.recording_bypass() {
            let bypass = pa.recording_bypass();
            self.recording_enabled.set_active(!bypass);
            self.previous_rec_bypass = bypass;
        }
    }

    fn update_bar(&self, bar: &gtk4::ProgressBar, label: &gtk4::Label, rms: f32) {
        let dbv = db_val(rms);
        let clamped = dbv.clamp(-60.0, 0.0);
        let fraction = ((clamped + 60.0) / 60.0) as f64;
        bar.set_fraction(fraction);
        bar.set_text(Some(&format!("{:.0} dB", clamped.round())));
        label.set_text(&format!("{:.1} dB", clamped));
    }

    fn populate_devices(&mut self, pa: &PaFilter) {
        let devices = pa.enumerate_devices();

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

        self.sink_combo.remove_all();
        self.source_combo.remove_all();

        self.sink_combo
            .append_text(&format!("Default ({})", default_sink_display));
        for d in &devices.sinks {
            self.sink_combo.append_text(&d.description);
        }

        self.source_combo
            .append_text(&format!("Default ({})", default_source_display));
        for d in &devices.sources {
            self.source_combo.append_text(&d.description);
        }

        self.sink_combo.set_active(Some(self.previous_sink_idx));
        self.source_combo.set_active(Some(self.previous_source_idx));
    }
}

fn make_bar_row(label: &str) -> (gtk4::ProgressBar, gtk4::Label, gtk4::Box) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);

    let lbl = gtk4::Label::new(Some(label));
    lbl.set_width_chars(10);
    lbl.set_halign(gtk4::Align::Start);
    row.append(&lbl);

    let bar = gtk4::ProgressBar::new();
    bar.set_hexpand(true);
    bar.set_show_text(true);
    bar.set_text(Some("-60 dB"));
    row.append(&bar);

    let val = gtk4::Label::new(Some("-60.0 dB"));
    val.set_width_chars(8);
    val.set_halign(gtk4::Align::End);
    row.append(&val);

    (bar, val, row)
}

fn build_window(app: &gtk4::Application) -> (gtk4::ApplicationWindow, AppState) {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("Upalla")
        .default_width(420)
        .default_height(480)
        .build();

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    outer.set_margin_start(10);
    outer.set_margin_end(10);
    outer.set_margin_top(10);
    outer.set_margin_bottom(10);

    let title = gtk4::Label::new(Some("Upalla \u{2014} Real-time Denoising"));
    title.add_css_class("title-1");
    title.set_halign(gtk4::Align::Start);
    outer.append(&title);

    // Output row
    let out_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let out_lbl = gtk4::Label::new(Some("Output:"));
    out_lbl.set_width_chars(8);
    out_lbl.set_halign(gtk4::Align::Start);
    out_row.append(&out_lbl);

    let sink_combo = gtk4::ComboBoxText::new();
    sink_combo.set_hexpand(true);
    sink_combo.set_size_request(60, -1);
    out_row.append(&sink_combo);

    let refresh_btn = gtk4::Button::with_label("\u{21bb}");
    out_row.append(&refresh_btn);

    outer.append(&out_row);

    // Input row
    let in_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let in_lbl = gtk4::Label::new(Some("Input:"));
    in_lbl.set_width_chars(8);
    in_lbl.set_halign(gtk4::Align::Start);
    in_row.append(&in_lbl);

    let source_combo = gtk4::ComboBoxText::new();
    source_combo.set_hexpand(true);
    source_combo.set_size_request(60, -1);
    in_row.append(&source_combo);

    outer.append(&in_row);

    // Spacer
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    outer.append(&spacer);

    // Playback section
    let pb_lbl = gtk4::Label::new(Some("Playback"));
    pb_lbl.add_css_class("title-4");
    pb_lbl.set_halign(gtk4::Align::Start);
    outer.append(&pb_lbl);

    let playback_enabled = gtk4::CheckButton::with_label("Enabled");
    playback_enabled.set_active(true);
    outer.append(&playback_enabled);

    let (pb_in_bar, pb_in_label, pb_in_row) = make_bar_row("Raw:");
    outer.append(&pb_in_row);
    let (pb_out_bar, pb_out_label, pb_out_row) = make_bar_row("Filtered:");
    outer.append(&pb_out_row);

    // Recording section
    let rec_lbl = gtk4::Label::new(Some("Recording"));
    rec_lbl.add_css_class("title-4");
    rec_lbl.set_halign(gtk4::Align::Start);
    outer.append(&rec_lbl);

    let recording_enabled = gtk4::CheckButton::with_label("Enabled");
    recording_enabled.set_active(true);
    outer.append(&recording_enabled);

    let (rec_in_bar, rec_in_label, rec_in_row) = make_bar_row("Raw:");
    outer.append(&rec_in_row);
    let (rec_out_bar, rec_out_label, rec_out_row) = make_bar_row("Filtered:");
    outer.append(&rec_out_row);

    window.set_child(Some(&outer));

    let state = AppState {
        status_rx: crossbeam_channel::unbounded().1,
        show_rx: crossbeam_channel::unbounded().1,
        previous_sink_idx: 0,
        previous_source_idx: 0,
        previous_pb_bypass: false,
        previous_rec_bypass: false,
        playback_in_bar: pb_in_bar,
        playback_out_bar: pb_out_bar,
        recording_in_bar: rec_in_bar,
        recording_out_bar: rec_out_bar,
        playback_in_label: pb_in_label,
        playback_out_label: pb_out_label,
        recording_in_label: rec_in_label,
        recording_out_label: rec_out_label,
        sink_combo,
        source_combo,
        playback_enabled,
        recording_enabled,
    };

    (window, state)
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
                    Box::new(move |_tray: &mut Self| {
                        let new_state =
                            !(pb_flag.load(Ordering::Relaxed) && rec_flag.load(Ordering::Relaxed));
                        pb_flag.store(new_state, Ordering::Relaxed);
                        rec_flag.store(new_state, Ordering::Relaxed);
                        pa.set_playback_bypass(!new_state);
                        pa.set_recording_bypass(!new_state);
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
                        std::process::exit(0);
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
    log::info!("Starting Upalla GTK4...");

    let pb_enabled = Arc::new(AtomicBool::new(true));
    let rec_enabled = Arc::new(AtomicBool::new(true));
    let pa = Arc::new(PaFilter::new(
        Model::default(),
        Arc::clone(&pb_enabled),
        Arc::clone(&rec_enabled),
    )?);
    let status_rx = pa.status_receiver().clone();
    let (show_tx, show_rx) = unbounded();

    let app = gtk4::Application::builder()
        .application_id("io.github.upalla")
        .build();

    let show_rx_cell = Rc::new(RefCell::new(Some(show_rx)));

    app.connect_activate({
        let pa = pa.clone();
        move |app| {
            let (window, mut state) = build_window(app);
            state.status_rx = status_rx.clone();
            state.show_rx = show_rx_cell
                .borrow_mut()
                .take()
                .expect("activate called twice");

            // Set dock icon when the window surface is available
            {
                let icon_pixbuf = icon::icon_pixbuf();
                window.connect_realize(move |win| {
                    if let Some(surface) = win.surface() {
                        if let Ok(toplevel) = surface.downcast::<gtk4::gdk::Toplevel>() {
                            let texture = gtk4::gdk::Texture::for_pixbuf(&icon_pixbuf);
                            toplevel.set_icon_list(&[texture]);
                        }
                    }
                });
            }

            let state = Rc::new(RefCell::new(state));

            // Status timer (250ms)
            {
                let state = state.clone();
                let pa = pa.clone();
                let window = window.clone();
                gtk4::glib::timeout_add_local(Duration::from_millis(250), move || {
                    state.borrow_mut().update_levels(&pa, &window);
                    gtk4::glib::ControlFlow::Continue
                });
            }

            // Device refresh timer (1s)
            {
                let state = state.clone();
                let pa = pa.clone();
                gtk4::glib::timeout_add_local(Duration::from_secs(1), move || {
                    state.borrow_mut().populate_devices(&pa);
                    gtk4::glib::ControlFlow::Continue
                });
            }

            // Refresh button
            {
                let state = state.clone();
                let pa = pa.clone();
                if let Some(outer) = window.child().and_then(|c| c.downcast::<gtk4::Box>().ok()) {
                    if let Some(out_row) =
                        nth_child(&outer, 1).and_then(|c| c.downcast::<gtk4::Box>().ok())
                    {
                        if let Some(btn) =
                            nth_child(&out_row, 2).and_then(|c| c.downcast::<gtk4::Button>().ok())
                        {
                            btn.connect_clicked(move |_| {
                                state.borrow_mut().populate_devices(&pa);
                            });
                        }
                    }
                }
            }

            // Initial populate
            state.borrow_mut().populate_devices(&pa);

            // Show window
            window.present();

            // Close → hide
            window.connect_close_request(move |win| {
                win.hide();
                gtk4::glib::Propagation::Stop
            });
        }
    });

    // Tray — spawned before event loop so it's ready when the window appears
    let tray = UpallaTray {
        pa: pa.clone(),
        pb_enabled: pb_enabled.clone(),
        rec_enabled: rec_enabled.clone(),
        show_tx,
    };

    let _handle = tray.spawn().expect("ksni tray service");

    app.run();
    Ok(())
}

fn nth_child(widget: &gtk4::Box, n: usize) -> Option<gtk4::Widget> {
    let mut child = widget.first_child();
    for _ in 0..n {
        child = child?.next_sibling();
    }
    child
}
