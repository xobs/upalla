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
    params: DfParams,
    channels: usize,
}

fn runtime_params(channels: usize) -> RuntimeParams {
    RuntimeParams::default_with_ch(channels)
        .with_atten_lim(100.0)
        .with_thresholds(-15.0, 35.0, 35.0)
        .with_mask_reduce(ReduceMask::MAX)
}

impl Denoiser {
    pub fn new(model: &Model, channels: usize) -> Result<Self> {
        let params = model.to_params()?;
        let model = DfTract::new(params.clone(), &runtime_params(channels))?;
        Ok(Denoiser {
            model,
            params,
            channels,
        })
    }

    pub fn process_stereo(&mut self, input: &StereoChunk) -> Result<StereoChunk> {
        let mut frame = Array2::<f32>::zeros((2, CHUNK));
        frame
            .row_mut(0)
            .as_slice_mut()
            .unwrap()
            .copy_from_slice(&input.left);
        frame
            .row_mut(1)
            .as_slice_mut()
            .unwrap()
            .copy_from_slice(&input.right);
        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((2, CHUNK));
        let enh_view = enhanced.view_mut();
        self.model.process(noisy_view, enh_view)?;
        let mut out = StereoChunk {
            left: [0.0; CHUNK],
            right: [0.0; CHUNK],
        };
        out.left
            .copy_from_slice(enhanced.row(0).as_slice().unwrap());
        out.right
            .copy_from_slice(enhanced.row(1).as_slice().unwrap());
        Ok(out)
    }

    pub fn process_mono(
        &mut self,
        input: &[f32; CHUNK],
        output: &mut [f32; CHUNK],
    ) -> Result<usize> {
        let mut frame = Array2::<f32>::zeros((1, CHUNK));
        frame
            .row_mut(0)
            .as_slice_mut()
            .unwrap()
            .copy_from_slice(input);
        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((1, CHUNK));
        let enh_view = enhanced.view_mut();
        self.model.process(noisy_view, enh_view)?;
        output.copy_from_slice(enhanced.row(0).as_slice().unwrap());
        Ok(CHUNK)
    }

    /// Drops all internal model state by rebuilding the network.
    ///
    /// Note that `DfTract::init()` cannot be used for this: it clears
    /// `rolling_spec_buf_y` but *not* `rolling_spec_buf_x`, and `DfTract::new()`
    /// has already called it once. Calling it again therefore grows the noisy
    /// spectrum buffer, which permanently misaligns the deep-filter stage and the
    /// upper-frequency mix — audible as a robotic, metallic voice.
    ///
    /// Rebuilding reloads and re-optimises the ONNX graph, so this takes on the
    /// order of a second. It is not safe to call from a real-time audio path;
    /// the rolling buffers flush themselves within `df_order` frames (~50 ms)
    /// anyway, so a stream restart does not need an explicit reset.
    pub fn reset(&mut self) -> Result<()> {
        self.model = DfTract::new(self.params.clone(), &runtime_params(self.channels))?;
        Ok(())
    }
}
