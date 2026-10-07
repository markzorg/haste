//! Tag reading and background scanning of files/folders/playlists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag};

use crate::audio;
use crate::playlist::{Track, m3u};
use crate::util::{is_audio, is_playlist};

/// Reads tags and duration. Never fails: unreadable files keep their name.
pub fn read_track(path: PathBuf) -> Track {
    let Ok(mut format) = audio::probe(&path, MetadataOptions::default()) else {
        return Track::from_path(path);
    };
    let duration = audio::duration_of(format.as_ref()).unwrap_or(0.0);
    let (mut title, mut artist, mut album, mut album_artist) = (None, None, None, None);
    let mut take = |rev: &MetadataRevision| {
        let tags = rev.media.tags.iter().chain(rev.per_track.iter().flat_map(|t| &t.metadata.tags));
        for tag in tags {
            let (slot, v) = match &tag.std {
                Some(StandardTag::TrackTitle(v)) => (&mut title, v),
                Some(StandardTag::Artist(v)) => (&mut artist, v),
                Some(StandardTag::Album(v)) => (&mut album, v),
                Some(StandardTag::AlbumArtist(v)) => (&mut album_artist, v),
                _ => continue,
            };
            if slot.is_none() && !v.trim().is_empty() {
                *slot = Some(v.trim().to_owned());
            }
        }
    };
    audio::for_each_revision(format.as_mut(), &mut take);
    Track::new(
        path,
        title.unwrap_or_default(),
        artist.or(album_artist).unwrap_or_default(),
        album.unwrap_or_default(),
        duration,
    )
}

/// Expands files, folders (recursively, sorted) and playlists into audio paths.
pub fn collect(inputs: Vec<PathBuf>, cancel: &AtomicBool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in inputs {
        walk(&p, &mut out, cancel, 0);
    }
    out
}

fn walk(path: &Path, out: &mut Vec<PathBuf>, cancel: &AtomicBool, depth: u32) {
    if cancel.load(Relaxed) || depth > 32 {
        return;
    }
    if path.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort_by_cached_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        // Files of a folder first, then its sub-folders.
        let (dirs, files): (Vec<_>, Vec<_>) = entries.into_iter().partition(|p| p.is_dir());
        out.extend(files.into_iter().filter(|p| is_audio(p)));
        for d in dirs {
            walk(&d, out, cancel, depth + 1);
        }
    } else if is_playlist(path) {
        if let Ok(bytes) = std::fs::read(path) {
            let base = path.parent().unwrap_or(Path::new("/"));
            out.extend(m3u::parse(&m3u::decode(&bytes), base).into_iter().map(|e| e.path));
        }
    } else if is_audio(path) {
        out.push(path.to_path_buf());
    }
}

pub enum ScanEvent {
    /// Tracks in input order; `first` marks the first batch of a scan.
    Batch { scan: u64, tracks: Vec<Track>, first: bool },
    Done { scan: u64 },
}

const CHUNK: usize = 32;
const BATCH: usize = 512;

/// Scans `inputs` on background threads, delivering tracks in order.
/// Setting `cancel` stops the scan early.
pub fn scan(scan: u64, inputs: Vec<PathBuf>, cancel: Arc<AtomicBool>, notify: impl Fn(ScanEvent) + Send + 'static) {
    std::thread::spawn(move || {
        let paths = Arc::new(collect(inputs, &cancel));
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
        let next = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel::<(usize, Vec<Track>)>();
        for _ in 0..workers.min(paths.len().div_ceil(CHUNK)) {
            let (paths, next, tx, cancel) = (paths.clone(), next.clone(), tx.clone(), cancel.clone());
            std::thread::spawn(move || {
                loop {
                    let start = next.fetch_add(CHUNK, Relaxed);
                    if start >= paths.len() || cancel.load(Relaxed) {
                        break;
                    }
                    let end = (start + CHUNK).min(paths.len());
                    let tracks = paths[start..end].iter().cloned().map(read_track).collect();
                    if tx.send((start / CHUNK, tracks)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        // Re-order chunks and forward them in reasonably sized batches.
        let mut waiting = BTreeMap::new();
        let mut want = 0;
        let mut batch = Vec::new();
        let mut first = true;
        let mut last_flush = Instant::now();
        for (idx, tracks) in rx {
            waiting.insert(idx, tracks);
            while let Some(tracks) = waiting.remove(&want) {
                batch.extend(tracks);
                want += 1;
            }
            let due = first || batch.len() >= BATCH || last_flush.elapsed() > Duration::from_millis(200);
            if !batch.is_empty() && due {
                if cancel.load(Relaxed) {
                    return;
                }
                notify(ScanEvent::Batch { scan, tracks: std::mem::take(&mut batch), first });
                first = false;
                last_flush = Instant::now();
            }
        }
        if !batch.is_empty() && !cancel.load(Relaxed) {
            notify(ScanEvent::Batch { scan, tracks: batch, first });
        }
        notify(ScanEvent::Done { scan });
    });
}
