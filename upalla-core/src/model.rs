use std::path::PathBuf;

use anyhow::{Context, Result};
use df::tract::DfParams;

#[derive(Clone, Default)]
pub enum Model {
    /// The standard DeepFilterNet3 model bundled with the `df` crate.
    #[default]
    DeepFilterNet3,
    /// A model loaded from a `*_onnx.tar.gz` archive on disk.
    Custom(PathBuf),
}

impl Model {
    pub fn to_params(&self) -> Result<DfParams> {
        match self {
            Self::DeepFilterNet3 => Ok(DfParams::default()),
            Self::Custom(path) => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("Failed to read model from {}", path.display()))?;
                DfParams::from_bytes(&bytes)
                    .with_context(|| format!("Failed to parse model at {}", path.display()))
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::DeepFilterNet3 => "Standard",
            Self::Custom(_) => "Custom",
        }
    }
}
