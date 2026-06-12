use std::collections::VecDeque;

use anyhow::{Result, bail};
use itertools::izip;
use ort::execution_providers::CPUExecutionProvider;
use ort::inputs;
use ort::memory::Allocator;
use ort::session::Session;
use ort::value::{Tensor, DynValue};

const ENC_ONNX: &[u8] = include_bytes!("../../models/enc.onnx");
const ERB_DEC_ONNX: &[u8] = include_bytes!("../../models/erb_dec.onnx");
const DF_DEC_ONNX: &[u8] = include_bytes!("../../models/df_dec.onnx");

const GRU_H_SIZE: usize = 512;

/// Deep Filtering formula — matches df::tract::df exactly.
fn df_fn(
    spec: &[Vec<df::Complex32>],
    coefs: &[Vec<df::Complex32>],
    nb_df: usize,
    df_order: usize,
    spec_out: &mut [df::Complex32],
) {
    spec_out[..nb_df].fill(df::Complex32::default());
    for k in 0..df_order {
        let s = &spec[k];
        let c = &coefs[k];
        for (o, (&s_bin, &c_bin)) in izip!(
            spec_out[..nb_df].iter_mut(),
            izip!(s[..nb_df].iter(), c[..nb_df].iter()),
        ) {
            *o += s_bin * c_bin;
        }
    }
}

/// Copy of df::tract::calc_norm_alpha.
fn calc_norm_alpha(sr: usize, hop_size: usize, tau: f32) -> f32 {
    let dt = hop_size as f32 / sr as f32;
    let alpha = f32::exp(-dt / tau);
    let mut a = 1.0;
    let mut precision = 3;
    while a >= 1.0 {
        a = (alpha * 10i32.pow(precision) as f32).round() / 10i32.pow(precision) as f32;
        precision += 1;
    }
    a
}

fn tensor_from_data(alloc: &Allocator, shape: &[usize], data: &[f32]) -> Result<DynValue> {
    let mut t = Tensor::<f32>::new(alloc, shape.to_vec())?;
    let (_, buf) = t.try_extract_tensor_mut::<f32>()?;
    buf.copy_from_slice(data);
    Ok(t.into_dyn())
}

pub struct OrtDfTract {
    enc: Session,
    erb_dec: Session,
    df_dec: Session,
    allocator: Allocator,

    enc_gru_h: Vec<f32>,
    erb_gru_h: Vec<f32>,
    erb_gru2_h: Vec<f32>,
    df_gru_h: Vec<f32>,
    df_gru2_h: Vec<f32>,
    df_gru3_h: Vec<f32>,

    pub lookahead: usize,
    pub sr: usize,
    pub ch: usize,
    pub fft_size: usize,
    pub hop_size: usize,
    nb_erb: usize,
    nb_df: usize,
    n_freqs: usize,
    df_order: usize,
    alpha: f32,

    min_db_thresh: f32,
    max_db_erb_thresh: f32,
    max_db_df_thresh: f32,
    atten_lim: Option<f32>,

    rolling_spec_buf_y: VecDeque<Vec<Vec<df::Complex32>>>,
    rolling_spec_buf_x: VecDeque<Vec<Vec<df::Complex32>>>,
    df_states: Vec<df::DFState>,

    post_filter: bool,
    post_filter_beta: f32,
    skip_counter: usize,
}

