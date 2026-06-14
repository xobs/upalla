use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone)]
pub enum Model {
    DeepFilterNet3,
    DeepFilterNet3Ll,
    Custom(PathBuf),
}

impl Default for Model {
    fn default() -> Self {
        Self::DeepFilterNet3
    }
}

impl Model {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::DeepFilterNet3 => Ok(include_bytes!(
                "../../../DeepFilterNet/models/DeepFilterNet3_onnx.tar.gz"
            )
            .to_vec()),
            Self::DeepFilterNet3Ll => Ok(include_bytes!(
                "../../../DeepFilterNet/models/DeepFilterNet3_ll_onnx.tar.gz"
            )
            .to_vec()),
            Self::Custom(path) => std::fs::read(path)
                .with_context(|| format!("Failed to read model from {}", path.display())),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::DeepFilterNet3 => "Standard",
            Self::DeepFilterNet3Ll => "Low Latency",
            Self::Custom(_) => "Custom",
        }
    }
}
