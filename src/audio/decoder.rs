//! Thin wrapper over symphonia: opens a file, decodes packets into
//! interleaved stereo `f32` and performs sample-accurate seeks.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardVisualKey};
use symphonia::core::units::{Time, TimeBase, Timestamp};

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    pub rate: u32,
    pub duration: Option<f64>,
    pub path: PathBuf,
    /// Frames still to drop after an accurate seek landed early.
    skip: u64,
    scratch: Vec<f32>,
}

/// Opens `path` and returns a reader probed with the given metadata options.
pub fn probe(path: &Path, meta: MetadataOptions) -> Result<Box<dyn FormatReader>, Error> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), meta)
}

/// Best known duration of the default audio track, in seconds.
pub fn duration_of(format: &dyn FormatReader) -> Option<f64> {
    let track = format.default_track(TrackType::Audio)?;
    let rate = track.codec_params.as_ref()?.audio()?.sample_rate;
    if let (Some(n), Some(rate)) = (track.num_frames, rate) {
        return Some(n as f64 / rate as f64);
    }
    if let (Some(d), Some(tb)) = (track.duration, track.time_base) {
        return tb.calc_duration(d).map(|t| t.as_secs_f64());
    }
    let info = format.media_info();
    let tb = info.time_base?;
    tb.calc_duration(info.duration?).map(|t| t.as_secs_f64())
}

/// Visits every queued metadata revision, newest first.
pub fn for_each_revision(format: &mut dyn FormatReader, f: &mut dyn FnMut(&MetadataRevision)) {
    let mut md = format.metadata();
    let mut older = Vec::new();
    while let Some(rev) = md.pop() {
        older.push(rev);
    }
    if let Some(cur) = md.current() {
        f(cur);
    }
    for rev in older.iter().rev() {
        f(rev);
    }
}

/// Picks the front cover (or any picture) from a metadata revision.
fn cover_of(rev: &MetadataRevision) -> Option<&[u8]> {
    let visuals = rev.media.visuals.iter().chain(rev.per_track.iter().flat_map(|t| &t.metadata.visuals));
    let mut best: Option<&[u8]> = None;
    for v in visuals {
        if v.usage == Some(StandardVisualKey::FrontCover) {
            return Some(&v.data);
        }
        best = best.or(Some(&v.data));
    }
    best
}

impl Decoder {
    /// Opens a file for playback. Also returns embedded cover art, if any.
    pub fn open(path: &Path) -> Result<(Decoder, Option<Vec<u8>>), Error> {
        let mut format = probe(path, MetadataOptions::default())?;
        let mut cover = None;
        for_each_revision(format.as_mut(), &mut |rev| {
            if cover.is_none() {
                cover = cover_of(rev).map(<[u8]>::to_vec);
            }
        });
        let duration = duration_of(format.as_ref());
        let track = format.default_track(TrackType::Audio).ok_or(Error::Unsupported("no audio track"))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(Error::Unsupported("no audio codec"))?;
        let rate = params.sample_rate.unwrap_or(44_100);
        let track_id = track.id;
        let time_base = track.time_base;
        let decoder = symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())?;
        let path = path.to_path_buf();
        let dec = Decoder { format, decoder, track_id, time_base, rate, duration, path, skip: 0, scratch: Vec::new() };
        Ok((dec, cover))
    }

    /// Decodes the next packet and appends interleaved stereo samples to
    /// `out`. Returns `Ok(false)` at end of stream.
    pub fn decode_into(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(Error::ResetRequired) => return Ok(false),
                Err(e) => return Err(e),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => continue,
                Err(e) => return Err(e),
            };
            let ch = buf.spec().channels().count().max(1);
            let rate = buf.spec().rate();
            buf.copy_to_vec_interleaved(&mut self.scratch);
            if rate != 0 && rate != self.rate {
                // Sample rate changed mid-stream (rare); follow it.
                self.rate = rate;
            }
            let frames = self.scratch.len() / ch;
            let skip = (self.skip as usize).min(frames);
            self.skip -= skip as u64;
            out.reserve((frames - skip) * 2);
            for f in self.scratch.chunks_exact(ch).skip(skip) {
                let (l, r) = if ch == 1 { (f[0], f[0]) } else { (f[0], f[1]) };
                out.push(l);
                out.push(r);
            }
            if frames > skip {
                return Ok(true);
            }
        }
    }

    /// Seeks to `secs`; returns the position actually reached.
    pub fn seek(&mut self, secs: f64) -> Result<f64, Error> {
        let time = Time::try_from_secs_f64(secs.max(0.0)).unwrap_or_default();
        let to = SeekTo::Time { time, track_id: Some(self.track_id) };
        let seeked = self.format.seek(SeekMode::Accurate, to)?;
        self.decoder.reset();
        self.skip = 0;
        let Some(tb) = self.time_base else { return Ok(secs) };
        let to_secs = |ts: Timestamp| tb.calc_time(ts).map_or(0.0, |t| t.as_secs_f64());
        let required = to_secs(seeked.required_ts);
        let actual = to_secs(seeked.actual_ts);
        if required > actual {
            self.skip = ((required - actual) * self.rate as f64).round() as u64;
        }
        Ok(required)
    }
}

/// Diagnostic for a real file:
/// `HASTE_DECODE_FILE=song.m4a cargo test decode_env_file -- --ignored --nocapture`
#[cfg(test)]
mod probe_file {
    #[test]
    #[ignore]
    fn decode_env_file() {
        let path = std::env::var("HASTE_DECODE_FILE").unwrap();
        let (mut d, _) = super::Decoder::open(std::path::Path::new(&path)).unwrap();
        let mut out = Vec::new();
        while d.decode_into(&mut out).unwrap() {}
        let frames = out.len() / 2;
        let lead = out.chunks(2).take_while(|f| f[0].abs() < 1e-3).count();
        let trail = out.chunks(2).rev().take_while(|f| f[0].abs() < 1e-3).count();
        println!("rate {} frames {} ({:.3}s) lead-silence {} trail-silence {} duration {:?}", d.rate, frames, frames as f64 / d.rate as f64, lead, trail, d.duration);
    }
}
