use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSPopUpButton,
    NSSlider, NSTextField, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use crate::audio::{DeviceInfo, Status};

/// Layout metrics. The window is sized from these so it fits its content exactly
/// rather than carrying whatever size an earlier arrangement happened to need.
pub const MARGIN: f64 = 20.0;
const LABEL_W: f64 = 55.0;
const POPUP_W: f64 = 200.0;
const BTN_W: f64 = 30.0;
const ROW_H: f64 = 24.0;
const GAP: f64 = 6.0;
const BAR_W: f64 = 170.0;
const DB_W: f64 = 60.0;
const RMS_TITLE_W: f64 = 55.0;
const HEADER_W: f64 = 80.0;
const CHECKBOX_W: f64 = 86.0;
const ACTIVE_W: f64 = 78.0;

/// Eight rows, with a double gap above each section header.
const CONTENT_H: f64 = 8.0 * ROW_H + 7.0 * GAP + 3.0 * GAP;
/// The widest row is the device pickers and the meters, which happen to match.
const CONTENT_W: f64 = LABEL_W + GAP + POPUP_W + GAP + BTN_W;

pub const WINDOW_W: f64 = MARGIN + CONTENT_W + MARGIN;
pub const WINDOW_H: f64 = MARGIN + CONTENT_H + MARGIN;

pub struct Controls {
    pub pb_raw_slider: Retained<NSSlider>,
    pub pb_raw_label: Retained<NSTextField>,
    pub pb_filt_slider: Retained<NSSlider>,
    pub pb_filt_label: Retained<NSTextField>,
    pub rec_raw_slider: Retained<NSSlider>,
    pub rec_raw_label: Retained<NSTextField>,
    pub rec_filt_slider: Retained<NSSlider>,
    pub rec_filt_label: Retained<NSTextField>,
    pub sink_popup: Retained<NSPopUpButton>,
    pub source_popup: Retained<NSPopUpButton>,
    pub pb_enabled_checkbox: Retained<NSButton>,
    pub pb_active_checkbox: Retained<NSButton>,
    pub rec_enabled_checkbox: Retained<NSButton>,
    pub rec_active_checkbox: Retained<NSButton>,
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let tf = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    tf.setEditable(false);
    tf.setSelectable(false);
    tf.setBordered(false);
    tf.setBezeled(false);
    tf.setBackgroundColor(None);
    tf
}

fn bold_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let tf = label(mtm, text);
    tf.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
    tf
}

fn make_slider(mtm: MainThreadMarker) -> Retained<NSSlider> {
    let s = NSSlider::initWithFrame(
        NSSlider::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
    );
    s.setMinValue(0.0);
    s.setMaxValue(1.0);
    s.setFloatValue(0.0);
    s.setEnabled(false);
    s
}

fn make_popup(mtm: MainThreadMarker, target: &AnyObject, action: Sel) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
        false,
    );
    unsafe { p.setTarget(Some(target)) };
    unsafe { p.setAction(Some(action)) };
    p
}

