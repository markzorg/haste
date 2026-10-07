//! Linear-interpolation resampler for interleaved stereo. Only used when the
//! output device refuses the file's sample rate (PipeWire/Pulse normally
//! accept any rate and resample with better quality themselves).

pub struct Resampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Resampler { step: from as f64 / to as f64, pos: 1.0, prev: [0.0; 2] }
    }

    /// Appends resampled frames of `input` to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len() / 2;
        if n == 0 {
            return;
        }
        // `pos` is measured in input frames where 0 is `prev` and 1 is input[0].
        let frame = |i: usize| if i == 0 { self.prev } else { [input[2 * i - 2], input[2 * i - 1]] };
        out.reserve(((n as f64 / self.step) as usize + 1) * 2);
        while self.pos <= n as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let a = frame(i);
            let b = if i < n { frame(i + 1) } else { a };
            out.push(a[0] + (b[0] - a[0]) * t);
            out.push(a[1] + (b[1] - a[1]) * t);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = frame(n);
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    #[test]
    fn ratio_is_respected() {
        let mut r = Resampler::new(44_100, 48_000);
        let input = vec![0.5f32; 44_100 * 2];
        let mut out = Vec::new();
        for chunk in input.chunks(1152 * 2) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() <= 2, "{frames}");
        // A constant signal stays constant (after the first frame).
        assert!(out[4..].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn identity_rate_passes_through() {
        let mut r = Resampler::new(48_000, 48_000);
        let input: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out, input);
    }
}
