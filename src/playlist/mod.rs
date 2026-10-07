//! Playlist model independent of GTK: tracks, sorting, filtering and the
//! play order (sequential / shuffle, repeat modes).

pub mod m3u;

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::util::Rng;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct Track {
    /// Unique per session; also encodes insertion order.
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds, `0.0` when unknown.
    pub duration: f64,
    /// Lower-cased "artist title album filename" for quick filtering.
    key: Box<str>,
}

impl Track {
    pub fn new(path: PathBuf, title: String, artist: String, album: String, duration: f64) -> Track {
        let title = if title.trim().is_empty() {
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            title
        };
        let file = path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
        let key = format!("{artist}\t{title}\t{album}\t{file}").to_lowercase().into_boxed_str();
        Track { id: NEXT_ID.fetch_add(1, Relaxed), path, title, artist, album, duration, key }
    }

    /// Placeholder used until tags are read.
    pub fn from_path(path: PathBuf) -> Track {
        Track::new(path, String::new(), String::new(), String::new(), 0.0)
    }

    /// `query` must be lower-case; every whitespace-separated word must match.
    pub fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|w| self.key.contains(w))
    }

    /// "Artist - Title" or just the title.
    pub fn display_name(&self) -> String {
        if self.artist.is_empty() { self.title.clone() } else { format!("{} - {}", self.artist, self.title) }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Column {
    Artist,
    Title,
    Album,
    Duration,
}

impl Column {
    pub const ALL: [Column; 4] = [Column::Artist, Column::Title, Column::Album, Column::Duration];

    pub fn name(self) -> &'static str {
        match self {
            Column::Artist => "artist",
            Column::Title => "title",
            Column::Album => "album",
            Column::Duration => "duration",
        }
    }

    pub fn from_name(s: &str) -> Option<Column> {
        Column::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// Case-insensitive comparison without allocating.
fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// Ordering for a column; ties fall back to album/title and finally to
/// insertion order so that sorting is stable and albums stay together.
pub fn compare(a: &Track, b: &Track, col: Column) -> Ordering {
    let primary = match col {
        Column::Artist => cmp_text(&a.artist, &b.artist).then_with(|| cmp_text(&a.album, &b.album)),
        Column::Title => cmp_text(&a.title, &b.title),
        Column::Album => cmp_text(&a.album, &b.album),
        Column::Duration => a.duration.total_cmp(&b.duration),
    };
    primary.then(a.id.cmp(&b.id))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycle(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }

    pub fn from_name(s: &str) -> Repeat {
        match s {
            "all" => Repeat::All,
            "one" => Repeat::One,
            _ => Repeat::Off,
        }
    }
}

/// Decides which track comes next. Works on positions of the current view
/// (`len` items, `id_at(i)` gives the track id at position `i`).
pub struct Order {
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Tracks already played in the current shuffle cycle.
    played: HashSet<u64>,
    /// Shuffle history for "previous".
    history: Vec<u64>,
    rng: Rng,
}

impl Default for Order {
    fn default() -> Self {
        Order::new(Rng::new())
    }
}

impl Order {
    pub fn new(rng: Rng) -> Order {
        Order { shuffle: false, repeat: Repeat::Off, played: HashSet::new(), history: Vec::new(), rng }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        self.played.clear();
        self.history.clear();
    }

    /// Records that a track started playing.
    pub fn started(&mut self, id: u64) {
        if self.shuffle {
            self.played.insert(id);
            if self.history.last() != Some(&id) {
                self.history.push(id);
                if self.history.len() > 1000 {
                    self.history.drain(..500);
                }
            }
        }
    }

    /// Next position. `auto` is true when the previous track ended by itself
    /// (repeat-one and repeat-off only apply then).
    pub fn next(&mut self, cur: Option<usize>, len: usize, id_at: impl Fn(usize) -> u64, auto: bool) -> Option<usize> {
        if len == 0 {
            return None;
        }
        if auto && self.repeat == Repeat::One {
            return cur.filter(|&c| c < len).or(Some(0));
        }
        if self.shuffle {
            return self.next_shuffled(cur, len, id_at, auto);
        }
        match cur {
            None => Some(0),
            Some(c) if c + 1 < len => Some(c + 1),
            Some(_) if auto && self.repeat == Repeat::Off => None,
            Some(_) => Some(0),
        }
    }

    fn next_shuffled(&mut self, cur: Option<usize>, len: usize, id_at: impl Fn(usize) -> u64, auto: bool) -> Option<usize> {
        let fresh = |i: usize, played: &HashSet<u64>| Some(i) != cur && !played.contains(&id_at(i));
        // Cheap rejection sampling first, exact scan when few tracks are left.
        for _ in 0..32 {
            let i = self.rng.below(len);
            if fresh(i, &self.played) {
                return Some(i);
            }
        }
        let left: Vec<usize> = (0..len).filter(|&i| fresh(i, &self.played)).collect();
        if !left.is_empty() {
            return Some(left[self.rng.below(left.len())]);
        }
        // Everything has been played once.
        if auto && self.repeat == Repeat::Off {
            return None;
        }
        self.played.clear();
        if len == 1 {
            return Some(0);
        }
        loop {
            let i = self.rng.below(len);
            if Some(i) != cur {
                return Some(i);
            }
        }
    }

    /// Previous position: shuffle history first, otherwise the row above.
    pub fn prev(&mut self, cur: Option<usize>, len: usize, find: impl Fn(u64) -> Option<usize>) -> Option<usize> {
        if len == 0 {
            return None;
        }
        if self.shuffle {
            // The top of the history is the current track itself.
            self.history.pop();
            while let Some(id) = self.history.pop() {
                if let Some(pos) = find(id) {
                    return Some(pos);
                }
            }
        }
        match cur {
            Some(c) if c > 0 && c <= len => Some(c - 1),
            Some(_) if self.repeat == Repeat::All => Some(len - 1),
            _ => Some(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, title: &str, album: &str, dur: f64) -> Track {
        Track::new(PathBuf::from(format!("/m/{title}.mp3")), title.into(), artist.into(), album.into(), dur)
    }

    fn order() -> Order {
        Order::new(Rng::with_seed(7))
    }

    #[test]
    fn title_falls_back_to_file_stem() {
        let t = Track::new(PathBuf::from("/x/01 Song.flac"), " ".into(), String::new(), String::new(), 0.0);
        assert_eq!(t.title, "01 Song");
        assert_eq!(t.display_name(), "01 Song");
    }

    #[test]
    fn filter_matches_all_words_case_insensitively() {
        let t = track("The Beatles", "Let It Be", "Let It Be", 243.0);
        assert!(t.matches("beatles"));
        assert!(t.matches("let beat"));
        assert!(t.matches("it.mp3") || t.matches("let it be.mp3"));
        assert!(!t.matches("stones"));
        assert!(t.matches(""));
    }

    #[test]
    fn sorting_is_case_insensitive_and_stable() {
        let a = track("abba", "Waterloo", "Waterloo", 160.0);
        let b = track("ABBA", "Mamma Mia", "ABBA", 210.0);
        let c = track("Beatles", "Help", "Help!", 140.0);
        // Same artist: album decides.
        assert_eq!(compare(&b, &a, Column::Artist), Ordering::Less);
        assert_eq!(compare(&a, &c, Column::Artist), Ordering::Less);
        assert_eq!(compare(&c, &b, Column::Title), Ordering::Less);
        assert_eq!(compare(&c, &a, Column::Duration), Ordering::Less);
        // Equal keys keep insertion order.
        let d = track("x", "same", "", 1.0);
        let e = track("x", "same", "", 1.0);
        assert_eq!(compare(&d, &e, Column::Title), Ordering::Less);
        assert_eq!(compare(&e, &d, Column::Title), Ordering::Greater);
    }

    #[test]
    fn sequential_order_and_repeat_modes() {
        let mut o = order();
        let id = |i: usize| i as u64;
        assert_eq!(o.next(None, 3, id, true), Some(0));
        assert_eq!(o.next(Some(0), 3, id, true), Some(1));
        assert_eq!(o.next(Some(2), 3, id, true), None);
        // A manual "next" at the end wraps around.
        assert_eq!(o.next(Some(2), 3, id, false), Some(0));
        o.repeat = Repeat::All;
        assert_eq!(o.next(Some(2), 3, id, true), Some(0));
        o.repeat = Repeat::One;
        assert_eq!(o.next(Some(1), 3, id, true), Some(1));
        assert_eq!(o.next(Some(1), 3, id, false), Some(2));
        assert_eq!(o.next(None, 0, id, false), None);
    }

    #[test]
    fn prev_moves_up_and_wraps_with_repeat_all() {
        let mut o = order();
        assert_eq!(o.prev(Some(2), 3, |_| None), Some(1));
        assert_eq!(o.prev(Some(0), 3, |_| None), Some(0));
        o.repeat = Repeat::All;
        assert_eq!(o.prev(Some(0), 3, |_| None), Some(2));
    }

    #[test]
    fn shuffle_plays_every_track_once_then_stops() {
        let mut o = order();
        o.set_shuffle(true);
        let len = 50;
        let mut seen = HashSet::new();
        let mut cur = None;
        for _ in 0..len {
            let n = o.next(cur, len, |i| i as u64, true).expect("track");
            assert!(seen.insert(n), "repeated {n}");
            o.started(n as u64);
            cur = Some(n);
        }
        assert_eq!(o.next(cur, len, |i| i as u64, true), None);
        o.repeat = Repeat::All;
        assert!(o.next(cur, len, |i| i as u64, true).is_some());
    }

    #[test]
    fn shuffle_prev_walks_history() {
        let mut o = order();
        o.set_shuffle(true);
        let mut cur = None;
        let mut played = Vec::new();
        for _ in 0..5 {
            let n = o.next(cur, 10, |i| i as u64, true).expect("track");
            o.started(n as u64);
            played.push(n);
            cur = Some(n);
        }
        let find = |id: u64| Some(id as usize);
        assert_eq!(o.prev(cur, 10, find), Some(played[3]));
        o.started(played[3] as u64);
        assert_eq!(o.prev(Some(played[3]), 10, find), Some(played[2]));
    }

    #[test]
    fn names_round_trip() {
        for c in Column::ALL {
            assert_eq!(Column::from_name(c.name()), Some(c));
        }
        for r in [Repeat::Off, Repeat::All, Repeat::One] {
            assert_eq!(Repeat::from_name(r.name()), r);
            assert_ne!(r.cycle(), r);
        }
    }
}
