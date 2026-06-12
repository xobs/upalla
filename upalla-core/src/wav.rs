use std::path::Path;

use hound::{WavReader, WavSpec, WavWriter};

pub fn read_wav_stereo(path: &Path) -> anyhow::Result<(Vec<f32>, Vec<f32>, u32)> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let sr = spec.sample_rate;
    let ch = spec.channels as usize;

    let samples: Vec<i16> = if spec.sample_format == hound::SampleFormat::Int {
        reader
            .samples::<i16>()
            .collect::<Result<Vec<i16>, _>>()
            .map_err(|e| anyhow::anyhow!("WAV read error: {e}"))?
    } else if spec.sample_format == hound::SampleFormat::Float {
        let f32_samples: Vec<f32> = reader
            .samples::<f32>()
            .collect::<Result<Vec<f32>, _>>()
            .map_err(|e| anyhow::anyhow!("WAV read error: {e}"))?;
        f32_samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect()
    } else {
        anyhow::bail!("Unsupported WAV sample format");
    };

    let n = samples.len();
    let mut left = Vec::with_capacity(n / ch);
    let mut right = Vec::with_capacity(n / ch);
    for i in 0..n / ch {
        left.push(samples[i * ch] as f32 / 32768.0);
        if ch >= 2 {
            right.push(samples[i * ch + 1] as f32 / 32768.0);
        } else {
            right.push(left[i]);
        }
    }

    Ok((left, right, sr))
}

pub fn write_wav_stereo(path: &str, left: &[f32], right: &[f32], sr: u32) -> anyhow::Result<()> {
    let spec = WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    let n = left.len().min(right.len());
    for i in 0..n {
        let sl = (left[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let sr_val = (right[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(sl)?;
        writer.write_sample(sr_val)?;
    }
    writer.finalize()?;
    Ok(())
}
