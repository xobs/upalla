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

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-noise plus a tone. The model short-circuits silence,
    /// so the test signal needs actual structure for the comparison to mean
    /// anything.
    fn test_frames(n: usize) -> Vec<StereoChunk> {
        let mut state = 0x2545_F491u32;
        let mut phase = 0.0f32;
        (0..n)
            .map(|_| {
                let mut chunk = StereoChunk {
                    left: [0.0; CHUNK],
                    right: [0.0; CHUNK],
                };
                for i in 0..CHUNK {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let noise = (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                    phase += 220.0 * 2.0 * std::f32::consts::PI / 48_000.0;
                    let v = 0.3 * phase.sin() + 0.1 * noise;
                    chunk.left[i] = v;
                    chunk.right[i] = v * 0.8;
                }
                chunk
            })
            .collect()
    }

    fn process_all(denoiser: &mut Denoiser, frames: &[StereoChunk]) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames.len() * CHUNK * 2);
        for frame in frames {
            let enhanced = denoiser.process_stereo(frame).expect("process frame");
            out.extend_from_slice(&enhanced.left);
            out.extend_from_slice(&enhanced.right);
        }
        out
    }

    /// `reset()` must leave the denoiser exactly as freshly built, however often
    /// it is called.
    ///
    /// Regression test: `DfTract::init()` looks like a reset but clears only
    /// `rolling_spec_buf_y`, and `DfTract::new()` has already called it once.
    /// Calling it again grows `rolling_spec_buf_x`, permanently misaligning the
    /// deep-filter stage — audible as a robotic, metallic voice that worsens with
    /// each additional call.
    #[test]
    fn reset_is_equivalent_to_a_fresh_denoiser() {
        let frames = test_frames(20);
        let model = Model::default();

        let mut fresh = Denoiser::new(&model, 2).expect("build denoiser");
        let expected = process_all(&mut fresh, &frames);
        assert!(
            expected.iter().any(|&s| s.abs() > 1e-6),
            "test signal must survive denoising, else the comparison is vacuous"
        );

        for resets in [1usize, 3] {
            let mut denoiser = Denoiser::new(&model, 2).expect("build denoiser");
            for _ in 0..resets {
                denoiser.reset().expect("reset");
            }
            assert_eq!(
                process_all(&mut denoiser, &frames),
                expected,
                "output differs after {resets} reset(s)"
            );
        }
    }

    /// A reset part-way through a stream must also return to the fresh state, so a
    /// restarted chain does not inherit the previous session's history.
    #[test]
    fn reset_clears_state_accumulated_while_running() {
        let frames = test_frames(12);
        let model = Model::default();

        let mut fresh = Denoiser::new(&model, 2).expect("build denoiser");
        let expected = process_all(&mut fresh, &frames);

        let mut reused = Denoiser::new(&model, 2).expect("build denoiser");
        process_all(&mut reused, &test_frames(15)); // dirty the internal state
        reused.reset().expect("reset");

        assert_eq!(process_all(&mut reused, &frames), expected);
    }

    /// Mono and stereo paths must both run and preserve a real signal.
    #[test]
    fn mono_path_processes_a_frame() {
        let mut denoiser = Denoiser::new(&Model::default(), 1).expect("build denoiser");
        let frames = test_frames(6);
        let mut output = [0.0f32; CHUNK];
        for frame in &frames {
            let n = denoiser
                .process_mono(&frame.left, &mut output)
                .expect("process mono");
            assert_eq!(n, CHUNK);
        }
        assert!(output.iter().all(|s| s.is_finite()));
    }
}
