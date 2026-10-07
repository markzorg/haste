//! The decoding thread: reacts to commands, keeps the ring buffer filled and
//! reports track boundaries.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::decoder::Decoder;
use super::output::Output;
use super::resample::Resampler;
use super::{Command, Event, Shared, Timeline};

/// Refill the ring as soon as at least this many samples are free.
const REFILL: usize = 4096;
/// Delay between the start of a pause fade-out and stopping the stream.
const PAUSE_DELAY: Duration = Duration::from_millis(60);

pub struct Engine {
    rx: Receiver<Command>,
    shared: Arc<Shared>,
    timeline: Arc<Mutex<Timeline>>,
    notify: Box<dyn Fn(Event) + Send>,
    dec: Option<Decoder>,
    out: Option<Output>,
    resampler: Option<Resampler>,
    decoded: Vec<f32>,
    pending: Vec<f32>,
    pending_off: usize,
    /// Ring index at which the current track ends (decoder hit EOF).
    eof_idx: Option<usize>,
    playing: bool,
    pause_at: Option<Instant>,
    seek_to: Option<f64>,
}

impl Engine {
    pub fn new(
        rx: Receiver<Command>,
        shared: Arc<Shared>,
        timeline: Arc<Mutex<Timeline>>,
        notify: Box<dyn Fn(Event) + Send>,
    ) -> Engine {
        Engine {
            rx,
            shared,
            timeline,
            notify,
            dec: None,
            out: None,
            resampler: None,
            decoded: Vec::new(),
            pending: Vec::new(),
            pending_off: 0,
            eof_idx: None,
            playing: false,
            pause_at: None,
            seek_to: None,
        }
    }

    pub fn run(mut self) {
        loop {
            let first = match self.rx.recv_timeout(self.wait_time()) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // Drain everything queued so that bursts (e.g. slider drags)
            // collapse into a single seek.
            let mut next = first;
            while let Some(cmd) = next {
                if !self.handle(cmd) {
                    return;
                }
                next = self.rx.try_recv().ok();
            }
            if let Some(secs) = self.seek_to.take() {
                self.seek(secs);
            }
            self.housekeeping();
            self.fill();
        }
    }

    fn wait_time(&self) -> Duration {
        if let Some(out) = &self.out {
            if self.dec.is_some() && self.eof_idx.is_none() && out.producer.free() >= REFILL {
                return Duration::ZERO;
            }
            if self.playing {
                let queued = out.producer.write_index().wrapping_sub(out.producer.read_index()) as u64;
                let ms = queued * 1000 / 2 / u64::from(out.rate.max(1));
                return Duration::from_millis((ms / 3).clamp(5, 50));
            }
            if let Some(t) = self.pause_at {
                return t.saturating_duration_since(Instant::now());
            }
        }
        Duration::from_secs(1)
    }

