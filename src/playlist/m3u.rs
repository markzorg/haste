//! M3U / M3U8 import and export.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::Track;
use crate::util::file_uri_to_path;

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// Text after the comma in `#EXTINF` (usually "Artist - Title").
    pub title: Option<String>,
    /// Seconds from `#EXTINF`, `None` when absent or negative.
    pub duration: Option<f64>,
}

/// Decodes playlist bytes: UTF-8 (with or without BOM), else Latin-1.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Parses playlist text. Relative paths are resolved against `base`
/// (the directory containing the playlist). Network URLs are skipped.
pub fn parse(text: &str, base: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut info: Option<(Option<f64>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if let Some(ext) = rest.strip_prefix("EXTINF:") {
                info = Some(parse_extinf(ext));
            }
            continue;
        }
        let (duration, title) = info.take().unwrap_or((None, None));
        let path = if line.starts_with("file://") {
            match file_uri_to_path(line) {
                Some(p) => p,
                None => continue,
            }
        } else if line.contains("://") {
            continue;
        } else {
            // Playlists written on Windows use backslashes.
            let p = if line.contains('\\') && !line.contains('/') { line.replace('\\', "/") } else { line.to_owned() };
            base.join(p)
        };
        out.push(Entry { path, title, duration });
    }
    out
}

fn parse_extinf(s: &str) -> (Option<f64>, Option<String>) {
    // First comma outside of quoted attribute values.
    let mut quoted = false;
    let comma = s.char_indices().find(|&(_, c)| {
        quoted ^= c == '"';
        c == ',' && !quoted
    });
    let (head, title) = match comma.map(|(i, _)| i) {
        Some(i) => (&s[..i], Some(s[i + 1..].trim().to_owned()).filter(|t| !t.is_empty())),
        None => (s, None),
    };
    // The duration may be followed by attributes: `#EXTINF:123 tvg-id="x",Title`.
    let dur = head.split_whitespace().next().and_then(|d| d.parse::<f64>().ok()).filter(|d| *d >= 0.0);
    (dur, title)
}

/// Serializes tracks as extended M3U (UTF-8). Paths inside `base` are
/// written relative to it so the playlist stays portable.
pub fn write<'a>(tracks: impl IntoIterator<Item = &'a Track>, base: Option<&Path>) -> String {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        let dur = if t.duration > 0.0 { t.duration.round() as i64 } else { -1 };
        let _ = writeln!(out, "#EXTINF:{dur},{}", t.display_name().replace(['\n', '\r'], " "));
        let path = base.and_then(|b| t.path.strip_prefix(b).ok()).unwrap_or(&t.path);
        out.push_str(&path.to_string_lossy());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_m3u() {
        let text = "#EXTM3U\n\
                    #EXTINF:123,Artist - Song\n\
                    music/song.mp3\n\
                    \n\
                    #EXTINF:-1,Stream\n\
                    http://radio.example/stream\n\
                    /abs/other.flac\n";
        let entries = parse(text, Path::new("/lists"));
        assert_eq!(
            entries,
            vec![
                Entry {
                    path: PathBuf::from("/lists/music/song.mp3"),
                    title: Some("Artist - Song".into()),
                    duration: Some(123.0)
                },
                Entry { path: PathBuf::from("/abs/other.flac"), title: None, duration: None },
            ]
        );
    }

    #[test]
    fn extinf_info_is_not_carried_over_skipped_urls() {
        let entries = parse("#EXTINF:5,Radio\nhttp://x/y\nlocal.ogg\n", Path::new("/b"));
        assert_eq!(entries, vec![Entry { path: PathBuf::from("/b/local.ogg"), title: None, duration: None }]);
    }

    #[test]
    fn handles_uris_windows_paths_bom_and_crlf() {
        let text = decode(b"\xEF\xBB\xBF#EXTM3U\r\nfile:///m/a%20b.mp3\r\nsub\\dir\\c.flac\r\n");
        let entries = parse(&text, Path::new("/base"));
        assert_eq!(entries[0].path, PathBuf::from("/m/a b.mp3"));
        assert_eq!(entries[1].path, PathBuf::from("/base/sub/dir/c.flac"));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn extinf_with_attributes() {
        assert_eq!(parse_extinf("200 tvg-id=\"a,b\",T"), (Some(200.0), Some("T".into())));
        assert_eq!(parse_extinf("42,"), (Some(42.0), None));
        assert_eq!(parse_extinf("x,Name"), (None, Some("Name".into())));
    }

    #[test]
    fn latin1_fallback() {
        assert_eq!(decode(b"caf\xe9.mp3"), "café.mp3");
    }

    #[test]
    fn round_trip() {
        let tracks = vec![
            Track::new("/music/a/1.flac".into(), "One".into(), "Band".into(), "LP".into(), 61.4),
            Track::new("/elsewhere/2.mp3".into(), "Two".into(), String::new(), String::new(), 0.0),
        ];
        let text = write(&tracks, Some(Path::new("/music")));
        assert_eq!(text, "#EXTM3U\n#EXTINF:61,Band - One\na/1.flac\n#EXTINF:-1,Two\n/elsewhere/2.mp3\n");
        let back = parse(&text, Path::new("/music"));
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].path, tracks[0].path);
        assert_eq!(back[0].duration, Some(61.0));
        assert_eq!(back[1].path, tracks[1].path);
        assert_eq!(back[1].title.as_deref(), Some("Two"));
    }
}
