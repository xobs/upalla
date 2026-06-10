use std::path::Path;

use anyhow::{Context, Result};
use ort::session::Session;

use crate::config;

pub struct OrtDenoiser {
    session: Session,
}

impl OrtDenoiser {
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .context("failed to create ONNX session builder")?
            .commit_from_file(model_path)
            .context("failed to load ONNX model")?;

        for input in &session.inputs {
            log::info!(
                "ONNX input: {} type={:?}",
                input.name,
                input.input_type
            );
        }
        for output in &session.outputs {
            log::info!(
                "ONNX output: {} type={:?}",
                output.name,
                output.output_type
            );
        }

        Ok(OrtDenoiser { session })
    }

    pub fn run(
        &mut self,
        feat_erb: &[f32],
        feat_spec: &[f32],
        num_frames: usize,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        let frames = num_frames;

        let erb_shape = [1i64, 1, frames as i64, config::ERB_BANDS as i64];
        let spec_shape = [1i64, 2, frames as i64, config::DF_BINS as i64];

        let erb_tensor = ort::value::Tensor::from_array(
            (erb_shape.as_slice(), feat_erb.to_vec()),
        )?;
        let spec_tensor = ort::value::Tensor::from_array(
            (spec_shape.as_slice(), feat_spec.to_vec()),
        )?;

        let outputs = self.session.run(ort::inputs![
            "feat_erb" => erb_tensor,
            "feat_spec" => spec_tensor,
        ])?;

        let (_shape, erb_mask_data): (_, &[f32]) =
            outputs["erb_mask"].try_extract_tensor()?;
        let (_shape, df_coefs_data): (_, &[f32]) =
            outputs["df_coefs"].try_extract_tensor()?;

        Ok((erb_mask_data.to_vec(), df_coefs_data.to_vec()))
    }
}
