use std::path::Path;

use anyhow::Result;
use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::Array2;

const CHUNK: usize = 480;

pub struct StereoChunk {
    pub left: [f32; CHUNK],
    pub right: [f32; CHUNK],
}

pub struct Denoiser {
    model: DfTract,
}

impl Denoiser {
    pub fn new(_model_dir: &Path) -> Result<Self> {
        let params = DfParams::default();
        let rp = RuntimeParams::default_with_ch(2)
            .with_atten_lim(100.0)
            .with_thresholds(-15.0, 35.0, 35.0)
            .with_mask_reduce(ReduceMask::MAX);
        let model = DfTract::new(params, &rp)?;
        Ok(Denoiser { model })
    }

    pub fn process(&mut self, input: &StereoChunk) -> Result<StereoChunk> {
        let mut frame = Array2::<f32>::zeros((2, CHUNK));
        frame.row_mut(0).as_slice_mut().unwrap().copy_from_slice(&input.left);
        frame.row_mut(1).as_slice_mut().unwrap().copy_from_slice(&input.right);

        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((2, CHUNK));
        let mut enh_view = enhanced.view_mut();

        self.model.process(noisy_view, enh_view)?;

        let mut out = StereoChunk { left: [0.0; CHUNK], right: [0.0; CHUNK] };
        out.left.copy_from_slice(enhanced.row(0).as_slice().unwrap());
        out.right.copy_from_slice(enhanced.row(1).as_slice().unwrap());
        Ok(out)
    }

    pub fn reset(&mut self) {
        if let Err(e) = self.model.init() {
            log::error!("Failed to reset model: {e}");
        }
    }
}
