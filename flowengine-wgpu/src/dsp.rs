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
const HP_CUTOFF_HZ: f32 = 90.0; // high-pass: kill mic rumble / mains hum below this

/// GLSL-style smoothstep — 0 below e0, 1 above e1, smooth ramp between.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

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
    pub speaking: bool,
    pub voice: bool,       // this frame looks like human speech (not noise / random sound)
    pub voice_ratio: f32,  // fraction of energy inside the speech band (diagnostic)
    pub voice_env: f32,    // smooth 0..1 envelope of the speaking state — drives the visual
    speak_hold: f32,       // seconds STATUS stays "SPEAKING" after the last voiced frame
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
            speaking: false,
            voice: false,
            voice_ratio: 0.0,
            voice_env: 0.0,
            speak_hold: 0.0,
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

        // fast-attack / SLOW-decay peak-HOLD envelope. Attack is instant (latch on a
        // transient); release is gentle (~1.4s time-constant) so a peak HOLDS then
        // fades out smoothly instead of snapping back — no jittery re-triggering.
        if target > self.peak {
            self.peak = target;
        } else {
            self.peak += (target - self.peak) * (1.0 - (-1.4 * dt).exp());
        }

        // windowed FFT
        for i in 0..N {
            self.inbuf[i] = win[i] * self.window[i];
        }
        let _ = self
            .fft
            .process_with_scratch(&mut self.inbuf, &mut self.outbuf, &mut self.scratch);

        // per-bin magnitude with linear temporal smoothing (Web-Audio: smooth then dB).
        // HIGH-PASS: roll off bins below ~90 Hz — the mic's DC/rumble/mains-hum energy
        // sits in bin 0-1 (~43 Hz) and otherwise dominates peak-Hz and bass. A high-pass
        // (not low-pass) is what removes LOW-frequency noise.
        let cutoff_bin = (HP_CUTOFF_HZ * N as f32 / self.sample_rate).max(1.0);
        let tau = 0.62f32;
        let mut max_mag = 0.0f32;
        let mut max_i = 0usize;
        let mut sum_mag = 0.0f32;
        for (i, c) in self.outbuf.iter().enumerate() {
            let hp = smoothstep(0.4 * cutoff_bin, cutoff_bin, i as f32); // 0 at DC → 1 above cutoff
            let m = (c.norm() / N as f32) * hp;
            self.smooth[i] = tau * self.smooth[i] + (1.0 - tau) * m;
            sum_mag += self.smooth[i];
            if self.smooth[i] > max_mag {
                max_mag = self.smooth[i];
                max_i = i;
            }
        }
        // peak-frequency with a TIME MOVING AVERAGE. Only trust a peak that clearly
        // stands out from the noise floor (dominance test), then EMA-smooth it so the
        // readout doesn't jitter or chase noise frame-to-frame. Silence → ease to 0.
        let mean_mag = sum_mag / self.smooth.len() as f32;
        let dominant = max_mag > 6.0 * mean_mag && max_mag > 1e-5;
        if level > 0.03 && dominant {
            let raw_hz = max_i as f32 * self.sample_rate / N as f32;
            if self.peak_hz <= 0.0 {
                self.peak_hz = raw_hz; // seed instantly on first detection
            } else {
                self.peak_hz = 0.82 * self.peak_hz + 0.18 * raw_hz; // moving average
            }
        } else if level <= 0.02 {
            self.peak_hz *= 0.90; // ease toward 0 when quiet
            if self.peak_hz < 1.0 {
                self.peak_hz = 0.0;
            }
        }

        // VOICE-ACTIVITY DETECTION — human speech, or just noise / a random sound?
        // Human speech concentrates its energy in ~120 Hz–3.8 kHz with a fundamental
        // (pitch) in the vocal range; broadband hiss and out-of-band thuds do not.
        let bin_hz = self.sample_rate / N as f32;
        let bidx = |hz: f32| ((hz / bin_hz) as usize).min(self.smooth.len() - 1);
        let (lo, hi) = (bidx(120.0), bidx(3800.0));
        let voice_e: f32 = self.smooth[lo..=hi].iter().map(|m| m * m).sum();
        let total_e: f32 = self.smooth.iter().map(|m| m * m).sum::<f32>() + 1e-12;
        self.voice_ratio = voice_e / total_e;
        let pitch_ok = self.peak_hz >= 90.0 && self.peak_hz <= 1000.0; // fundamental in vocal range
        self.voice = level > 0.05 && self.voice_ratio > 0.55 && pitch_ok;

        // STATUS "SPEAKING": hold + hysteresis on the VOICE decision, so a pause between
        // words doesn't cut it and a stray noise blip doesn't trigger it.
        if self.voice {
            self.speak_hold = 0.6;
        } else {
            self.speak_hold = (self.speak_hold - dt).max(0.0);
        }
        self.speaking = self.speak_hold > 0.0;

        // smooth 0..1 envelope of the speaking state: rises fast when a voice is present,
        // eases down after — this is what the shader uses to swell the ripple on speech
        // while staying calm at rest.
        let vt = if self.speaking { 1.0 } else { 0.0 };
        self.voice_env += (vt - self.voice_env) * (1.0 - (-5.0 * dt).exp());

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
