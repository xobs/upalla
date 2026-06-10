pub struct Auxiliary {
    pub erb_fb: Vec<f32>,
    pub erb_inv_fb: Vec<f32>,
    pub window: Vec<f32>,
}

impl Auxiliary {
    pub fn new() -> Self {
        let erb_fb = compute_erb_fb();
        let erb_inv_fb = compute_erb_inv_fb(&erb_fb);
        let window = compute_vorbis_window();
        Auxiliary {
            erb_fb,
            erb_inv_fb,
            window,
        }
    }
}

fn hz_to_erb(f: f32) -> f32 {
    9.265 * (1.0 + f / 228.8).ln()
}

fn erb_to_hz(e: f32) -> f32 {
    228.8 * ((e / 9.265).exp() - 1.0)
}

fn compute_erb_fb() -> Vec<f32> {
    use crate::config;

    let n_freqs = config::FREQ_BINS;
    let n_bands = config::ERB_BANDS;
    let sr = config::SAMPLE_RATE as f32;

    let erb_min = hz_to_erb(20.0);
    let erb_max = hz_to_erb(sr / 2.0);

    let mut centers = Vec::with_capacity(n_bands + 2);
    for i in 0..(n_bands + 2) {
        let e = erb_min + (erb_max - erb_min) * i as f32 / (n_bands + 1) as f32;
        centers.push(erb_to_hz(e));
    }

    let freqs: Vec<f32> = (0..n_freqs)
        .map(|i| sr / 2.0 * i as f32 / (n_freqs - 1) as f32)
        .collect();

    let mut fb = vec![0.0f32; n_freqs * n_bands];

    for b in 0..n_bands {
        let lower = centers[b];
        let center = centers[b + 1];
        let upper = centers[b + 2];

        for (f_idx, &f) in freqs.iter().enumerate() {
            let val = if f > lower && f <= center {
                (f - lower) / (center - lower)
            } else if f > center && f < upper {
                (upper - f) / (upper - center)
            } else {
                0.0
            };
            fb[f_idx * n_bands + b] = val;
        }
    }

    for b in 0..n_bands {
        let col_sum: f32 = (0..n_freqs).map(|f| fb[f * n_bands + b]).sum();
        if col_sum > 0.0 {
            let inv = 1.0 / col_sum;
            for f in 0..n_freqs {
                fb[f * n_bands + b] *= inv;
            }
        }
    }

    fb
}

fn compute_erb_inv_fb(erb_fb: &[f32]) -> Vec<f32> {
    let n_freqs = crate::config::FREQ_BINS;
    let n_bands = crate::config::ERB_BANDS;

    let mut inv = vec![0.0f32; n_bands * n_freqs];

    for b in 0..n_bands {
        let row_sum: f32 = (0..n_freqs).map(|f| erb_fb[f * n_bands + b]).sum();
        if row_sum > 0.0 {
            let scale = 1.0 / row_sum;
            for f in 0..n_freqs {
                inv[b * n_freqs + f] = erb_fb[f * n_bands + b] * scale;
            }
        }
    }

    inv
}

fn compute_vorbis_window() -> Vec<f32> {
    use std::f32::consts::PI;

    let n = crate::config::FFT_SIZE;
    let mut window = vec![0.0f32; n];

    for i in 0..n {
        let x = PI * i as f32 / n as f32;
        let sin_x = x.sin();
        window[i] = (PI / 2.0 * sin_x * sin_x).sin();
    }

    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_erb_fb_shape() {
        let aux = Auxiliary::new();
        assert_eq!(aux.erb_fb.len(), 481 * 32);
        assert_eq!(aux.erb_inv_fb.len(), 32 * 481);
        assert_eq!(aux.window.len(), 960);
    }

    #[test]
    fn test_vorbis_window_range() {
        let aux = Auxiliary::new();
        let n = aux.window.len();
        assert!(aux.window[0] < 0.01, "window should start near 0");
        assert!(aux.window[n / 2] > 0.9, "window should peak near 1");
        assert!(aux.window[n - 1] < 0.01, "window should end near 0");
    }

    #[test]
    fn test_erb_fb_normalized() {
        let aux = Auxiliary::new();
        for b in 0..32 {
            let sum: f32 = (0..481).map(|f| aux.erb_fb[f * 32 + b]).sum();
            assert!((sum - 1.0).abs() < 0.15, "band {b} sum = {sum}");
        }
    }
}
