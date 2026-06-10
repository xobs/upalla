use crate::config;

pub fn apply_erb_mask(
    real: &mut [f32],
    imag: &mut [f32],
    erb_mask: &[f32],
    erb_inv_fb: &[f32],
    frame_idx: usize,
) {
    let mask_offset = frame_idx * config::ERB_BANDS;

    for f in 0..config::FREQ_BINS {
        let mut gain = 0.0f32;
        for b in 0..config::ERB_BANDS {
            gain += erb_mask[mask_offset + b] * erb_inv_fb[b * config::FREQ_BINS + f];
        }
        real[f] *= gain;
        imag[f] *= gain;
    }
}

pub fn apply_deep_filter(
    spec_real: &[Vec<f32>],
    spec_imag: &[Vec<f32>],
    df_coefs: &[f32],
    num_frames: usize,
    frame_idx: usize,
    out_real: &mut [f32],
    out_imag: &mut [f32],
) {
    let pad_before = config::DF_ORDER - 1 - config::DF_LOOKAHEAD;

    for f in 0..config::DF_BINS {
        let mut acc_re = 0.0f32;
        let mut acc_im = 0.0f32;

        for n in 0..config::DF_ORDER {
            let src_t = frame_idx as isize + n as isize - pad_before as isize;

            if src_t >= 0 && src_t < num_frames as isize {
                let src_t = src_t as usize;
                let x_re = spec_real[src_t][f];
                let x_im = spec_imag[src_t][f];

                let coef_base = ((n * num_frames + frame_idx) * config::DF_BINS + f) * 2;
                let w_re = df_coefs[coef_base];
                let w_im = df_coefs[coef_base + 1];

                acc_re += x_re * w_re - x_im * w_im;
                acc_im += x_re * w_im + x_im * w_re;
            }
        }

        out_real[f] = acc_re;
        out_imag[f] = acc_im;
    }

    for f in config::DF_BINS..config::FREQ_BINS {
        out_real[f] = spec_real[frame_idx][f];
        out_imag[f] = spec_imag[frame_idx][f];
    }
}

pub fn erb_mask_to_gain(
    erb_mask: &[f32],
    erb_inv_fb: &[f32],
    frame_idx: usize,
) -> Vec<f32> {
    let mask_offset = frame_idx * config::ERB_BANDS;
    let mut gain = vec![0.0f32; config::FREQ_BINS];

    for f in 0..config::FREQ_BINS {
        for b in 0..config::ERB_BANDS {
            gain[f] += erb_mask[mask_offset + b] * erb_inv_fb[b * config::FREQ_BINS + f];
        }
    }

    gain
}
