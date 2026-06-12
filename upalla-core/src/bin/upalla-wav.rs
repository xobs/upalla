use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use upalla_core::ort_tract::OrtDfTract;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value_t = 100.)]
    atten_lim_db: f32,

    input_file: PathBuf,
    output_file: PathBuf,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default())
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    log::info!("Loading model...");
    let config = upalla_core::load_config()?;
    let mut model = OrtDfTract::new(&config, 1)?;
    model.set_atten_lim(args.atten_lim_db);

    log::info!("Reading {}", args.input_file.display());
    let (left, right, sr) = upalla_core::wav::read_wav_stereo(&args.input_file)?;
    let n = left.len();
    let hop = model.hop_size;
    let n_frames = n / hop;
    let n_pad = n_frames * hop;

    let process_channel = |model: &mut OrtDfTract, input: &[f32]| -> Result<Vec<f32>> {
        let mut output = vec![0.0f32; n_pad];
        for i in 0..n_frames {
            let start = i * hop;
            let noisy = vec![input[start..start + hop].to_vec()];
            let mut enh = vec![vec![0.0f32; hop]];
            model.process(&noisy, &mut enh)?;
            output[start..start + hop].copy_from_slice(&enh[0]);
        }
        Ok(output)
    };

    let t0 = Instant::now();
    let enhanced_left = process_channel(&mut model, &left)?;

    let config2 = upalla_core::load_config()?;
    let mut model2 = OrtDfTract::new(&config2, 1)?;
    model2.set_atten_lim(args.atten_lim_db);
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
