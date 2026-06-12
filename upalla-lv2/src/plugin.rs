use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use upalla_core::auxiliary::Auxiliary;
use upalla_core::config;
use upalla_core::postprocess::{apply_deep_filter, apply_erb_mask};
use upalla_core::stft::StftEngine;
use upalla_core::Denoiser;

use truce::prelude::*;

use crate::params::{UpallaParams, UpallaParamsParamId};
use crate::worker::{GpuTask, WorkerChannel};

const MODEL_SEARCH_PATHS: &[&str] = &[
    "/usr/share/upalla",
    "/usr/local/share/upalla",
    ".local/share/upalla",
    "upalla/models",
];

fn find_model_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home).join(".local/share/upalla");
        if has_model(&path) {
            return Some(path);
        }
    }
    for search in MODEL_SEARCH_PATHS {
        if search.starts_with('.') {
            if let Ok(home) = std::env::var("HOME") {
                let path = PathBuf::from(home).join(search);
                if has_model(&path) {
                    return Some(path);
                }
            }
        } else {
            let path = PathBuf::from(search);
            if has_model(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn has_model(dir: &PathBuf) -> bool {
    dir.join("enc.onnx").exists()
        && dir.join("erb_dec.onnx").exists()
        && dir.join("df_dec.onnx").exists()
}

pub struct UpallaPlugin {
    params: Arc<UpallaParams>,
    aux: Auxiliary,
    worker: Option<WorkerChannel>,
    cpu_denoiser: Option<Denoiser>,
    stft_in: Vec<StftEngine>,
    stft_out: Vec<StftEngine>,
    input_buf: Vec<VecDeque<f32>>,
    output_buf: Vec<VecDeque<f32>>,
    spec_real_history: Vec<Vec<Vec<f32>>>,
    spec_imag_history: Vec<Vec<Vec<f32>>>,
    channels: usize,
    pending_task: bool,
    last_sent_frame: usize,
    last_mask: Option<Vec<f32>>,
    last_coefs: Option<Vec<f32>>,
    last_num_frames: usize,
    vad_grace_remaining: usize,
}

impl UpallaPlugin {
    pub fn new(params: Arc<UpallaParams>) -> Self {
        let aux = Auxiliary::new();
        let window = aux.window.clone();

        let model_dir = find_model_dir();

        let (worker, cpu_denoiser) = if let Some(ref path) = model_dir {
            log::info!("Loading model from {:?}", path);
            match Denoiser::new(path.clone()) {
                Ok(denoiser) => {
                    log::info!("Upalla ONNX model loaded, spawning worker thread for GPU inference");
                    (Some(WorkerChannel::spawn(denoiser)), None)
                }
                Err(e) => {
                    log::error!("Failed to load ONNX model for GPU worker: {e}");
                    match Denoiser::new(path.clone()) {
                        Ok(d) => {
                            log::info!("Falling back to synchronous CPU inference");
                            (None, Some(d))
                        }
                        Err(e2) => {
                            log::error!("CPU fallback also failed: {e2}");
                            (None, None)
                        }
                    }
                }
            }
        } else {
            log::warn!(
                "No ONNX model found at ~/.local/share/upalla/. \
                 Audio will pass through unprocessed. \
                 Run 'scripts/download-model.sh' to fetch the model."
            );
            (None, None)
        };

        let window2 = window.clone();
        UpallaPlugin {
            params,
            aux,
            worker,
            cpu_denoiser,
            stft_in: vec![StftEngine::new(window), StftEngine::new(window2.clone())],
            stft_out: vec![
                StftEngine::new(window2.clone()),
                StftEngine::new(window2),
            ],
            input_buf: vec![
                VecDeque::with_capacity(config::HOP_SIZE * 4),
                VecDeque::with_capacity(config::HOP_SIZE * 4),
            ],
            output_buf: vec![
                VecDeque::with_capacity(config::HOP_SIZE * 4),
                VecDeque::with_capacity(config::HOP_SIZE * 4),
            ],
            spec_real_history: Vec::new(),
            spec_imag_history: Vec::new(),
            channels: 1,
            pending_task: false,
            last_sent_frame: 0,
            last_mask: None,
            last_coefs: None,
            last_num_frames: 0,
            vad_grace_remaining: 0,
        }
    }

    fn reinit_channels(&mut self, num_channels: usize) {
        if num_channels == self.channels && !self.stft_in.is_empty() {
            return;
        }
        self.channels = num_channels;
        let window = self.aux.window.clone();
        self.stft_in = (0..num_channels)
            .map(|_| StftEngine::new(window.clone()))
            .collect();
        self.stft_out = (0..num_channels)
            .map(|_| StftEngine::new(window.clone()))
            .collect();
        self.input_buf = (0..num_channels)
            .map(|_| VecDeque::with_capacity(config::HOP_SIZE * 4))
            .collect();
        self.output_buf = (0..num_channels)
            .map(|_| VecDeque::with_capacity(config::HOP_SIZE * 4))
            .collect();
        self.spec_real_history.clear();
        self.spec_imag_history.clear();
        self.last_sent_frame = 0;
    }

    fn frames_to_send(&self) -> usize {
        if self.spec_real_history.len() > self.last_sent_frame {
            self.spec_real_history.len() - self.last_sent_frame
        } else {
            0
        }
    }

    fn send_to_worker_if_needed(&mut self) {
        if self.worker.is_none() || self.pending_task {
            return;
        }

        let new_frames = self.frames_to_send();
        if new_frames < 8 {
            return;
        }

        let start = self.last_sent_frame;
        let end = self.spec_real_history.len();
        let num_frames = end - start;

        let mut all_spec_real: Vec<Vec<f32>> =
            vec![vec![0.0; config::FREQ_BINS]; num_frames];
        let mut all_spec_imag: Vec<Vec<f32>> =
            vec![vec![0.0; config::FREQ_BINS]; num_frames];

        for (i, t) in (start..end).enumerate() {
            for f in 0..config::FREQ_BINS {
                all_spec_real[i][f] = self.spec_real_history[t][0][f];
                all_spec_imag[i][f] = self.spec_imag_history[t][0][f];
            }
        }

        if let Some(ref worker) = self.worker {
            let task = GpuTask {
                spec_real: all_spec_real,
                spec_imag: all_spec_imag,
                num_frames,
            };
            worker.try_send(task);
            self.pending_task = true;
            self.last_sent_frame = end;
        }
    }

    fn check_worker_results(&mut self) {
        if let Some(ref worker) = self.worker {
            if let Some(response) = worker.try_recv() {
                self.last_mask = Some(response.erb_mask);
                self.last_coefs = Some(response.df_coefs);
                self.last_num_frames = response.processed_frames;
                self.pending_task = false;
            }
        }
    }

    fn process_cpu_fallback(&mut self) {
        if self.cpu_denoiser.is_none() {
            return;
        }

        let denoiser = self.cpu_denoiser.as_mut().unwrap();

        while self.input_buf[0].len() >= config::HOP_SIZE {
            let mut input_frame = vec![0.0f32; config::HOP_SIZE];
            for i in 0..config::HOP_SIZE {
                for ch in 0..self.channels {
                    if ch == 0 {
                        input_frame[i] = self.input_buf[ch].pop_front().unwrap_or(0.0);
                    } else {
                        self.input_buf[ch].pop_front();
                    }
                }
            }

            let mut out_frame = vec![0.0f32; config::HOP_SIZE];
            let produced = denoiser.process(&input_frame, &mut out_frame).unwrap_or(0);

            for ch in 0..self.channels {
                for i in 0..produced {
                    self.output_buf[ch].push_back(out_frame[i]);
                }
            }
        }
    }

    fn apply_vad_gating(&mut self) {
        let vad_threshold = self.params.vad_threshold_normalized();
        let grace_blocks = 20;

        let energy: f32 = self
            .output_buf
            .first()
            .map(|buf| {
                let sum: f32 = buf.iter().take(config::HOP_SIZE).map(|&s| s * s).sum();
                (sum / config::HOP_SIZE as f32).sqrt()
            })
            .unwrap_or(0.0);

        if energy > vad_threshold * 0.1 {
            self.vad_grace_remaining = grace_blocks;
        } else if self.vad_grace_remaining > 0 {
            self.vad_grace_remaining -= 1;
        }
    }

    fn drain_output(&mut self, buffer: &mut AudioBuffer, n_samples: usize) {
        let num_channels = self.channels.min(2);
        let silence = self.vad_grace_remaining == 0;

        for ch in 0..num_channels {
            let (_, out) = buffer.io(ch);
            for i in 0..n_samples {
                if silence {
                    out[i] = 0.0;
                    if !self.output_buf[ch].is_empty() {
                        self.output_buf[ch].pop_front();
                    }
                } else if let Some(s) = self.output_buf[ch].pop_front() {
                    out[i] = s;
                }
            }
        }
    }

    fn try_apply_results(&mut self, num_channels: usize) {
        self.check_worker_results();

        if let (Some(ref mask), Some(ref coefs)) =
            (&self.last_mask, &self.last_coefs)
        {
            for t in 0..self.last_num_frames.min(self.spec_real_history.len()) {
                for ch in 0..num_channels {
                    let spec_real = &self.spec_real_history[t][ch];
                    let spec_imag = &self.spec_imag_history[t][ch];

                    let mut masked_real = spec_real.clone();
                    let mut masked_imag = spec_imag.clone();

                    apply_erb_mask(
                        &mut masked_real,
                        &mut masked_imag,
                        mask,
                        &self.aux.erb_inv_fb,
                        t,
                    );

                    let mut out_real = vec![0.0f32; config::FREQ_BINS];
                    let mut out_imag = vec![0.0f32; config::FREQ_BINS];

                    let spec_real_flat: Vec<Vec<f32>> = self
                        .spec_real_history
                        .iter()
                        .map(|v| v[ch].clone())
                        .collect();
                    let spec_imag_flat: Vec<Vec<f32>> = self
                        .spec_imag_history
                        .iter()
                        .map(|v| v[ch].clone())
                        .collect();

                    apply_deep_filter(
                        &spec_real_flat,
                        &spec_imag_flat,
                        coefs,
                        self.last_num_frames,
                        t,
                        &mut out_real,
                        &mut out_imag,
                    );

                    let audio = self.stft_out[ch].inverse(&out_real, &out_imag);
                    for s in audio {
                        self.output_buf[ch].push_back(s);
                    }
                }
            }
        }
    }

    fn update_meters(
        &self,
        buffer: &mut AudioBuffer,
        n_samples: usize,
        ctx: &mut ProcessContext,
    ) {
        let mut input_peak = 0.0f32;
        let mut output_peak = 0.0f32;

        for i in 0..n_samples {
            let (inp, _) = buffer.io(0);
            let s = inp[i].abs();
            if s > input_peak {
                input_peak = s;
            }
        }
        for i in 0..n_samples {
            let (_, out) = buffer.io(0);
            let s = out[i].abs();
            if s > output_peak {
                output_peak = s;
            }
        }

        ctx.set_meter(UpallaParamsParamId::InputLevel, input_peak);
        ctx.set_meter(UpallaParamsParamId::OutputLevel, output_peak);
    }
}

impl PluginLogic for UpallaPlugin {
    fn reset(&mut self, sample_rate: f64, _max_block_size: usize) {
        self.params.set_sample_rate(sample_rate);
        self.spec_real_history.clear();
        self.spec_imag_history.clear();
        self.last_sent_frame = 0;
        self.pending_task = false;
        self.vad_grace_remaining = 0;
        if let Some(ref mut d) = self.cpu_denoiser {
            d.reset();
        }
    }

    fn process(
        &mut self,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        ctx: &mut ProcessContext,
    ) -> ProcessStatus {
        let num_channels = buffer.channels().min(2);
        self.reinit_channels(num_channels);

        if self.params.bypass.value() {
            return ProcessStatus::Normal;
        }

        let n_samples = buffer.num_samples();

        for i in 0..n_samples {
            for ch in 0..num_channels {
                let (inp, _out) = buffer.io(ch);
                self.input_buf[ch].push_back(inp[i]);
            }
        }

        for ch in 0..num_channels {
            let (_, out) = buffer.io(ch);
            for i in 0..n_samples {
                out[i] = 0.0;
            }
        }

        if self.cpu_denoiser.is_some() {
            self.process_cpu_fallback();
        }

        while self.input_buf[0].len() >= config::HOP_SIZE {
            let frame_bufs: Vec<Vec<f32>> = (0..num_channels)
                .map(|ch| {
                    (0..config::HOP_SIZE)
                        .map(|_| self.input_buf[ch].pop_front().unwrap_or(0.0))
                        .collect()
                })
                .collect();

            let mut spec_real_ch: Vec<Vec<f32>> = vec![vec![]; num_channels];
            let mut spec_imag_ch: Vec<Vec<f32>> = vec![vec![]; num_channels];

            for ch in 0..num_channels {
                spec_real_ch[ch] = vec![0.0f32; config::FREQ_BINS];
                spec_imag_ch[ch] = vec![0.0f32; config::FREQ_BINS];
                self.stft_in[ch].forward(
                    &frame_bufs[ch],
                    &mut spec_real_ch[ch],
                    &mut spec_imag_ch[ch],
                );
            }

            self.spec_real_history.push(spec_real_ch);
            self.spec_imag_history.push(spec_imag_ch);

            while self.spec_real_history.len() > 64 {
                self.spec_real_history.remove(0);
                self.spec_imag_history.remove(0);
                self.last_sent_frame = self.last_sent_frame.saturating_sub(1);
            }
        }

        self.try_apply_results(num_channels);
        self.send_to_worker_if_needed();
        self.apply_vad_gating();
        self.drain_output(buffer, n_samples);
        self.update_meters(buffer, n_samples, ctx);

        ProcessStatus::Normal
    }

    fn latency(&self) -> u32 {
        if self.cpu_denoiser.is_some() {
            config::TOTAL_LATENCY_SAMPLES as u32
        } else {
            config::TOTAL_LATENCY_SAMPLES as u32
        }
    }

    fn bus_layouts() -> Vec<BusLayout> {
        vec![BusLayout::stereo()]
    }

    fn supports_in_place() -> bool {
        false
    }

    fn editor(&self) -> Box<dyn Editor> {
        use truce_gui::IntoLayoutEditor;
        use truce_gui_types::layout::{knob, meter, toggle, widgets, GridLayout};

        GridLayout::build(vec![
            widgets(vec![
                knob(UpallaParamsParamId::Suppression, "Suppression"),
                knob(UpallaParamsParamId::VadThreshold, "VAD"),
                toggle(UpallaParamsParamId::Bypass, "Bypass"),
            ]),
            widgets(vec![
                meter::<u32>(
                    &[UpallaParamsParamId::InputLevel.into()],
                    "Input",
                ),
                meter::<u32>(
                    &[UpallaParamsParamId::OutputLevel.into()],
                    "Output",
                ),
            ]),
        ])
        .into_editor(&self.params)
    }

    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load_state(&mut self, _data: &[u8]) -> Result<(), StateLoadError> {
        Ok(())
    }
}
