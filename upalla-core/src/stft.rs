use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

use crate::config;

pub struct StftEngine {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    analysis_buf: Vec<f32>,
    synthesis_buf: Vec<f32>,
    fft_in: Vec<f32>,
    fft_out: Vec<rustfft::num_complex::Complex<f32>>,
    overlap_buf: Vec<f32>,
}

impl StftEngine {
    pub fn new(window: Vec<f32>) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(config::FFT_SIZE);
        let fft_out_len = config::FFT_SIZE / 2 + 1;

        StftEngine {
            fft,
            window,
            analysis_buf: vec![0.0f32; config::FFT_SIZE],
            synthesis_buf: vec![0.0f32; config::FFT_SIZE],
            fft_in: vec![0.0f32; config::FFT_SIZE],
            fft_out: vec![
                rustfft::num_complex::Complex { re: 0.0, im: 0.0 };
                fft_out_len
            ],
            overlap_buf: vec![0.0f32; config::FFT_SIZE],
        }
    }

    pub fn forward(
        &mut self,
        samples: &[f32],
        real: &mut [f32],
        imag: &mut [f32],
    ) {
        assert_eq!(samples.len(), config::HOP_SIZE);
        assert_eq!(real.len(), config::FREQ_BINS);
        assert_eq!(imag.len(), config::FREQ_BINS);

        self.analysis_buf.copy_within(config::HOP_SIZE.., 0);
        let dst = &mut self.analysis_buf[config::HOP_SIZE..];
        dst.copy_from_slice(samples);

        for i in 0..config::FFT_SIZE {
            self.fft_in[i] = self.analysis_buf[i] * self.window[i];
        }

        self.fft
            .process(&mut self.fft_in, &mut self.fft_out)
            .expect("FFT forward failed");

        let scale = 1.0 / (config::FFT_SIZE as f32).sqrt();
        for f in 0..config::FREQ_BINS {
            real[f] = self.fft_out[f].re * scale;
            imag[f] = self.fft_out[f].im * scale;
        }
    }

    pub fn inverse(&mut self, real: &[f32], imag: &[f32]) -> Vec<f32> {
        assert_eq!(real.len(), config::FREQ_BINS);
        assert_eq!(imag.len(), config::FREQ_BINS);

        let mut planner = RealFftPlanner::<f32>::new();
        let ifft = planner.plan_fft_inverse(config::FFT_SIZE);

        for f in 0..config::FREQ_BINS {
            self.fft_out[f].re = real[f];
            self.fft_out[f].im = imag[f];
        }

        ifft.process(&mut self.fft_out, &mut self.fft_in)
            .expect("FFT inverse failed");

        let scale = 1.0 / (config::FFT_SIZE as f32).sqrt();
        for i in 0..config::FFT_SIZE {
            self.fft_in[i] *= scale;
        }

        for i in 0..config::FFT_SIZE {
            self.fft_in[i] *= self.window[i];
        }

        for i in 0..config::FFT_SIZE {
            self.synthesis_buf[i] = self.overlap_buf[i] + self.fft_in[i];
        }

        let output: Vec<f32> = self.synthesis_buf[..config::HOP_SIZE].to_vec();

        self.overlap_buf
            .copy_within(config::HOP_SIZE.., 0);
        let overlap_tail = &mut self.overlap_buf[config::FFT_SIZE - config::HOP_SIZE..];
        overlap_tail.fill(0.0);

        let residual = &self.synthesis_buf[config::HOP_SIZE..];
        for (i, &v) in residual.iter().enumerate() {
            self.overlap_buf[i] += v;
        }

        output
    }

    pub fn reset(&mut self) {
        self.analysis_buf.fill(0.0);
        self.synthesis_buf.fill(0.0);
        self.fft_in.fill(0.0);
        self.fft_out.fill(rustfft::num_complex::Complex { re: 0.0, im: 0.0 });
        self.overlap_buf.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_silence() {
        let aux = crate::auxiliary::Auxiliary::new();
        let mut engine = StftEngine::new(aux.window);

        let input = vec![0.0f32; config::HOP_SIZE];
        let mut real = vec![0.0f32; config::FREQ_BINS];
        let mut imag = vec![0.0f32; config::FREQ_BINS];

        engine.forward(&input, &mut real, &mut imag);
        let output = engine.inverse(&real, &imag);

        for &s in &output {
            assert!(s.abs() < 1e-4, "silence roundtrip error: {s}");
        }
    }

    #[test]
    fn test_roundtrip_impulse() {
        let aux = crate::auxiliary::Auxiliary::new();
        let mut engine = StftEngine::new(aux.window);

        let mut found_impulse = false;
        for frame in 0..12 {
            let mut input = vec![0.0f32; config::HOP_SIZE];
            if frame == 3 {
                input[config::HOP_SIZE / 2] = 1.0;
            }

            let mut real = vec![0.0f32; config::FREQ_BINS];
            let mut imag = vec![0.0f32; config::FREQ_BINS];
            engine.forward(&input, &mut real, &mut imag);
            let output = engine.inverse(&real, &imag);

            if frame >= 4 && frame <= 5 {
                if output.iter().any(|&s| s.abs() > 0.1) {
                    found_impulse = true;
                }
            }
        }
        assert!(found_impulse, "impulse reconstruction failed");
    }
}
