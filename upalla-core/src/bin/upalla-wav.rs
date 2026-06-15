use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use upalla_core::denoiser::{Denoiser, StereoChunk, CHUNK};
use upalla_core::model::Model;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value_t = 100.)]
    atten_lim_db: f32,

    #[arg(long)]
    post_filter: bool,

    #[arg(long, default_value_t = 0.02)]
    post_filter_beta: f32,

    #[arg(long)]
    model: Option<PathBuf>,

    input_file: PathBuf,
    output_file: PathBuf,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default())
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    log::info!("Loading model...");
    let model = match &args.model {
        Some(path) => Model::Custom(path.clone()),
        None => Model::default(),
    };
    let mut denoiser = Denoiser::new(&model, 2)?;

    log::info!("Reading {}", args.input_file.display());
    let (left, right, sr) = upalla_core::wav::read_wav_stereo(&args.input_file)?;
    let n = left.len();
    let n_frames = n / CHUNK;
    let n_pad = n_frames * CHUNK;

    let mut enhanced_left = vec![0.0f32; n_pad];
    let mut enhanced_right = vec![0.0f32; n_pad];

    let t0 = Instant::now();
    for i in 0..n_frames {
        let start = i * CHUNK;
        let end = start + CHUNK;

        let mut sc = StereoChunk {
            left: [0.0; CHUNK],
            right: [0.0; CHUNK],
        };
        sc.left.copy_from_slice(&left[start..end]);
        sc.right.copy_from_slice(&right[start..end]);

        let out = denoiser.process_stereo(&sc)?;
        enhanced_left[start..end].copy_from_slice(&out.left);
        enhanced_right[start..end].copy_from_slice(&out.right);
    }

    let elapsed = t0.elapsed().as_secs_f32();
    let audio_len = n_pad as f32 / sr as f32;
    log::info!(
        "Processed {:.1}s in {:.1}s (RTF: {:.2})",
        audio_len,
        elapsed,
        elapsed / audio_len
    );

    log::info!("Writing {}", args.output_file.display());
    upalla_core::wav::write_wav_stereo(
        args.output_file.to_str().unwrap(),
        &enhanced_left,
        &enhanced_right,
        sr,
    )?;
    Ok(())
}
