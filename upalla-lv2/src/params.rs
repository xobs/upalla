use truce::prelude::*;

#[derive(Params)]
pub struct UpallaParams {
    #[param(
        name = "Suppression",
        range = "linear(0, 100)",
        default = 80.0,
        smooth = "exp(5)"
    )]
    pub suppression: FloatParam,

    #[param(
        name = "VAD Threshold",
        range = "linear(0, 100)",
        default = 50.0,
        smooth = "exp(5)"
    )]
    pub vad_threshold: FloatParam,

    #[param(name = "Bypass")]
    pub bypass: BoolParam,

    #[meter]
    pub input_level: MeterSlot,

    #[meter]
    pub output_level: MeterSlot,
}

impl UpallaParams {
    pub fn suppression_gain(&self) -> f32 {
        self.suppression.read().clamp(0.0, 100.0) / 100.0
    }

    pub fn vad_threshold_normalized(&self) -> f32 {
        self.vad_threshold.read().clamp(0.0, 100.0) / 100.0
    }
}