pub fn build_controls(mtm: MainThreadMarker, view: &NSView, target: &AnyObject) -> Controls {
    let out_label = label(mtm, "Output:");
    let sink_popup = make_popup(mtm, target, sel!(sinkSelected:));
    let refresh_out = make_button(mtm, target, sel!(refreshDevices:), "\u{21bb}");

    view.addSubview(&out_label);
    view.addSubview(&sink_popup);
    view.addSubview(&refresh_out);

    let in_label = label(mtm, "Input:");
    let source_popup = make_popup(mtm, target, sel!(sourceSelected:));
    let refresh_in = make_button(mtm, target, sel!(refreshDevices:), "\u{21bb}");

    view.addSubview(&in_label);
    view.addSubview(&source_popup);
    view.addSubview(&refresh_in);

    let pb_header = bold_label(mtm, "Playback");
    let rec_header = bold_label(mtm, "Recording");
    view.addSubview(&pb_header);
    view.addSubview(&rec_header);

    // One "Enabled" box per chain, sitting beside its section header.
    let pb_enabled_checkbox = make_checkbox(mtm, target, sel!(playbackToggled:));
    let rec_enabled_checkbox = make_checkbox(mtm, target, sel!(recordingToggled:));
    view.addSubview(&pb_enabled_checkbox);
    view.addSubview(&rec_enabled_checkbox);

    // Read-only: both chains open and close on demand, so this reports the engine's
    // state rather than offering a control that would fight it.
    let pb_active_checkbox = make_indicator(mtm, "Active");
    let rec_active_checkbox = make_indicator(mtm, "Active");
    view.addSubview(&pb_active_checkbox);
    view.addSubview(&rec_active_checkbox);

    let pb_raw_title = label(mtm, "Raw:");
    let pb_raw_slider = make_slider(mtm);
    let pb_raw_label = label(mtm, "-60.0 dB");

    let pb_filt_title = label(mtm, "Filtered:");
    let pb_filt_slider = make_slider(mtm);
    let pb_filt_label = label(mtm, "-60.0 dB");

    view.addSubview(&pb_raw_title);
    view.addSubview(&pb_raw_slider);
    view.addSubview(&pb_raw_label);
    view.addSubview(&pb_filt_title);
    view.addSubview(&pb_filt_slider);
    view.addSubview(&pb_filt_label);

    let rec_raw_title = label(mtm, "Raw:");
    let rec_raw_slider = make_slider(mtm);
    let rec_raw_label = label(mtm, "-60.0 dB");

    let rec_filt_title = label(mtm, "Filtered:");
    let rec_filt_slider = make_slider(mtm);
    let rec_filt_label = label(mtm, "-60.0 dB");

    view.addSubview(&rec_raw_title);
    view.addSubview(&rec_raw_slider);
    view.addSubview(&rec_raw_label);
    view.addSubview(&rec_filt_title);
    view.addSubview(&rec_filt_slider);
    view.addSubview(&rec_filt_label);

    layout_controls(
        &out_label,
        &sink_popup,
        &refresh_out,
        &in_label,
        &source_popup,
        &refresh_in,
        &pb_enabled_checkbox,
        &rec_enabled_checkbox,
        &pb_active_checkbox,
        &rec_active_checkbox,
        &pb_header,
        &rec_header,
        &pb_raw_title,
        &pb_raw_slider,
        &pb_raw_label,
        &pb_filt_title,
        &pb_filt_slider,
        &pb_filt_label,
        &rec_raw_title,
        &rec_raw_slider,
        &rec_raw_label,
        &rec_filt_title,
        &rec_filt_slider,
        &rec_filt_label,
    );

    Controls {
        pb_raw_slider,
        pb_raw_label,
        pb_filt_slider,
        pb_filt_label,
        rec_raw_slider,
        rec_raw_label,
        rec_filt_slider,
        rec_filt_label,
        sink_popup,
        source_popup,
        pb_enabled_checkbox,
        rec_enabled_checkbox,
        pb_active_checkbox,
        rec_active_checkbox,
    }
}

fn make_button(
    mtm: MainThreadMarker,
    target: &AnyObject,
    action: Sel,
    title: &str,
) -> Retained<NSButton> {
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    };
    b.setBezelStyle(NSBezelStyle::SmallSquare);
    b
}

fn set_checkbox(checkbox: &NSButton, on: bool) {
    checkbox.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
}

fn make_indicator(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    let b = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    b.setState(NSControlStateValueOff);
    b.setEnabled(false);
    b
}

fn make_checkbox(mtm: MainThreadMarker, target: &AnyObject, action: Sel) -> Retained<NSButton> {
    let b = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str("Enabled"),
            Some(target),
            Some(action),
            mtm,
        )
    };
    b.setState(NSControlStateValueOn);
    b
}

