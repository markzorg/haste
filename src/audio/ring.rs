//! Lock-free single-producer / single-consumer ring buffer of `f32` samples.
//!
//! Indices grow monotonically (wrapping) and are masked on access, so
//! `write - read` is always the number of readable samples. Besides the usual
//! push/pop the buffer supports a wait-free *flush*: the producer publishes
//! the write index it wants the consumer to skip to, and the consumer jumps
//! there at the start of its next read. This lets the engine discard queued
//! audio on seek/stop without ever blocking the realtime callback.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Shared {
    buf: Box<[UnsafeCell<f32>]>,
    mask: usize,
    read: AtomicUsize,
    write: AtomicUsize,
    flush_to: AtomicUsize,
}

// SAFETY: slots in `[read, write)` are only read by the consumer and slots in
// `[write, read + cap)` are only written by the producer; ownership of a slot
// is handed over through the Release/Acquire pair on `write`/`read`.
unsafe impl Sync for Shared {}

pub struct Producer(Arc<Shared>);
pub struct Consumer(Arc<Shared>);

/// Creates a ring with room for at least `min_cap` samples.
pub fn ring(min_cap: usize) -> (Producer, Consumer) {
    let cap = min_cap.next_power_of_two();
    let buf = (0..cap).map(|_| UnsafeCell::new(0.0)).collect();
    let shared = Arc::new(Shared {
        buf,
        mask: cap - 1,
        read: AtomicUsize::new(0),
        write: AtomicUsize::new(0),
        flush_to: AtomicUsize::new(0),
    });
    (Producer(shared.clone()), Consumer(shared))
}

impl Producer {
    /// Number of samples that can be pushed right now.
    pub fn free(&self) -> usize {
        let s = &self.0;
        let used = s.write.load(Ordering::Relaxed).wrapping_sub(s.read.load(Ordering::Acquire));
        s.buf.len().saturating_sub(used)
    }

    /// Pushes as many samples as fit and returns how many were written.
    pub fn push(&mut self, data: &[f32]) -> usize {
        let s = &self.0;
        let w = s.write.load(Ordering::Relaxed);
        let n = data.len().min(self.free());
        for (i, &v) in data[..n].iter().enumerate() {
            // SAFETY: the slot lies in the producer-owned region (see `Shared`).
            unsafe { *s.buf[w.wrapping_add(i) & s.mask].get() = v };
        }
        s.write.store(w.wrapping_add(n), Ordering::Release);
        n
    }

    /// Absolute index of the next sample to be written.
    pub fn write_index(&self) -> usize {
        self.0.write.load(Ordering::Relaxed)
    }

    /// Absolute index of the next sample the consumer will play.
    pub fn read_index(&self) -> usize {
        self.0.read.load(Ordering::Acquire)
    }

    /// Asks the consumer to drop everything queued so far. Returns the index
    /// at which new data will start.
    pub fn flush(&self) -> usize {
        let w = self.write_index();
        self.0.flush_to.store(w, Ordering::Release);
        w
    }

    /// Shares the read cursor with other (non-realtime) observers.
    pub fn cursor(&self) -> Cursor {
        Cursor(self.0.clone())
    }
}

impl Consumer {
    /// Fills `out` from the queue, returning the number of samples copied.
    /// Never blocks and never allocates.
    pub fn pop(&mut self, out: &mut [f32]) -> usize {
        let s = &self.0;
        let mut r = s.read.load(Ordering::Relaxed);
        let flush = s.flush_to.load(Ordering::Acquire);
        if (flush.wrapping_sub(r) as isize) > 0 {
            r = flush;
        }
        let avail = s.write.load(Ordering::Acquire).wrapping_sub(r);
        let n = out.len().min(avail);
        for (i, o) in out[..n].iter_mut().enumerate() {
            // SAFETY: the slot lies in the consumer-owned region (see `Shared`).
            *o = unsafe { *s.buf[r.wrapping_add(i) & s.mask].get() };
        }
        s.read.store(r.wrapping_add(n), Ordering::Release);
        n
    }
}

/// Read-only view of the consumer position, used for the playback clock.
#[derive(Clone)]
pub struct Cursor(Arc<Shared>);

impl Cursor {
    pub fn read_index(&self) -> usize {
        self.0.read.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_wraps() {
        let (mut p, mut c) = ring(8);
        let mut out = [0.0; 8];
        for round in 0..10 {
            let data: Vec<f32> = (0..6).map(|i| (round * 10 + i) as f32).collect();
            assert_eq!(p.push(&data), 6);
            assert_eq!(c.pop(&mut out[..6]), 6);
            assert_eq!(&out[..6], &data[..]);
        }
    }

    #[test]
    fn push_respects_capacity() {
        let (mut p, mut c) = ring(4);
        assert_eq!(p.push(&[1.0; 10]), 4);
        assert_eq!(p.free(), 0);
        let mut out = [0.0; 2];
        assert_eq!(c.pop(&mut out), 2);
        assert_eq!(p.free(), 2);
    }

    #[test]
    fn flush_skips_queued_data() {
        let (mut p, mut c) = ring(16);
        p.push(&[1.0; 6]);
        let start = p.flush();
        assert_eq!(start, 6);
        // Old data still occupies space until the consumer catches up.
        assert_eq!(p.free(), 10);
        p.push(&[2.0; 3]);
        let mut out = [0.0; 8];
        assert_eq!(c.pop(&mut out), 3);
        assert_eq!(&out[..3], &[2.0; 3]);
        assert_eq!(p.read_index(), 9);
        assert_eq!(p.free(), 16);
    }
}
