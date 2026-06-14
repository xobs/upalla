use crate::tray::TrayMenuIds;
use crate::TrayAction;

pub struct AppGuiState {
    pub bypass: bool,
    pub buffer_ms: u32,
    pub rms_in: f32,
    pub rms_out: f32,

    tray_ids: Option<TrayMenuIds>,
    show_window: bool,
    start_hidden: bool,
    pending_actions: Vec<TrayAction>,
}

impl AppGuiState {
    pub fn new(tray_ids: Option<TrayMenuIds>) -> Self {
        AppGuiState {
            bypass: false,
            buffer_ms: 48,
            rms_in: 0.0,
            rms_out: 0.0,
            tray_ids,
            show_window: false,
            start_hidden: true,
            pending_actions: Vec::new(),
        }
    }

    pub fn handle_tray_event(&mut self, event: tray_icon::menu::MenuEvent) {
        let Some(ids) = &self.tray_ids else {
            return;
        };
        if event.id == ids.show_hide {
            if self.show_window {
                self.pending_actions.push(TrayAction::Hide);
                self.show_window = false;
            } else {
                self.pending_actions.push(TrayAction::Show);
                self.show_window = true;
            }
        } else if event.id == ids.enabled {
            self.pending_actions.push(TrayAction::SetEnabled(true));
        } else if event.id == ids.quit {
            self.pending_actions.push(TrayAction::Quit);
        }
    }

    pub fn start_hidden(&mut self) -> bool {
        if self.start_hidden {
            self.start_hidden = false;
            true
        } else {
            false
        }
    }

    pub fn drain_actions(&mut self) -> Vec<TrayAction> {
        std::mem::take(&mut self.pending_actions)
    }
}

#[allow(deprecated)]
pub fn render_gui(ctx: &egui::Context, state: &mut AppGuiState) -> Vec<TrayAction> {
    let actions = state.drain_actions();

    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
        ui.heading("Upalla");
        ui.separator();
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Buffer:");
            ui.add(
                egui::Slider::new(&mut state.buffer_ms, 10..=500)
                    .step_by(10.0)
                    .suffix(" ms"),
            );
        });

        ui.add_space(4.0);
        ui.checkbox(&mut state.bypass, "Bypass");

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label("Levels");

        let in_db = if state.rms_in > 0.0 {
            20.0 * state.rms_in.log10()
        } else {
            -60.0
        };
        let out_db = if state.rms_out > 0.0 {
            20.0 * state.rms_out.log10()
        } else {
            -60.0
        };

        ui.horizontal(|ui| {
            ui.label("In: ");
            let bar_val = ((in_db + 60.0) / 60.0).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(bar_val)
                    .text(format!("{:.1} dB", in_db))
                    .desired_width(200.0),
            );
        });

        ui.horizontal(|ui| {
            ui.label("Out:");
            let bar_val = ((out_db + 60.0) / 60.0).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(bar_val)
                    .text(format!("{:.1} dB", out_db))
                    .desired_width(200.0),
            );
        });
    });

    actions
}