#[allow(clippy::too_many_arguments)]
fn layout_controls(
    out_label: &NSTextField,
    sink_popup: &NSPopUpButton,
    refresh_out: &NSButton,
    in_label: &NSTextField,
    source_popup: &NSPopUpButton,
    refresh_in: &NSButton,
    pb_enabled_checkbox: &NSButton,
    rec_enabled_checkbox: &NSButton,
    pb_active_checkbox: &NSButton,
    rec_active_checkbox: &NSButton,
    pb_header: &NSTextField,
    rec_header: &NSTextField,
    pb_raw_title: &NSTextField,
    pb_raw_slider: &NSSlider,
    pb_raw_label: &NSTextField,
    pb_filt_title: &NSTextField,
    pb_filt_slider: &NSSlider,
    pb_filt_label: &NSTextField,
    rec_raw_title: &NSTextField,
    rec_raw_slider: &NSSlider,
    rec_raw_label: &NSTextField,
    rec_filt_title: &NSTextField,
    rec_filt_slider: &NSSlider,
    rec_filt_label: &NSTextField,
) {
    let (label_w, popup_w, btn_w) = (LABEL_W, POPUP_W, BTN_W);
    let (row_h, gap, margin) = (ROW_H, GAP, MARGIN);
    let (bar_w, db_w, rms_title_w) = (BAR_W, DB_W, RMS_TITLE_W);
    let (header_w, checkbox_w) = (HEADER_W, CHECKBOX_W);

    // Top-down from the top margin, so the bottom margin comes out equal.
    let mut y: f64 = WINDOW_H - margin - row_h;

    out_label.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(label_w, row_h),
    ));
    sink_popup.setFrame(NSRect::new(
        NSPoint::new(margin + label_w + gap, y),
        NSSize::new(popup_w, row_h),
    ));
    refresh_out.setFrame(NSRect::new(
        NSPoint::new(margin + label_w + gap + popup_w + gap, y),
        NSSize::new(btn_w, row_h),
    ));
    y -= row_h + gap;

    in_label.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(label_w, row_h),
    ));
    source_popup.setFrame(NSRect::new(
        NSPoint::new(margin + label_w + gap, y),
        NSSize::new(popup_w, row_h),
    ));
    refresh_in.setFrame(NSRect::new(
        NSPoint::new(margin + label_w + gap + popup_w + gap, y),
        NSSize::new(btn_w, row_h),
    ));
    y -= row_h + gap * 2.0;

    pb_header.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(header_w, row_h),
    ));
    pb_enabled_checkbox.setFrame(NSRect::new(
        NSPoint::new(margin + header_w + gap, y),
        NSSize::new(checkbox_w, row_h),
    ));
    pb_active_checkbox.setFrame(NSRect::new(
        NSPoint::new(margin + header_w + gap + checkbox_w + gap, y),
        NSSize::new(ACTIVE_W, row_h),
    ));
    y -= row_h + gap;

    pb_raw_title.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(rms_title_w, row_h),
    ));
    pb_raw_slider.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap, y),
        NSSize::new(bar_w, row_h),
    ));
    pb_raw_label.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap + bar_w + gap, y),
        NSSize::new(db_w, row_h),
    ));
    y -= row_h + gap;

    pb_filt_title.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(rms_title_w, row_h),
    ));
    pb_filt_slider.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap, y),
        NSSize::new(bar_w, row_h),
    ));
    pb_filt_label.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap + bar_w + gap, y),
        NSSize::new(db_w, row_h),
    ));
    y -= row_h + gap * 2.0;

    rec_header.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(header_w, row_h),
    ));
    rec_enabled_checkbox.setFrame(NSRect::new(
        NSPoint::new(margin + header_w + gap, y),
        NSSize::new(checkbox_w, row_h),
    ));
    rec_active_checkbox.setFrame(NSRect::new(
        NSPoint::new(margin + header_w + gap + checkbox_w + gap, y),
        NSSize::new(ACTIVE_W, row_h),
    ));
    y -= row_h + gap;

    rec_raw_title.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(rms_title_w, row_h),
    ));
    rec_raw_slider.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap, y),
        NSSize::new(bar_w, row_h),
    ));
    rec_raw_label.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap + bar_w + gap, y),
        NSSize::new(db_w, row_h),
    ));
    y -= row_h + gap;

    rec_filt_title.setFrame(NSRect::new(
        NSPoint::new(margin, y),
        NSSize::new(rms_title_w, row_h),
    ));
    rec_filt_slider.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap, y),
        NSSize::new(bar_w, row_h),
    ));
    rec_filt_label.setFrame(NSRect::new(
        NSPoint::new(margin + rms_title_w + gap + bar_w + gap, y),
        NSSize::new(db_w, row_h),
    ));
}

