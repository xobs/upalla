pub mod auxiliary;
pub mod config;
pub mod features;
pub mod inference;
pub mod postprocess;
pub mod stft;

use std::collections::VecDeque;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::auxiliary::Auxiliary;
use crate::features::{extract_erb_features, extract_spec_features, FeatureState};
use crate::inference::OrtDenoiser;
use crate::postprocess::{apply_deep_filter, apply_erb_mask};
use crate::stft::StftEngine;

pub struct Denoiser {
    stft: StftEngine,
    aux: Auxiliary,
    model: OrtDenoiser,
    feat_state: FeatureState,
    input_buf: VecDeque<f32>,
    spec_real_history: Vec<Vec<f32>>,
    spec_imag_history: Vec<Vec<f32>>,
    output_buf: VecDeque<f32>,
}

impl Denoiser {
    pub fn new(model_path: PathBuf) -> Result<Self> {
        let aux = Auxiliary::new();
        let stft = StftEngine::new(aux.window.clone());
        let model = OrtDenoiser::new(&model_path)
            .context("failed to initialize ONNX denoiser")?;
        let feat_state = FeatureState::new();

        Ok(Denoiser {
            stft,
            aux,
            model,
            feat_state,
            input_buf: VecDeque::with_capacity(config::HOP_SIZE * 4),
            spec_real_history: Vec::new(),
            spec_imag_history: Vec::new(),
            output_buf: VecDeque::with_capacity(config::HOP_SIZE * 4),
        })
    }

    pub fn latency_samples(&self) -> usize {
        config::TOTAL_LATENCY_SAMPLES
    }

    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<usize> {
        for &s in input {
            self.input_buf.push_back(s);
        }

        let mut produced = 0;

        while self.input_buf.len() >= config::HOP_SIZE {
            let mut frame = vec![0.0f32; config::HOP_SIZE];
            for i in 0..config::HOP_SIZE {
                frame[i] = self.input_buf.pop_front().unwrap_or(0.0);
            }

            let mut real = vec![0.0f32; config::FREQ_BINS];
            let mut imag = vec![0.0f32; config::FREQ_BINS];
            self.stft.forward(&frame, &mut real, &mut imag);

            self.spec_real_history.push(real.clone());
            self.spec_imag_history.push(imag.clone());

            if self.spec_real_history.len() >= 32 {
                self.process_batch()?;
            }

            if self.spec_real_history.len() > 64 {
                self.spec_real_history.remove(0);
                self.spec_imag_history.remove(0);
            }
        }

        while produced < output.len() && !self.output_buf.is_empty() {
            if let Some(s) = self.output_buf.pop_front() {
                if produced < output.len() {
                    output[produced] = s;
                    produced += 1;
                }
            }
        }

        Ok(produced)
    }

    fn process_batch(&mut self) -> Result<()> {
        let num_frames = self.spec_real_history.len();
        if num_frames == 0 {
            return Ok(());
        }

        let mut feat_erb_all = Vec::with_capacity(num_frames * config::ERB_BANDS);
        let mut feat_spec_all =
            Vec::with_capacity(num_frames * config::DF_BINS * 2);

        for t in 0..num_frames {
            let erb = extract_erb_features(
                &self.spec_real_history[t],
                &self.spec_imag_history[t],
                &self.aux.erb_fb,
                &mut self.feat_state,
            );
            let spec = extract_spec_features(
                &self.spec_real_history[t],
                &self.spec_imag_history[t],
                &mut self.feat_state,
            );

            feat_erb_all.extend_from_slice(&erb);
            feat_spec_all.extend_from_slice(&spec);
        }

        let (erb_mask, df_coefs) =
            self.model.run(&feat_erb_all, &feat_spec_all, num_frames)?;

        let oldest_frame = self.spec_real_history.len().saturating_sub(num_frames);

        for t in 0..num_frames {
            apply_erb_mask(
                &mut self.spec_real_history[t],
                &mut self.spec_imag_history[t],
                &erb_mask,
                &self.aux.erb_inv_fb,
                t,
            );
        }

        for t in 0..num_frames {
            if self.spec_real_history.len() > oldest_frame + 2 {
                let mut out_real = vec![0.0f32; config::FREQ_BINS];
                let mut out_imag = vec![0.0f32; config::FREQ_BINS];

                apply_deep_filter(
                    &self.spec_real_history,
                    &self.spec_imag_history,
                    &df_coefs,
                    num_frames,
                    t,
                    &mut out_real,
                    &mut out_imag,
                );

                let audio = self.stft.inverse(&out_real, &out_imag);
                for s in audio {
                    self.output_buf.push_back(s);
                }
            }
        }

        Ok(())
    }

    pub fn process_batch_direct(
        &mut self,
        spec_real: &[Vec<f32>],
        spec_imag: &[Vec<f32>],
        num_frames: usize,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        let mut feat_erb_all = Vec::with_capacity(num_frames * config::ERB_BANDS);
        let mut feat_spec_all =
            Vec::with_capacity(num_frames * config::DF_BINS * 2);

        for t in 0..num_frames {
            let erb = extract_erb_features(
                &spec_real[t],
                &spec_imag[t],
                &self.aux.erb_fb,
                &mut self.feat_state,
            );
            let spec = extract_spec_features(
                &spec_real[t],
                &spec_imag[t],
                &mut self.feat_state,
            );
            feat_erb_all.extend_from_slice(&erb);
            feat_spec_all.extend_from_slice(&spec);
        }

        self.model
            .run(&feat_erb_all, &feat_spec_all, num_frames)
    }

    pub fn reset(&mut self) {
        self.stft.reset();
        self.feat_state.reset();
        self.input_buf.clear();
        self.spec_real_history.clear();
        self.spec_imag_history.clear();
        self.output_buf.clear();
    }
}