    fn handle(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Load { path, start, play } => self.load(&path, start, play),
            Command::Play => self.resume(),
            Command::Pause => self.pause(),
            Command::Stop => self.stop(),
            Command::Seek(secs) => self.seek_to = Some(secs),
            Command::Quit => return false,
        }
        true
    }

    fn load(&mut self, path: &std::path::Path, start: f64, play: bool) {
        self.seek_to = None;
        self.dec = None;
        let (mut dec, cover) = match Decoder::open(path) {
            Ok(d) => d,
            Err(e) => {
                self.stop();
                (self.notify)(Event::LoadFailed(format!("{}: {e}", path.display())));
                return;
            }
        };
        if self.out.as_ref().is_none_or(|o| o.wanted != dec.rate) {
            self.out = None;
            match Output::open(dec.rate, self.shared.clone()) {
                Ok(o) => self.out = Some(o),
                Err(e) => {
                    self.stop();
                    (self.notify)(Event::LoadFailed(e));
                    return;
                }
            }
        }
        let start = if start > 0.0 { dec.seek(start).unwrap_or(0.0) } else { 0.0 };
        let duration = dec.duration;
        self.dec = Some(dec);
        self.restart_at(start);
        (self.notify)(Event::Loaded { path: path.to_path_buf(), duration, cover });
        if play { self.resume() } else { self.pause() }
    }

    /// Drops queued audio and restarts the clock at `secs`.
    fn restart_at(&mut self, secs: f64) {
        let (Some(dec), Some(out)) = (&self.dec, &self.out) else { return };
        self.resampler = (out.rate != dec.rate).then(|| Resampler::new(dec.rate, out.rate));
        self.pending.clear();
        self.pending_off = 0;
        self.eof_idx = None;
        let base_idx = out.producer.flush();
        if let Ok(mut t) = self.timeline.lock() {
            *t = Timeline { cursor: Some(out.producer.cursor()), base_idx, base_secs: secs, rate: out.rate };
        }
    }

    fn seek(&mut self, secs: f64) {
        let Some(dec) = &mut self.dec else { return };
        match dec.seek(secs) {
            Ok(actual) => self.restart_at(actual),
            Err(_) => {
                // Past the end (durations in headers can be off): finish the
                // track as if it had played out.
                self.restart_at(secs);
                if let Some(out) = &self.out {
                    self.eof_idx = Some(out.producer.write_index());
                }
            }
        }
    }

    fn resume(&mut self) {
        if let Some(out) = &self.out {
            self.shared.paused.store(false, Relaxed);
            out.play();
            self.playing = true;
            self.pause_at = None;
        }
    }

    fn pause(&mut self) {
        self.shared.paused.store(true, Relaxed);
        if self.playing {
            self.pause_at = Some(Instant::now() + PAUSE_DELAY);
        } else if let Some(out) = &self.out {
            out.pause();
        }
        self.playing = false;
    }

    fn stop(&mut self) {
        self.shared.paused.store(true, Relaxed);
        self.playing = false;
        self.pause_at = None;
        self.dec = None;
        self.out = None;
        self.eof_idx = None;
        self.pending.clear();
        if let Ok(mut t) = self.timeline.lock() {
            *t = Timeline::default();
        }
    }

    fn housekeeping(&mut self) {
        if self.pause_at.is_some_and(|t| Instant::now() >= t) {
            self.pause_at = None;
            if let Some(out) = &self.out {
                out.pause();
            }
        }
        if self.shared.failed.swap(false, Relaxed) && self.out.is_some() {
            // Device vanished or the server restarted: reopen and continue.
            let pos = self.timeline.lock().map_or(0.0, |t| t.position());
            let rate = self.dec.as_ref().map_or(48_000, |d| d.rate);
            self.out = None;
            match Output::open(rate, self.shared.clone()) {
                Ok(o) => self.out = Some(o),
                Err(e) => {
                    self.stop();
                    (self.notify)(Event::LoadFailed(e));
                    return;
                }
            }
            if let Some(dec) = &mut self.dec {
                let pos = dec.seek(pos).unwrap_or(pos);
                self.restart_at(pos);
            }
            if self.playing {
                self.resume();
            }
        }
        if let (Some(end), Some(out)) = (self.eof_idx, &self.out) {
            if out.producer.read_index().wrapping_sub(end) as isize >= 0 {
                self.eof_idx = None;
                let path = self.dec.take().map(|d| d.path).unwrap_or_default();
                self.pause();
                (self.notify)(Event::Finished(path));
            }
        }
    }

    fn fill(&mut self) {
        let (Some(dec), Some(out)) = (&mut self.dec, &mut self.out) else { return };
        if self.eof_idx.is_some() {
            return;
        }
        loop {
            if self.pending_off < self.pending.len() {
                self.pending_off += out.producer.push(&self.pending[self.pending_off..]);
                if self.pending_off < self.pending.len() {
                    return;
                }
            }
            self.pending.clear();
            self.pending_off = 0;
            if out.producer.free() == 0 {
                return;
            }
            self.decoded.clear();
            match dec.decode_into(&mut self.decoded) {
                Ok(true) => match &mut self.resampler {
                    Some(r) => r.process(&self.decoded, &mut self.pending),
                    None => std::mem::swap(&mut self.pending, &mut self.decoded),
                },
                Ok(false) => {
                    self.eof_idx = Some(out.producer.write_index());
                    return;
                }
                Err(e) => {
                    (self.notify)(Event::Error(e.to_string()));
                    self.eof_idx = Some(out.producer.write_index());
                    return;
                }
            }
        }
    }
}
