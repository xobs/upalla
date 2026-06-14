use upalla_pa::DeviceInfo;
use upalla_pa::DeviceLists;

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

    sinks: Vec<DeviceInfo>,
    sources: Vec<DeviceInfo>,
    default_sink_display: String,
    default_source_display: String,
    pub selected_sink: String,
    pub selected_source: String,
    pub refresh_devices: bool,
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
            sinks: Vec::new(),
            sources: Vec::new(),
            default_sink_display: String::new(),
            default_source_display: String::new(),
            selected_sink: String::new(),
            selected_source: String::new(),
            refresh_devices: true,
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
            self.pending_actions.push(TrayAction::ToggleEnabled);
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

    pub fn drain_pending_actions(&mut self) -> Vec<TrayAction> {
        std::mem::take(&mut self.pending_actions)
    }

    pub fn set_devices(&mut self, lists: DeviceLists) {
        self.sinks = lists.sinks;
        self.sources = lists.sources;
        self.default_sink_display = self
            .sinks
            .iter()
            .find(|d| d.name == lists.default_sink)
            .map(|d| d.description.clone())
            .unwrap_or(lists.default_sink);
        self.default_source_display = self
            .sources
            .iter()
            .find(|d| d.name == lists.default_source)
            .map(|d| d.description.clone())
            .unwrap_or(lists.default_source);
        if self.selected_sink.is_empty() {
            self.selected_sink = "@DEFAULT_SINK@".into();
        }
        if self.selected_source.is_empty() {
            self.selected_source = "@DEFAULT_SOURCE@".into();
        }
    }
}

fn device_combo(
    ui: &mut egui::Ui,
    label: &str,
    devices: &[DeviceInfo],
    default_alias: &str,
    default_name: &str,
    selected: &mut String,
) {
    let default_label = format!("Default ({})", default_name);
    let mut entries: Vec<(String, String)> = vec![(default_alias.into(), default_label)];
    for d in devices {
        entries.push((d.name.clone(), d.description.clone()));
    }

    let mut selected_idx = entries
        .iter()
        .position(|(name, _)| name == selected.as_str())
        .unwrap_or(0);

    let display_text = entries
        .get(selected_idx)
        .map(|(_, desc)| desc.as_str())
        .unwrap_or("");

    egui::ComboBox::from_id_salt(label)
        .width(200.0)
        .selected_text(display_text)
        .show_ui(ui, |ui| {
            for (i, (_, desc)) in entries.iter().enumerate() {
                ui.selectable_value(&mut selected_idx, i, desc);
            }
        });

    if selected_idx < entries.len() {
        *selected = entries[selected_idx].0.clone();
    }
}

#[allow(deprecated)]
pub fn render_gui(ctx: &egui::Context, state: &mut AppGuiState) {
    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
        ui.heading("Upalla");
        ui.separator();
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Output:");
            device_combo(
                ui,
                "output_sink",
                &state.sinks,
                "@DEFAULT_SINK@",
                &state.default_sink_display,
                &mut state.selected_sink,
            );
            if ui.button("\u{21bb}").clicked() {
                state.refresh_devices = true;
            }
        });

        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.label("Input: ");
            device_combo(
                ui,
                "input_source",
                &state.sources,
                "@DEFAULT_SOURCE@",
                &state.default_source_display,
                &mut state.selected_source,
            );
            if ui.button("\u{21bb}").clicked() {
                state.refresh_devices = true;
            }
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Buffer:");
            ui.add(
                egui::Slider::new(&mut state.buffer_ms, 10..=500)
                    .step_by(10.0)
                    .suffix(" ms"),
            );
        });

        ui.add_space(4.0);
        let mut enabled = !state.bypass;
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            state.bypass = !enabled;
        }

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
}
