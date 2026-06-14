use anyhow::Result;
use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::Array2;

use crate::model::Model;

pub const CHUNK: usize = 480;

pub struct StereoChunk {
    pub left: [f32; CHUNK],
    pub right: [f32; CHUNK],
}

pub struct Denoiser {
    model: DfTract,
}

impl Denoiser {
    pub fn new(model: &Model, channels: usize) -> Result<Self> {
        let bytes = model.to_bytes()?;
        let params = DfParams::from_bytes(&bytes)?;
        let rp = RuntimeParams::default_with_ch(channels)
            .with_atten_lim(100.0)
            .with_thresholds(-15.0, 35.0, 35.0)
            .with_mask_reduce(ReduceMask::MAX);
        let model = DfTract::new(params, &rp)?;
        Ok(Denoiser { model })
    }

    pub fn process_stereo(&mut self, input: &StereoChunk) -> Result<StereoChunk> {
        let mut frame = Array2::<f32>::zeros((2, CHUNK));
        frame.row_mut(0).as_slice_mut().unwrap().copy_from_slice(&input.left);
        frame.row_mut(1).as_slice_mut().unwrap().copy_from_slice(&input.right);
        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((2, CHUNK));
        let enh_view = enhanced.view_mut();
        self.model.process(noisy_view, enh_view)?;
        let mut out = StereoChunk { left: [0.0; CHUNK], right: [0.0; CHUNK] };
        out.left.copy_from_slice(enhanced.row(0).as_slice().unwrap());
        out.right.copy_from_slice(enhanced.row(1).as_slice().unwrap());
        Ok(out)
    }

    pub fn process_mono(&mut self, input: &[f32; CHUNK], output: &mut [f32; CHUNK]) -> Result<usize> {
        let mut frame = Array2::<f32>::zeros((1, CHUNK));
        frame.row_mut(0).as_slice_mut().unwrap().copy_from_slice(input);
        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((1, CHUNK));
        let enh_view = enhanced.view_mut();
        self.model.process(noisy_view, enh_view)?;
        output.copy_from_slice(enhanced.row(0).as_slice().unwrap());
        Ok(CHUNK)
    }

    pub fn reset(&mut self) {
        if let Err(e) = self.model.init() {
            log::error!("Failed to reset model: {e}");
        }
    }
}
