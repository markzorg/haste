//! 10-band graphic equalizer: a cascade of peaking biquads (RBJ audio EQ
//! cookbook) in transposed direct form II, f32, stereo.
//!
//! Parameters are written by the UI into atomics and a version counter is
//! bumped; the realtime callback notices the new version and recomputes the
//! coefficients itself (a few `sin`/`powf` calls, no allocation, no locks).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const BANDS: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const MAX_DB: f32 = 12.0;
/// About one octave per band.
const Q: f32 = 1.41;
/// Keeps the recursive filter state out of the (slow) denormal range.
const ANTI_DENORMAL: f32 = 1e-20;

pub struct EqParams {
    enabled: AtomicBool,
    preamp: AtomicU32,
    gains: [AtomicU32; 10],
    version: AtomicU32,
}

impl EqParams {
    pub fn new() -> EqParams {
        EqParams {
            enabled: AtomicBool::new(false),
            preamp: AtomicU32::new(0),
            gains: Default::default(),
            version: AtomicU32::new(1),
        }
    }

    /// Gains and preamp in dB.
    pub fn set(&self, enabled: bool, preamp: f32, gains: &[f32; 10]) {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.preamp.store(preamp.to_bits(), Ordering::Relaxed);
        for (a, g) in self.gains.iter().zip(gains) {
            a.store(g.clamp(-MAX_DB, MAX_DB).to_bits(), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn peaking(rate: f32, f0: f32, db: f32) -> Biquad {
        let a = 10f32.powf(db / 40.0);
        let (sin, cos) = (std::f32::consts::TAU * f0 / rate).sin_cos();
        let alpha = sin / (2.0 * Q);
        let a0 = 1.0 + alpha / a;
        Biquad {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

/// Per-stream filter state, owned by the audio callback.
pub struct Processor {
    rate: f32,
    seen: u32,
    active: bool,
    preamp: f32,
    bands: [Biquad; 10],
    on: [bool; 10],
    /// [band][channel][z1, z2]
    state: [[[f32; 2]; 2]; 10],
}

impl Processor {
    pub fn new(rate: f32) -> Processor {
        Processor {
            rate,
            seen: 0,
            active: false,
            preamp: 1.0,
            bands: [Biquad::default(); 10],
            on: [false; 10],
            state: [[[0.0; 2]; 2]; 10],
        }
    }

    /// Picks up new parameters if the UI changed them.
    pub fn sync(&mut self, p: &EqParams) {
        let v = p.version.load(Ordering::Acquire);
        if v == self.seen {
            return;
        }
        self.seen = v;
        let was_active = self.active;
        self.active = p.enabled.load(Ordering::Relaxed);
        if self.active && !was_active {
            self.state = [[[0.0; 2]; 2]; 10];
        }
        self.preamp = 10f32.powf(f32::from_bits(p.preamp.load(Ordering::Relaxed)) / 20.0);
        for (i, f0) in BANDS.iter().enumerate() {
            let db = f32::from_bits(p.gains[i].load(Ordering::Relaxed));
            self.on[i] = db.abs() >= 0.05 && *f0 < self.rate * 0.45;
            if self.on[i] {
                self.bands[i] = Biquad::peaking(self.rate, *f0, db);
            }
        }
    }

    /// Filters interleaved stereo samples in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.active {
            return;
        }
        for frame in buf.chunks_exact_mut(2) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample * self.preamp + ANTI_DENORMAL;
                for ((b, on), st) in self.bands.iter().zip(&self.on).zip(&mut self.state) {
                    if !on {
                        continue;
                    }
                    let s = &mut st[ch];
                    let y = b.b0 * x + s[0];
                    s[0] = b.b1 * x - b.a1 * y + s[1];
                    s[1] = b.b2 * x - b.a2 * y;
                    x = y;
                }
                *sample = x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// Peak amplitude of a filtered sine after the filter has settled.
    fn response(gains: [f32; 10], preamp: f32, freq: f32) -> f32 {
        let params = EqParams::new();
        params.set(true, preamp, &gains);
        let mut p = Processor::new(RATE);
        p.sync(&params);
        let mut buf: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let s = (std::f32::consts::TAU * freq * i as f32 / RATE).sin() * 0.25;
                [s, s]
            })
            .collect();
        p.process(&mut buf);
        buf[buf.len() / 2..].iter().fold(0.0f32, |m, s| m.max(s.abs())) / 0.25
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    #[test]
    fn flat_is_transparent() {
        assert!((db(response([0.0; 10], 0.0, 1000.0))).abs() < 0.01);
    }

    #[test]
    fn band_boost_and_cut_hit_their_centre() {
        let mut g = [0.0; 10];
        g[5] = 6.0;
        assert!((db(response(g, 0.0, 1000.0)) - 6.0).abs() < 0.3);
        g[5] = -12.0;
        assert!((db(response(g, 0.0, 1000.0)) + 12.0).abs() < 0.5);
    }

    #[test]
    fn distant_frequencies_are_left_alone() {
        let mut g = [0.0; 10];
        g[9] = 12.0;
        assert!(db(response(g, 0.0, 100.0)).abs() < 0.3);
    }

    #[test]
    fn preamp_scales() {
        assert!((db(response([0.0; 10], -6.0, 440.0)) + 6.0).abs() < 0.05);
    }

    #[test]
    fn disabled_does_nothing() {
        let params = EqParams::new();
        params.set(false, 12.0, &[12.0; 10]);
        let mut p = Processor::new(RATE);
        p.sync(&params);
        let mut buf = [0.5f32, -0.5, 0.25, 0.1];
        p.process(&mut buf);
        assert_eq!(buf, [0.5, -0.5, 0.25, 0.1]);
    }
}
