use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2, ShapeBuilder};

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value_t = 100.)]
    atten_lim_db: f32,

    #[arg(long)]
    post_filter: bool,

    #[arg(long, default_value_t = 0.02)]
    post_filter_beta: f32,

    input_file: PathBuf,
    output_file: PathBuf,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default())
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    log::info!("Loading model...");
    let params = DfParams::default();
    let rp = RuntimeParams::default_with_ch(1)
        .with_atten_lim(args.atten_lim_db)
        .with_thresholds(-15.0, 35.0, 35.0);
    let mut model = DfTract::new(params, &rp)?;
    if args.post_filter {
        model.set_pf_beta(args.post_filter_beta);
    }

    log::info!("Reading {}", args.input_file.display());
    let (left, right, sr) = upalla_core::wav::read_wav_stereo(&args.input_file)?;
    let n = left.len();
    let hop = model.hop_size;
    let n_frames = n / hop;
    let n_pad = n_frames * hop;

    let process_channel = |model: &mut DfTract, input: &[f32]| -> Result<Vec<f32>> {
        let mut output = vec![0.0f32; n_pad];
        let mut enhanced = vec![0.0f32; hop];
        for i in 0..n_frames {
            let start = i * hop;
            let noisy_view = ArrayView2::from_shape((1, hop).f(), &input[start..start + hop])?;
            let mut enh_view = ArrayViewMut2::from_shape((1, hop).f(), &mut enhanced)?;
            model.process(noisy_view, enh_view)?;
            output[start..start + hop].copy_from_slice(&enhanced);
        }
        Ok(output)
    };

    let t0 = Instant::now();
    let enhanced_left = process_channel(&mut model, &left)?;

    let params2 = DfParams::default();
    let rp2 = RuntimeParams::default_with_ch(1)
        .with_atten_lim(args.atten_lim_db)
        .with_thresholds(-15.0, 35.0, 35.0);
    let mut model2 = DfTract::new(params2, &rp2)?;
    if args.post_filter {
        model2.set_pf_beta(args.post_filter_beta);
    }
    let enhanced_right = process_channel(&mut model2, &right)?;

    let elapsed = t0.elapsed().as_secs_f32();
    let audio_len = n_pad as f32 / sr as f32;
    log::info!("Processed {:.1}s in {:.1}s (RTF: {:.2})", audio_len, elapsed, elapsed/audio_len);

    log::info!("Writing {}", args.output_file.display());
    upalla_core::wav::write_wav_stereo(
        args.output_file.to_str().unwrap(),
        &enhanced_left, &enhanced_right, sr,
    )?;
    Ok(())
}
