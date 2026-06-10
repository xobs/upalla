use crate::config;

#[derive(Clone)]
pub struct FeatureState {
    erb_ma: Vec<f32>,
    spec_ma: Vec<f32>,
}

impl FeatureState {
    pub fn new() -> Self {
        FeatureState {
            erb_ma: vec![0.0f32; config::ERB_BANDS],
            spec_ma: vec![0.0f32; config::DF_BINS],
        }
    }

    pub fn reset(&mut self) {
        self.erb_ma.fill(0.0);
        self.spec_ma.fill(0.0);
    }
}

pub fn extract_erb_features(
    real: &[f32],
    imag: &[f32],
    erb_fb: &[f32],
    state: &mut FeatureState,
) -> Vec<f32> {
    let mut erb_features = vec![0.0f32; config::ERB_BANDS];

    for b in 0..config::ERB_BANDS {
        let mut power = 0.0f32;
        for f in 0..config::FREQ_BINS {
            let re = real[f];
            let im = imag[f];
            power += (re * re + im * im) * erb_fb[f * config::ERB_BANDS + b];
        }
        let db = 10.0 * (power + 1e-10f32).log10();
        erb_features[b] = db;
    }

    for b in 0..config::ERB_BANDS {
        let x = erb_features[b];
        let ma = state.erb_ma[b];
        let new_ma = ma * 0.98 + x * 0.02;
        state.erb_ma[b] = new_ma;
        erb_features[b] = (x - new_ma) / 40.0;
    }

    erb_features
}

pub fn extract_spec_features(
    real: &[f32],
    imag: &[f32],
    state: &mut FeatureState,
) -> Vec<f32> {
    let mut spec_features = vec![0.0f32; config::DF_BINS * 2];

    for f in 0..config::DF_BINS {
        let re = real[f];
        let im = imag[f];
        let mag = (re * re + im * im).sqrt();

        let new_ma = state.spec_ma[f] * 0.98 + mag * 0.02;
        state.spec_ma[f] = new_ma;

        let scale = if new_ma > 1e-8 { 1.0 / (new_ma).sqrt() } else { 1.0 };
        spec_features[f] = re * scale;
        spec_features[config::DF_BINS + f] = im * scale;
    }

    spec_features
}