impl Controls {
    pub fn update_levels(&self, status: &Status) {
        self.update_slider(&self.pb_raw_slider, &self.pb_raw_label, status.playback_in);
        self.update_slider(
            &self.pb_filt_slider,
            &self.pb_filt_label,
            status.playback_out,
        );
        self.update_slider(
            &self.rec_raw_slider,
            &self.rec_raw_label,
            status.recording_in,
        );
        self.update_slider(
            &self.rec_filt_slider,
            &self.rec_filt_label,
            status.recording_out,
        );
        set_checkbox(&self.pb_active_checkbox, status.playback_active);
        set_checkbox(&self.rec_active_checkbox, status.recording_active);
    }

    fn update_slider(&self, slider: &NSSlider, label: &NSTextField, level: f32) {
        let db = if level > 0.0 {
            (20.0 * level.log10()).max(-60.0)
        } else {
            -60.0
        };
        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
        slider.setDoubleValue(norm);
        label.setStringValue(&NSString::from_str(&format!("{:.1} dB", db)));
    }

    pub fn set_playback_enabled(&self, enabled: bool) {
        set_checkbox(&self.pb_enabled_checkbox, enabled);
    }

    pub fn set_recording_enabled(&self, enabled: bool) {
        set_checkbox(&self.rec_enabled_checkbox, enabled);
    }

    pub fn populate_devices(&self, sinks: &[DeviceInfo], sources: &[DeviceInfo]) {
        let selected_sink = self
            .sink_popup
            .titleOfSelectedItem()
            .map(|s| s.to_string())
            .unwrap_or_default();

        self.sink_popup.removeAllItems();
        self.sink_popup
            .addItemWithTitle(&NSString::from_str("Default"));
        for dev in sinks {
            if dev.name.contains("BlackHole") {
                continue;
            }
            self.sink_popup
                .addItemWithTitle(&NSString::from_str(&dev.name));
        }
        if !selected_sink.is_empty() {
            self.sink_popup
                .selectItemWithTitle(&NSString::from_str(&selected_sink));
        }

        let selected_source = self
            .source_popup
            .titleOfSelectedItem()
            .map(|s| s.to_string())
            .unwrap_or_default();

        self.source_popup.removeAllItems();
        self.source_popup
            .addItemWithTitle(&NSString::from_str("Default"));
        for dev in sources {
            if dev.name.contains("BlackHole") {
                continue;
            }
            self.source_popup
                .addItemWithTitle(&NSString::from_str(&dev.name));
        }
        if !selected_source.is_empty() {
            self.source_popup
                .selectItemWithTitle(&NSString::from_str(&selected_source));
        }
    }

    pub fn selected_sink(&self) -> String {
        self.sink_popup
            .titleOfSelectedItem()
            .map(|s| {
                let name = s.to_string();
                if name == "Default" {
                    "@DEFAULT_SINK@".into()
                } else {
                    name
                }
            })
            .unwrap_or_else(|| "@DEFAULT_SINK@".into())
    }

    pub fn selected_source(&self) -> String {
        self.source_popup
            .titleOfSelectedItem()
            .map(|s| {
                let name = s.to_string();
                if name == "Default" {
                    "@DEFAULT_SOURCE@".into()
                } else {
                    name
                }
            })
            .unwrap_or_else(|| "@DEFAULT_SOURCE@".into())
    }

    pub fn playback_enabled(&self) -> bool {
        self.pb_enabled_checkbox.state() == NSControlStateValueOn
    }

    pub fn recording_enabled(&self) -> bool {
        self.rec_enabled_checkbox.state() == NSControlStateValueOn
    }
}
