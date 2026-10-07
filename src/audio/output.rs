//! cpal output stream. The data callback only pops from the ring buffer,
//! applies a smoothed gain and converts samples: no locks, no allocations.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use super::Shared;
use super::ring::{self, Consumer, Producer};

/// Ring capacity in samples (stereo interleaved): ~0.7 s at 48 kHz.
const RING_SAMPLES: usize = 1 << 16;
/// Largest chunk processed per inner iteration of the callback.
const CHUNK_FRAMES: usize = 1024;

pub struct Output {
    stream: cpal::Stream,
    pub producer: Producer,
    /// Rate the stream actually runs at.
    pub rate: u32,
    /// Rate that was requested when the stream was opened.
    pub wanted: u32,
}

impl Output {
    /// Opens the default device, preferring `wanted` Hz stereo so that no
    /// resampling is needed. Falls back to the device default rate.
    pub fn open(wanted: u32, shared: Arc<Shared>) -> Result<Output, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let pick = |fmt: SampleFormat| {
            device.supported_output_configs().ok()?.find(|c| {
                c.sample_format() == fmt
                    && c.channels() == 2
                    && (c.min_sample_rate()..=c.max_sample_rate()).contains(&wanted)
            })
        };
        let (format, config) = match [SampleFormat::F32, SampleFormat::I16, SampleFormat::I32]
            .into_iter()
            .find_map(pick)
        {
            Some(range) => (range.sample_format(), range.with_sample_rate(wanted).config()),
            None => {
                let def = device.default_output_config().map_err(|e| e.to_string())?;
                (def.sample_format(), def.config())
            }
        };
        let (producer, consumer) = ring::ring(RING_SAMPLES);
        let rate = config.sample_rate;
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, consumer, shared),
            SampleFormat::I16 => build::<i16>(&device, config, consumer, shared),
            SampleFormat::I32 => build::<i32>(&device, config, consumer, shared),
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        Ok(Output { stream, producer, rate, wanted })
    }

    pub fn play(&self) {
        let _ = self.stream.play();
    }

    pub fn pause(&self) {
        let _ = self.stream.pause();
    }
}

fn build<T>(device: &cpal::Device, config: StreamConfig, mut ring: Consumer, shared: Arc<Shared>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let out_ch = config.channels as usize;
    // ~10 ms gain ramp: click-free pause/resume and volume changes.
    let step = 1.0 / (config.sample_rate as f32 * 0.01);
    let mut gain = 0.0f32;
    let mut buf = vec![0.0f32; CHUNK_FRAMES * 2];
    let failed = shared.clone();
    let mut fx = super::eq::Processor::new(config.sample_rate as f32);
    let data_cb = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let paused = shared.paused.load(Relaxed);
        let target = if paused { 0.0 } else { shared.volume() };
        fx.sync(&shared.eq);
        for out in data.chunks_mut(CHUNK_FRAMES * out_ch) {
            let frames = out.len() / out_ch;
            let tmp = &mut buf[..frames * 2];
            // Once faded out while paused, stop consuming so nothing is lost.
            let got = if paused && gain == 0.0 { 0 } else { ring.pop(tmp) };
            tmp[got..].fill(0.0);
            if got > 0 {
                fx.process(tmp);
            }
            for (frame, src) in out.chunks_exact_mut(out_ch).zip(tmp.chunks_exact(2)) {
                if gain != target {
                    gain = if (target - gain).abs() <= step { target } else { gain + step.copysign(target - gain) };
                }
                let l = (src[0] * gain).clamp(-1.0, 1.0);
                let r = (src[1] * gain).clamp(-1.0, 1.0);
                if out_ch == 1 {
                    frame[0] = T::from_sample(0.5 * (l + r));
                } else {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for s in &mut frame[2..] {
                        *s = T::from_sample(0.0);
                    }
                }
            }
        }
    };
    let err_cb = move |e: cpal::Error| {
        if !matches!(e.kind(), cpal::ErrorKind::Xrun) {
            failed.failed.store(true, Relaxed);
        }
    };
    device.build_output_stream(config, data_cb, err_cb, None).map_err(|e| e.to_string())
}
