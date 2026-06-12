use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::Array2;

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
    let rp = RuntimeParams::default_with_ch(2)
        .with_atten_lim(args.atten_lim_db)
        .with_thresholds(-15.0, 35.0, 35.0)
        .with_mask_reduce(ReduceMask::MAX);
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

    let mut enhanced_left = vec![0.0f32; n_pad];
    let mut enhanced_right = vec![0.0f32; n_pad];

    let t0 = Instant::now();
    for i in 0..n_frames {
        let start = i * hop;
        let end = start + hop;

        let mut frame = Array2::<f32>::zeros((2, hop));
        frame.row_mut(0).as_slice_mut().unwrap().copy_from_slice(&left[start..end]);
        frame.row_mut(1).as_slice_mut().unwrap().copy_from_slice(&right[start..end]);

        let noisy_view = frame.view();
        let mut enhanced = Array2::<f32>::zeros((2, hop));
        let mut enh_view = enhanced.view_mut();

        model.process(noisy_view, enh_view)?;

        enhanced_left[start..end]
            .copy_from_slice(enhanced.row(0).as_slice().unwrap());
        enhanced_right[start..end]
            .copy_from_slice(enhanced.row(1).as_slice().unwrap());
    }

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
