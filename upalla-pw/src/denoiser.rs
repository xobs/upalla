use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use df::tract::{DfParams, DfTract, RuntimeParams};

const CHUNK: usize = 480;

pub struct Denoiser {
    models: Vec<DfTract>,
}

impl Denoiser {
    pub fn new(model_dir: &Path) -> Result<Self> {
        let dfp = match find_model_tar(model_dir) {
            Ok(tar_path) => DfParams::new(tar_path)
                .context("Failed to load model from tar")?,
            Err(_) => {
                log::info!("No model tar at {:?}, using embedded model", model_dir);
                DfParams::default()
            }
        };

        let rp = RuntimeParams::default_with_ch(1)
            .with_atten_lim(6.0)
            .with_thresholds(-15.0, 35.0, 35.0);

        // Two channels, each with its own model
        let left_model = DfTract::new(dfp.clone(), &rp)?;
        let right_model = DfTract::new(dfp, &rp)?;

        Ok(Denoiser {
            models: vec![left_model, right_model],
        })
    }

    pub fn process(&mut self, input: &[f32; CHUNK], output: &mut [f32; CHUNK]) -> Result<usize> {
        use ndarray::{ArrayView2, ArrayViewMut2, ShapeBuilder};

        // Process with first model (mono)
        let model = &mut self.models[0];
        let noisy_view = ArrayView2::from_shape((1, CHUNK).f(), input.as_slice())
            .context("Failed to create noisy view")?;
        let mut enhanced = vec![0.0f32; CHUNK];
        let mut enh_view = ArrayViewMut2::from_shape((1, CHUNK).f(), &mut enhanced)
            .context("Failed to create enhanced view")?;

        let _lsnr = model.process(noisy_view, enh_view)?;
        output.copy_from_slice(&enhanced);

        Ok(CHUNK)
    }

    pub fn reset(&mut self) {
        for model in &mut self.models {
            if let Err(e) = model.init() {
                log::error!("Failed to reset model: {e}");
            }
        }
    }
}

fn find_model_tar(dir: &Path) -> Result<PathBuf> {
    let candidates = [
        dir.join("DeepFilterNet3_onnx.tar.gz"),
        dir.join("DeepFilterNet3_ll_onnx.tar.gz"),
        dir.join("model.tar.gz"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!("No model tar found in {:?}", dir)
}
