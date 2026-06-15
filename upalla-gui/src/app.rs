use upalla_pa::DeviceInfo;
use upalla_pa::DeviceLists;

pub struct AppGuiState {
    pub bypass: bool,
    pub rms_in: f32,
    pub rms_out: f32,

    sinks: Vec<DeviceInfo>,
    sources: Vec<DeviceInfo>,
    default_sink_display: String,
    default_source_display: String,
    pub selected_sink: String,
    pub selected_source: String,
    pub refresh_devices: bool,
}

impl AppGuiState {
    pub fn new() -> Self {
        AppGuiState {
            bypass: false,
            rms_in: 0.0,
            rms_out: 0.0,
            sinks: Vec::new(),
            sources: Vec::new(),
            default_sink_display: String::new(),
            default_source_display: String::new(),
            selected_sink: String::new(),
            selected_source: String::new(),
            refresh_devices: true,
        }
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

        ui.add_space(4.0);
        let mut enabled = !state.bypass;
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            log::trace!(
                "\"Enabled\" checkbox changed, enabled is now {enabled} (bypass is now {})",
                !enabled
            );
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
