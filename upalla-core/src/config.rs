pub const SAMPLE_RATE: usize = 48000;
pub const FFT_SIZE: usize = 960;
pub const HOP_SIZE: usize = 480;
pub const FREQ_BINS: usize = FFT_SIZE / 2 + 1; // 481
pub const ERB_BANDS: usize = 32;
pub const DF_BINS: usize = 96;
pub const DF_ORDER: usize = 5;
pub const DF_LOOKAHEAD: usize = 2;
pub const NB_ERB_FREQS: usize = 2;

pub const FRAME_SAMPLES: usize = HOP_SIZE;

pub const STFT_LATENCY_SAMPLES: usize = HOP_SIZE;
pub const DF_LOOKAHEAD_SAMPLES: usize = DF_ORDER * HOP_SIZE;
pub const WORKER_LATENCY_SAMPLES: usize = HOP_SIZE;
pub const TOTAL_LATENCY_SAMPLES: usize =
    STFT_LATENCY_SAMPLES + DF_LOOKAHEAD_SAMPLES + WORKER_LATENCY_SAMPLES;
pub const TOTAL_LATENCY_MS: f64 =
    TOTAL_LATENCY_SAMPLES as f64 / SAMPLE_RATE as f64 * 1000.0;

pub const ERB_ALPHA: f32 = 0.98;
pub const SPEC_ALPHA: f32 = 0.98;
