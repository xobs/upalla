use std::path::Path;

use hound::{WavReader, WavSpec, WavWriter};

pub fn read_wav_stereo(path: &Path) -> anyhow::Result<(Vec<f32>, Vec<f32>, u32)> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let sr = spec.sample_rate;
    let ch = spec.channels as usize;

    let (left, right): (Vec<f32>, Vec<f32>) = if spec.sample_format == hound::SampleFormat::Int {
        let samples: Vec<i16> = reader
            .samples::<i16>()
            .collect::<Result<Vec<i16>, _>>()
            .map_err(|e| anyhow::anyhow!("WAV read error: {e}"))?;
        let n = samples.len();
        let mut l = Vec::with_capacity(n / ch);
        let mut r = Vec::with_capacity(n / ch);
        for i in 0..n / ch {
            l.push(samples[i * ch] as f32 / 32767.0);
            r.push(if ch >= 2 {
                samples[i * ch + 1] as f32 / 32767.0
            } else {
                l[i]
            });
        }
        (l, r)
    } else if spec.sample_format == hound::SampleFormat::Float {
        let samples: Vec<f32> = reader
            .samples::<f32>()
            .collect::<Result<Vec<f32>, _>>()
            .map_err(|e| anyhow::anyhow!("WAV read error: {e}"))?;
        let n = samples.len();
        let mut l = Vec::with_capacity(n / ch);
        let mut r = Vec::with_capacity(n / ch);
        for i in 0..n / ch {
            l.push(samples[i * ch]);
            r.push(if ch >= 2 { samples[i * ch + 1] } else { l[i] });
        }
        (l, r)
    } else {
        anyhow::bail!("Unsupported WAV sample format");
    };

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