impl OrtDfTract {
    pub fn new(config: &ini::Ini, ch: usize) -> Result<Self> {
        let model_cfg = config.section(Some("deepfilternet")).unwrap();
        let df_cfg = config.section(Some("df")).unwrap();

        let enc = Session::builder()?
            .with_execution_providers([CPUExecutionProvider::default().build()])?
            .commit_from_memory(ENC_ONNX)?;
        let erb_dec = Session::builder()?
            .with_execution_providers([CPUExecutionProvider::default().build()])?
            .commit_from_memory(ERB_DEC_ONNX)?;
        let df_dec = Session::builder()?
            .with_execution_providers([CPUExecutionProvider::default().build()])?
            .commit_from_memory(DF_DEC_ONNX)?;

        let sr: usize = df_cfg.get("sr").unwrap().parse()?;
        let hop_size: usize = df_cfg.get("hop_size").unwrap().parse()?;
        let fft_size: usize = df_cfg.get("fft_size").unwrap().parse()?;
        let min_nb_erb_freqs: usize = df_cfg.get("min_nb_erb_freqs").unwrap().parse()?;
        let nb_erb: usize = df_cfg.get("nb_erb").unwrap().parse()?;
        let nb_df: usize = df_cfg.get("nb_df").unwrap().parse()?;
        let df_order: usize = df_cfg
            .get("df_order")
            .unwrap_or_else(|| model_cfg.get("df_order").unwrap())
            .parse()?;
        let conv_lookahead: usize = model_cfg.get("conv_lookahead").unwrap().parse()?;
        let df_lookahead: usize = df_cfg
            .get("df_lookahead")
            .unwrap_or_else(|| model_cfg.get("df_lookahead").unwrap())
            .parse()?;
        let n_freqs = fft_size / 2 + 1;

        let alpha = if let Some(a) = df_cfg.get("norm_alpha") {
            a.parse::<f32>()?
        } else {
            let tau: f32 = df_cfg.get("norm_tau").unwrap().parse()?;
            calc_norm_alpha(sr, hop_size, tau)
        };

        let model_type = config.section(Some("train")).unwrap().get("model").unwrap();
        let lookahead = match model_type {
            "deepfilternet3" => conv_lookahead.max(df_lookahead),
            _ => bail!("Unsupported model type: {}", model_type),
        };

        let mut base_state = df::DFState::new(sr, fft_size, hop_size, nb_erb, min_nb_erb_freqs);
        base_state.init_norm_states(nb_df);
        let df_states: Vec<df::DFState> = (0..ch).map(|_| base_state.clone()).collect();

        let mut rolling_spec_buf_y: VecDeque<_> = VecDeque::new();
        for _ in 0..(df_order + conv_lookahead) {
            rolling_spec_buf_y.push_back(vec![vec![df::Complex32::default(); n_freqs]; ch]);
        }
        let mut rolling_spec_buf_x: VecDeque<_> = VecDeque::new();
        for _ in 0..df_order.max(lookahead) {
            rolling_spec_buf_x.push_back(vec![vec![df::Complex32::default(); n_freqs]; ch]);
        }

        Ok(OrtDfTract {
            enc, erb_dec, df_dec,
            allocator: Allocator::default(),
            enc_gru_h: vec![0.0f32; GRU_H_SIZE],
            erb_gru_h: vec![0.0f32; GRU_H_SIZE],
            erb_gru2_h: vec![0.0f32; GRU_H_SIZE],
            df_gru_h: vec![0.0f32; GRU_H_SIZE],
            df_gru2_h: vec![0.0f32; GRU_H_SIZE],
            df_gru3_h: vec![0.0f32; GRU_H_SIZE],
            lookahead,
            sr, ch, fft_size, hop_size,
            nb_erb, nb_df, n_freqs, df_order, alpha,
            min_db_thresh: -15.0,
            max_db_erb_thresh: 35.0,
            max_db_df_thresh: 35.0,
            atten_lim: None,
            rolling_spec_buf_y, rolling_spec_buf_x, df_states,
            post_filter: false, post_filter_beta: 0.02,
            skip_counter: 0,
        })
    }

    pub fn set_atten_lim(&mut self, db: f32) {
        let lim = db.abs();
        self.atten_lim = if lim >= 100. { None } else { Some(10f32.powf(-lim / 20.)) };
    }

    pub fn process(
        &mut self,
        noisy: &[Vec<f32>],
        enh: &mut [Vec<f32>],
    ) -> Result<f32> {
        let ch = noisy.len().min(enh.len()).min(self.ch);
        let frame_size = noisy[0].len();

        let (max_a, e) = noisy.iter().flat_map(|c| c.iter())
            .fold((0f32, 0f32), |(m, s), &x| (m.max(x.abs()), s + x * x));
        let rms = e / (ch * frame_size) as f32;

        if rms < 1e-7 { self.skip_counter += 1; } else { self.skip_counter = 0; }
        if self.skip_counter > 5 {
            for ch_out in enh.iter_mut().take(ch) { ch_out.fill(0.0); }
            return Ok(-15.);
        }
        if max_a > 0.9999 { log::warn!("Possible clipping ({:.3})", max_a); }

        self.rolling_spec_buf_y.pop_front();
        self.rolling_spec_buf_x.pop_front();

        let mut spec = vec![vec![df::Complex32::default(); self.n_freqs]; ch];
        for c in 0..ch { self.df_states[c].analysis(&noisy[c], &mut spec[c]); }
        self.rolling_spec_buf_y.push_back(spec.clone());
        self.rolling_spec_buf_x.push_back(spec.clone());

        if self.atten_lim.unwrap_or_default() == 1. {
            for c in 0..ch { enh[c].copy_from_slice(&noisy[c]); }
            return Ok(35.);
        }

        let (lsnr, gains, coefs) = self.process_raw(&spec)?;
        let (apply_erb, _, _) = {
            let min = self.min_db_thresh;
            let max_erb = self.max_db_erb_thresh;
            let max_df = self.max_db_df_thresh;
            if lsnr < min { (false, true, false) }
            else if lsnr > max_erb { (false, false, false) }
            else if lsnr > max_df { (true, false, false) }
            else { (true, false, true) }
        };

        let mut spec_enhanced = self.rolling_spec_buf_y[self.df_order - 1].clone();

        if let Some(ref gains) = gains {
            for c in 0..ch { self.df_states[0].apply_mask(&mut spec_enhanced[c], gains); }
            self.skip_counter = 0;
        } else { self.skip_counter += 1; }

        if let Some(ref coefs) = coefs {
            let spec_old: Vec<Vec<df::Complex32>> = self.rolling_spec_buf_x.iter()
                .take(self.df_order).map(|s| s[0].clone()).collect();
            df_fn(&spec_old, coefs, self.nb_df, self.df_order, &mut spec_enhanced[0]);
        }

        let spec_noisy = self.rolling_spec_buf_x
            .get(self.lookahead.max(self.df_order) - self.lookahead - 1).unwrap();

        if apply_erb && self.post_filter {
            for c in 0..ch { df::post_filter(&spec_noisy[c], &mut spec_enhanced[c], self.post_filter_beta); }
        }

        if let Some(lim) = self.atten_lim {
            for c in 0..ch {
                for (e, n) in spec_enhanced[c].iter_mut().zip(spec_noisy[c].iter()) {
                    *e *= 1. - lim; *e += n * lim;
                }
            }
        }

        for c in 0..ch { self.df_states[c].synthesis(&mut spec_enhanced[c], &mut enh[c]); }
        Ok(lsnr)
    }

