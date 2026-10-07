//! Session persistence in `~/.config/haste/`:
//! `session.conf` (key=value) and `playlist.tsv` (tracks with cached tags,
//! so large playlists load instantly without re-reading files).

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::playlist::{Column, Repeat, Track};
use crate::util::config_dir;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Slider value `0..=1`.
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    /// Index of the current track in playlist (insertion) order.
    pub current: Option<usize>,
    pub position: f64,
    pub sort: Option<(Column, bool)>,
    pub last_dir: Option<PathBuf>,
    pub eq_enabled: bool,
    pub eq_preamp: f32,
    pub eq_gains: [f32; 10],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume: 0.8,
            shuffle: false,
            repeat: Repeat::Off,
            width: 860,
            height: 600,
            maximized: false,
            current: None,
            position: 0.0,
            sort: None,
            last_dir: None,
            eq_enabled: false,
            eq_preamp: 0.0,
            eq_gains: [0.0; 10],
        }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "volume" => s.volume = v.parse().unwrap_or(s.volume),
                "shuffle" => s.shuffle = v == "true",
                "repeat" => s.repeat = Repeat::from_name(v),
                "width" => s.width = v.parse().unwrap_or(s.width),
                "height" => s.height = v.parse().unwrap_or(s.height),
                "maximized" => s.maximized = v == "true",
                "current" => s.current = v.parse().ok(),
                "position" => s.position = v.parse().unwrap_or(0.0),
                "sort" => {
                    let (col, dir) = v.split_once(',').unwrap_or((v, "asc"));
                    s.sort = Column::from_name(col).map(|c| (c, dir == "desc"));
                }
                "last_dir" if !v.is_empty() => s.last_dir = Some(PathBuf::from(v)),
                "eq_enabled" => s.eq_enabled = v == "true",
                "eq_preamp" => s.eq_preamp = v.parse().unwrap_or(0.0),
                "eq_bands" => {
                    for (g, x) in s.eq_gains.iter_mut().zip(v.split(',')) {
                        *g = x.trim().parse().unwrap_or(0.0);
                    }
                }
                _ => {}
            }
        }
        s.volume = s.volume.clamp(0.0, 1.0);
        s.width = s.width.clamp(320, 10_000);
        s.height = s.height.clamp(240, 10_000);
        s
    }

    pub fn serialize(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "volume={}", self.volume);
        let _ = writeln!(o, "shuffle={}", self.shuffle);
        let _ = writeln!(o, "repeat={}", self.repeat.name());
        let _ = writeln!(o, "width={}", self.width);
        let _ = writeln!(o, "height={}", self.height);
        let _ = writeln!(o, "maximized={}", self.maximized);
        if let Some(c) = self.current {
            let _ = writeln!(o, "current={c}");
            let _ = writeln!(o, "position={:.3}", self.position);
        }
        if let Some((col, desc)) = self.sort {
            let _ = writeln!(o, "sort={},{}", col.name(), if desc { "desc" } else { "asc" });
        }
        if let Some(d) = &self.last_dir {
            let _ = writeln!(o, "last_dir={}", d.display());
        }
        let _ = writeln!(o, "eq_enabled={}", self.eq_enabled);
        let _ = writeln!(o, "eq_preamp={}", self.eq_preamp);
        let bands: Vec<String> = self.eq_gains.iter().map(|g| g.to_string()).collect();
        let _ = writeln!(o, "eq_bands={}", bands.join(","));
        o
    }
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

const TSV_HEADER: &str = "#haste-playlist v1";

/// One line per track: `path \t duration \t artist \t title \t album`.
pub fn serialize_playlist<'a>(tracks: impl IntoIterator<Item = &'a Track>) -> String {
    let mut o = String::from(TSV_HEADER);
    o.push('\n');
    for t in tracks {
        escape(&t.path.to_string_lossy(), &mut o);
        let _ = write!(o, "\t{:.3}\t", t.duration);
        escape(&t.artist, &mut o);
        o.push('\t');
        escape(&t.title, &mut o);
        o.push('\t');
        escape(&t.album, &mut o);
        o.push('\n');
    }
    o
}

pub fn parse_playlist(text: &str) -> Vec<Track> {
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let mut f = line.split('\t');
            let path = PathBuf::from(unescape(f.next()?));
            let duration = f.next().and_then(|d| d.parse().ok()).unwrap_or(0.0);
            let mut next = || f.next().map(unescape).unwrap_or_default();
            let (artist, title, album) = (next(), next(), next());
            Some(Track::new(path, title, artist, album, duration))
        })
        .collect()
}

/// Writes atomically (temp file + rename) so a crash never truncates data.
fn write_atomic(path: &Path, data: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

fn read(name: &str) -> String {
    std::fs::read(config_dir().join(name)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

pub fn load() -> (Settings, Vec<Track>) {
    (Settings::parse(&read("session.conf")), parse_playlist(&read("playlist.tsv")))
}

pub fn save(settings: &Settings, playlist: &str) -> io::Result<()> {
    let dir = config_dir();
    write_atomic(&dir.join("playlist.tsv"), playlist)?;
    write_atomic(&dir.join("session.conf"), &settings.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            volume: 0.5,
            shuffle: true,
            repeat: Repeat::One,
            width: 1000,
            height: 700,
            maximized: true,
            current: Some(12),
            position: 33.5,
            sort: Some((Column::Album, true)),
            last_dir: Some(PathBuf::from("/home/u/Music")),
            eq_enabled: true,
            eq_preamp: -3.0,
            eq_gains: [5.0, 4.0, 3.0, 1.0, -1.0, -1.5, 1.0, 3.0, 4.0, 5.0],
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn settings_tolerate_garbage() {
        let s = Settings::parse("volume=7\nwidth=abc\nnonsense\nsort=nope,asc\n");
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.width, Settings::default().width);
        assert_eq!(s.sort, None);
    }

    #[test]
    fn playlist_round_trip_with_special_chars() {
        let tracks = vec![
            Track::new("/m/tab\there.mp3".into(), "Ti\\tle".into(), "Art\nist".into(), "Al\tbum".into(), 12.5),
            Track::new("/m/b.flac".into(), String::new(), String::new(), String::new(), 0.0),
        ];
        let back = parse_playlist(&serialize_playlist(&tracks));
        assert_eq!(back.len(), 2);
        for (a, b) in tracks.iter().zip(&back) {
            assert_eq!((&a.path, &a.title, &a.artist, &a.album, a.duration), (&b.path, &b.title, &b.artist, &b.album, b.duration));
        }
    }
}
