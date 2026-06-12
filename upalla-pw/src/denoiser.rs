use std::path::Path;

use anyhow::Result;
use df::tract::{DfParams, DfTract, RuntimeParams};

const CHUNK: usize = 480;

pub struct Denoiser {
    model: DfTract,
}

impl Denoiser {
    pub fn new(_model_dir: &Path) -> Result<Self> {
        let params = DfParams::default();
        let rp = RuntimeParams::default_with_ch(1)
            .with_atten_lim(100.0)
            .with_thresholds(-15.0, 35.0, 35.0);
        let model = DfTract::new(params, &rp)?;
        Ok(Denoiser { model })
    }

    pub fn process(&mut self, input: &[f32; CHUNK], output: &mut [f32; CHUNK]) -> Result<usize> {
        use ndarray::{ArrayView2, ArrayViewMut2, ShapeBuilder};
        let noisy_view = ArrayView2::from_shape((1, CHUNK).f(), input.as_slice())?;
        let mut enhanced = vec![0.0f32; CHUNK];
        let enh_view = ArrayViewMut2::from_shape((1, CHUNK).f(), &mut enhanced)?;
        self.model.process(noisy_view, enh_view)?;
        output.copy_from_slice(&enhanced);
        Ok(CHUNK)
    }

    pub fn reset(&mut self) {
        if let Err(e) = self.model.init() {
            log::error!("Failed to reset model: {e}");
        }
    }
}