    fn process_raw(
        &mut self,
        spec: &[Vec<df::Complex32>],
    ) -> Result<(f32, Option<Vec<f32>>, Option<Vec<Vec<df::Complex32>>>)> {
        let alloc = &self.allocator;

        let mut feat_erb = vec![0.0f32; self.nb_erb];
        let mut feat_spec = vec![0.0f32; 2 * self.nb_df];
        self.df_states[0].feat_erb(&spec[0], self.alpha, &mut feat_erb);
        {
            let mut cplx_out = vec![df::Complex32::default(); self.nb_df];
            self.df_states[0].feat_cplx(&spec[0][..self.nb_df], self.alpha, &mut cplx_out);
            for i in 0..self.nb_df {
                feat_spec[i] = cplx_out[i].re;
                feat_spec[self.nb_df + i] = cplx_out[i].im;
            }
        }

        // Extract state before mutable borrow
        let enc_gru_data = self.enc_gru_h.clone();
        let erb_gru_data = self.erb_gru_h.clone();
        let erb_gru2_data = self.erb_gru2_h.clone();
        let df_gru_data = self.df_gru_h.clone();
        let df_gru2_data = self.df_gru2_h.clone();
        let df_gru3_data = self.df_gru3_h.clone();
        let min_t = self.min_db_thresh;
        let max_erb_t = self.max_db_erb_thresh;
        let max_df_t = self.max_db_df_thresh;

        // --- Encoder (S=1, Pads in model handle zero-padding) ---
        let mut enc_outputs = self.enc.run(inputs![
            "feat_erb" => tensor_from_data(alloc, &[1, 1, 1, self.nb_erb], &feat_erb)?,
            "feat_spec" => tensor_from_data(alloc, &[1, 2, 1, self.nb_df], &feat_spec)?,
            "gru_h_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &enc_gru_data)?,
        ])?;

        let lsnr = {
            let val = enc_outputs.remove("lsnr").unwrap();
            let t: Tensor<f32> = val.downcast()?;
            let (_, data) = t.try_extract_tensor::<f32>()?;
            data[0]
        };

        {
            let val = enc_outputs.remove("/emb_gru/GRU_output_1").unwrap();
            let t: Tensor<f32> = val.downcast()?;
            let (_, data) = t.try_extract_tensor::<f32>()?;
            self.enc_gru_h.copy_from_slice(data);
        }

        let (apply_gains, apply_gain_zeros, apply_df) = {
            if lsnr < min_t { (false, true, false) }
            else if lsnr > max_erb_t { (false, false, false) }
            else if lsnr > max_df_t { (true, false, false) }
            else { (true, false, true) }
        };

        // Extract emb once (shared by both decoders)
        let emb_data = if apply_gains || apply_df {
            let val = enc_outputs.remove("emb").unwrap();
            let t: Tensor<f32> = val.downcast()?;
            let (_, data) = t.try_extract_tensor::<f32>()?;
            Some(data.to_vec())
        } else {
            None
        };

        // --- ERB decoder ---
        let mut gains: Option<Vec<f32>> = None;
        if apply_gains {
            let emb = tensor_from_data(alloc, &[1, 1, emb_data.as_ref().unwrap().len()], emb_data.as_ref().unwrap())?;
            let e3 = enc_outputs.remove("e3").unwrap();
            let e2 = enc_outputs.remove("e2").unwrap();
            let e1 = enc_outputs.remove("e1").unwrap();
            let e0 = enc_outputs.remove("e0").unwrap();

            let mut erb_outputs = self.erb_dec.run(inputs![
                "emb" => emb,
                "e3" => e3, "e2" => e2, "e1" => e1, "e0" => e0,
                "gru_h_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &erb_gru_data)?,
                "gru_h2_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &erb_gru2_data)?,
            ])?;

            {
                let m_val = erb_outputs.remove("m").unwrap();
                let m_t: Tensor<f32> = m_val.downcast()?;
                let (_, data) = m_t.try_extract_tensor::<f32>()?;
                gains = Some(data.to_vec());
            }
            {
                let val = erb_outputs.remove("/emb_gru/GRU_output_1").unwrap();
                let t: Tensor<f32> = val.downcast()?;
                let (_, data) = t.try_extract_tensor::<f32>()?;
                self.erb_gru_h.copy_from_slice(data);
            }
            {
                let val = erb_outputs.remove("/emb_gru/GRU_1_output_1").unwrap();
                let t: Tensor<f32> = val.downcast()?;
                let (_, data) = t.try_extract_tensor::<f32>()?;
                self.erb_gru2_h.copy_from_slice(data);
            }
        } else if apply_gain_zeros {
            gains = Some(vec![0.0f32; self.nb_erb]);
        }

        // --- DF decoder ---
        let mut coefs: Option<Vec<Vec<df::Complex32>>> = None;
        if apply_df {
            let emb = tensor_from_data(alloc, &[1, 1, emb_data.as_ref().unwrap().len()], emb_data.unwrap().as_slice())?;
            let c0_val = enc_outputs.remove("c0").unwrap();
            let c0_t: Tensor<f32> = c0_val.downcast()?;
            let (_, c0_data) = c0_t.try_extract_tensor::<f32>()?;
            let c0_vec = c0_data.to_vec();

            let mut df_outputs = self.df_dec.run(inputs![
                "emb" => emb,
                "c0" => tensor_from_data(alloc, &[1, 64, 1, 96], &c0_vec)?,
                "gru_h_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &df_gru_data)?,
                "gru_h2_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &df_gru2_data)?,
                "gru_h3_in" => tensor_from_data(alloc, &[1, 1, GRU_H_SIZE], &df_gru3_data)?,
            ])?;

            {
                let c_val = df_outputs.remove("coefs").unwrap();
                let c_t: Tensor<f32> = c_val.downcast()?;
                let (shape, data) = c_t.try_extract_tensor::<f32>()?;
                let nb_df_out = shape[shape.len() - 2] as usize;
                let df_re_im = shape[shape.len() - 1] as usize;
                let df_order_out = df_re_im / 2;
                let mut cf = vec![vec![df::Complex32::default(); self.nb_df]; self.df_order];
                for freq in 0..nb_df_out.min(self.nb_df) {
                    for df_i in 0..df_order_out.min(self.df_order) {
                        let idx = freq * df_re_im + df_i * 2;
                        cf[df_i][freq] = df::Complex32::new(data[idx], data[idx + 1]);
                    }
                }
                coefs = Some(cf);
            }
            {
                let val = df_outputs.remove("/df_gru/gru/GRU_output_1").unwrap();
                let t: Tensor<f32> = val.downcast()?;
                let (_, data) = t.try_extract_tensor::<f32>()?;
                self.df_gru_h.copy_from_slice(data);
            }
            {
                let val = df_outputs.remove("/df_gru/gru/GRU_1_output_1").unwrap();
                let t: Tensor<f32> = val.downcast()?;
                let (_, data) = t.try_extract_tensor::<f32>()?;
                self.df_gru2_h.copy_from_slice(data);
            }
            {
                let val = df_outputs.remove("/df_gru/gru/GRU_2_output_1").unwrap();
                let t: Tensor<f32> = val.downcast()?;
                let (_, data) = t.try_extract_tensor::<f32>()?;
                self.df_gru3_h.copy_from_slice(data);
            }
        }

        Ok((lsnr, gains, coefs))
    }

    pub fn init(&mut self) -> Result<()> {
        for state in &mut self.df_states { state.reset(); }
        for buf in &mut self.rolling_spec_buf_y { for ch_buf in buf { ch_buf.fill(df::Complex32::default()); } }
        for buf in &mut self.rolling_spec_buf_x { for ch_buf in buf { ch_buf.fill(df::Complex32::default()); } }
        self.enc_gru_h.fill(0.0);
        self.erb_gru_h.fill(0.0);
        self.erb_gru2_h.fill(0.0);
        self.df_gru_h.fill(0.0);
        self.df_gru2_h.fill(0.0);
        self.df_gru3_h.fill(0.0);
        self.skip_counter = 0;
        Ok(())
    }
}
