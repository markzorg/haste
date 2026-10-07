//! Small helpers that would otherwise pull in crates.

use std::path::{Path, PathBuf};

/// File extensions handled by the enabled symphonia features.
pub const AUDIO_EXTS: &[&str] = &["mp3", "flac", "ogg", "oga", "wav", "m4a", "mp4", "m4b", "aac"];
pub const PLAYLIST_EXTS: &[&str] = &["m3u", "m3u8"];

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn is_audio(path: &Path) -> bool {
    has_ext(path, AUDIO_EXTS)
}

pub fn is_playlist(path: &Path) -> bool {
    has_ext(path, PLAYLIST_EXTS)
}

/// `m:ss` or `h:mm:ss`; empty for unknown durations.
pub fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return if secs == 0.0 { "0:00".into() } else { String::new() };
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Decodes `%XX` escapes (invalid escapes are kept verbatim).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Converts a `file://` URI into a local path.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Skip an optional host part ("file://localhost/...").
    let rest = &rest[rest.find('/')?..];
    Some(PathBuf::from(percent_decode(rest)))
}

/// xorshift64* — plenty for shuffling a playlist.
pub struct Rng(u64);

impl Rng {
    pub fn new() -> Rng {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Rng::with_seed(t ^ ((std::process::id() as u64) << 32))
    }

    pub fn with_seed(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish integer in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (((self.next_u64() >> 32) * n as u64) >> 32) as usize
    }
}

/// `$XDG_CONFIG_HOME/haste` or `~/.config/haste`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("haste")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(59.9), "0:59");
        assert_eq!(fmt_time(61.0), "1:01");
        assert_eq!(fmt_time(3600.0 + 62.0), "1:01:02");
        assert_eq!(fmt_time(-1.0), "");
    }

    #[test]
    fn uri_decoding() {
        assert_eq!(percent_decode("a%20b%zz%"), "a b%zz%");
        assert_eq!(percent_decode("%D0%9F"), "П");
        assert_eq!(file_uri_to_path("file:///music/a%20b.mp3"), Some(PathBuf::from("/music/a b.mp3")));
        assert_eq!(file_uri_to_path("file://localhost/x.flac"), Some(PathBuf::from("/x.flac")));
        assert_eq!(file_uri_to_path("http://x/y.mp3"), None);
    }

    #[test]
    fn rng_range() {
        let mut r = Rng::with_seed(42);
        let mut seen = [false; 7];
        for _ in 0..1000 {
            seen[r.below(7)] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn extensions() {
        assert!(is_audio(Path::new("/a/B.FLAC")));
        assert!(!is_audio(Path::new("/a/cover.jpg")));
        assert!(is_playlist(Path::new("x.M3U8")));
    }
}
