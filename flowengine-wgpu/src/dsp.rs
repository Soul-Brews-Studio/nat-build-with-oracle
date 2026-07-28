// dsp.rs — audio analysis: Blackman-windowed real FFT -> per-bin magnitude ->
// temporal smoothing -> dB byte map (Web-Audio parity), plus RMS level (critically-
// damped-spring smoothed), a fast-attack/slow-decay peak, and a bass band.
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

const N: usize = 1024;
const SPEC_BINS: usize = 256;
const MIN_DB: f32 = -92.0;
const MAX_DB: f32 = -24.0;

pub struct AudioAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: [f32; N],
    inbuf: Vec<f32>,
    outbuf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    smooth: Vec<f32>, // linear-magnitude smoothing state, per bin
    pub bytes: [u8; SPEC_BINS],
    level_x: f32,
    level_v: f32,
    peak: f32,
    pub peak_hz: f32,
    sample_rate: f32,
}

impl AudioAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N);
        let mut window = [0.0f32; N];
        for (i, w) in window.iter_mut().enumerate() {
            let x = i as f32 / (N as f32 - 1.0);
            let two_pi = std::f32::consts::TAU;
            *w = 0.42 - 0.5 * (two_pi * x).cos() + 0.08 * (2.0 * two_pi * x).cos(); // Blackman
        }
        let outbuf = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let nbins = outbuf.len(); // N/2 + 1 = 513
        AudioAnalyzer {
            fft,
            window,
            inbuf: vec![0.0; N],
            outbuf,
            scratch,
            smooth: vec![0.0; nbins],
            bytes: [0; SPEC_BINS],
            level_x: 0.0,
            level_v: 0.0,
            peak: 0.0,
            peak_hz: 0.0,
            sample_rate: sample_rate as f32,
        }
    }

    /// Process the latest N-sample window; returns (level, peak, bass), all 0..1.
    pub fn process(&mut self, win: &[f32], dt: f32) -> (f32, f32, f32) {
        // RMS -> spring-smoothed level
        let mut sum = 0.0f32;
        for &s in win {
            sum += s * s;
        }
        let rms = (sum / N as f32).sqrt();
        let target = (rms * 4.0).min(1.0);
        cds_tween(&mut self.level_x, &mut self.level_v, target, 12.0, dt);
        let level = self.level_x.max(0.0);

        // fast-attack / slow-decay peak
        if target > self.peak {
            self.peak = target;
        } else {
            self.peak += (target - self.peak) * (1.0 - (-6.0 * dt).exp());
        }

        // windowed FFT
        for i in 0..N {
            self.inbuf[i] = win[i] * self.window[i];
        }
        let _ = self
            .fft
            .process_with_scratch(&mut self.inbuf, &mut self.outbuf, &mut self.scratch);

        // per-bin magnitude with linear temporal smoothing (Web-Audio: smooth then dB)
        let tau = 0.62f32;
        let mut max_mag = 0.0f32;
        let mut max_i = 0usize;
        for (i, c) in self.outbuf.iter().enumerate() {
            let m = c.norm() / N as f32;
            self.smooth[i] = tau * self.smooth[i] + (1.0 - tau) * m;
            if self.smooth[i] > max_mag {
                max_mag = self.smooth[i];
                max_i = i;
            }
        }
        self.peak_hz = max_i as f32 * self.sample_rate / N as f32;

        // dB byte map for the first SPEC_BINS bins
        for i in 0..SPEC_BINS {
            let m = self.smooth[i].max(1e-10);
            let db = 20.0 * m.log10();
            let v = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0) * 255.0;
            self.bytes[i] = v as u8;
        }

        // bass = mean of low bins (matches WebGL's fdata[1..=16]/200)
        let mut b = 0u32;
        for i in 1..=16 {
            b += self.bytes[i] as u32;
        }
        let bass = ((b as f32 / 16.0) / 200.0).min(1.0);

        (level, self.peak.max(0.0), bass)
    }
}

/// KlakMath CdsTween: implicit critically-damped spring (unconditionally stable).
#[inline]
fn cds_tween(x: &mut f32, v: &mut f32, target: f32, omega: f32, dt: f32) {
    let f = 1.0 + 2.0 * dt * omega;
    let oo = omega * omega;
    let hoo = dt * oo;
    let hhoo = dt * hoo;
    let inv = 1.0 / (f + hhoo);
    let nx = (f * *x + dt * *v + hhoo * target) * inv;
    let nv = (*v + hoo * (target - *x)) * inv;
    *x = nx;
    *v = nv;
}
