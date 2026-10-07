//! Audio playback: a decoding thread feeding a lock-free ring buffer that is
//! drained by the cpal callback.

mod decoder;
pub mod eq;
mod engine;
mod output;
mod resample;
mod ring;

pub use decoder::{duration_of, for_each_revision, probe};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub enum Command {
    Load { path: PathBuf, start: f64, play: bool },
    Play,
    Pause,
    Stop,
    Seek(f64),
    Quit,
}

pub enum Event {
    /// A track was opened; carries its duration and embedded cover art.
    Loaded { duration: Option<f64>, cover: Option<Vec<u8>> },
    /// The current track has been played to the very end.
    Finished,
    /// The track could not be opened (playback stopped).
    LoadFailed(String),
    /// Non-fatal problem worth showing to the user.
    Error(String),
}

/// State shared with the realtime callback (atomics only).
pub struct Shared {
    volume: AtomicU32,
    paused: AtomicBool,
    failed: AtomicBool,
    eq: eq::EqParams,
}

impl Shared {
    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }
}

/// Maps the playback clock (ring read index) to a track position.
#[derive(Default)]
struct Timeline {
    cursor: Option<ring::Cursor>,
    base_idx: usize,
    base_secs: f64,
    rate: u32,
}

impl Timeline {
    fn position(&self) -> f64 {
        match &self.cursor {
            Some(c) if self.rate > 0 => {
                let played = c.read_index().wrapping_sub(self.base_idx) as isize;
                self.base_secs + played.max(0) as f64 / 2.0 / self.rate as f64
            }
            _ => self.base_secs,
        }
    }
}

pub struct Player {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    timeline: Arc<Mutex<Timeline>>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Starts the engine thread. `notify` is called from that thread.
    pub fn new(notify: impl Fn(Event) + Send + 'static) -> Player {
        let shared = Arc::new(Shared {
            volume: AtomicU32::new(1.0f32.to_bits()),
            paused: AtomicBool::new(true),
            failed: AtomicBool::new(false),
            eq: eq::EqParams::new(),
        });
        let timeline = Arc::new(Mutex::new(Timeline::default()));
        let (tx, rx) = mpsc::channel();
        let engine = engine::Engine::new(rx, shared.clone(), timeline.clone(), Box::new(notify));
        let thread = std::thread::Builder::new()
            .name("haste-audio".into())
            .spawn(move || engine.run())
            .expect("spawn audio thread");
        Player { tx, shared, timeline, thread: Some(thread) }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Linear slider value in `0..=1`; mapped to a perceptual (cubic) gain.
    pub fn set_volume(&self, v: f64) {
        let g = v.clamp(0.0, 1.0).powi(3) as f32;
        self.shared.volume.store(g.to_bits(), Relaxed);
    }

    /// Equalizer settings in dB (applied by the output callback).
    pub fn set_eq(&self, enabled: bool, preamp: f32, gains: &[f32; 10]) {
        self.shared.eq.set(enabled, preamp, gains);
    }

    /// Current playback position in seconds.
    pub fn position(&self) -> f64 {
        self.timeline.lock().map_or(0.0, |t| t.position())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
