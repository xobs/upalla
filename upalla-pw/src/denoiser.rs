use std::path::Path;

use anyhow::Result;
use upalla_core::ort_tract::OrtDfTract;

const CHUNK: usize = 480;

pub struct Denoiser {
    model: OrtDfTract,
}

impl Denoiser {
    pub fn new(_model_dir: &Path) -> Result<Self> {
        let config = upalla_core::load_config()?;
        let mut model = OrtDfTract::new(&config, 1)?;
        model.set_atten_lim(100.0);
        Ok(Denoiser { model })
    }

    pub fn process(&mut self, input: &[f32; CHUNK], output: &mut [f32; CHUNK]) -> Result<usize> {
        let noisy = vec![input.to_vec()];
        let mut enhanced = vec![vec![0.0f32; CHUNK]];
        self.model.process(&noisy, &mut enhanced)?;
        output.copy_from_slice(&enhanced[0]);
        Ok(CHUNK)
    }

    pub fn reset(&mut self) {
        if let Err(e) = self.model.init() {
            log::error!("Failed to reset model: {e}");
        }
    }
}
