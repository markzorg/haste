//! "Now playing" panel: cover, titles, seek bar and transport controls.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use super::tr;
use crate::playlist::Repeat;

pub struct Panel {
    pub root: gtk::Box,
    pub cover: gtk::Picture,
    pub title: gtk::Label,
    pub info: gtk::Label,
    pub pos: gtk::Label,
    pub len: gtk::Label,
    pub seek: gtk::Scale,
    pub prev: gtk::Button,
    pub play: gtk::Button,
    pub stop: gtk::Button,
    pub next: gtk::Button,
    pub shuffle: gtk::ToggleButton,
    pub repeat: gtk::Button,
    pub eq: gtk::ToggleButton,
    pub volume: gtk::Scale,
    pub volume_icon: gtk::Image,
}

const COVER: i32 = 104;

fn button(icon: &str, tip: &'static str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tr(tip)));
    b.add_css_class("flat");
    b
}

fn label(css: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if !css.is_empty() {
        l.add_css_class(css);
    }
    l
}

impl Panel {
    pub fn new() -> Panel {
        let cover = gtk::Picture::new();
        cover.set_size_request(COVER, COVER);
        cover.set_content_fit(gtk::ContentFit::Cover);
        cover.set_can_shrink(true);
        cover.add_css_class("cover");
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&cover));
        frame.set_valign(gtk::Align::Center);
        frame.set_overflow(gtk::Overflow::Hidden);

        let title = label("track-title");
        let info = label("dim-label");
        let pos = label("numeric");
        let len = label("numeric");
        len.set_xalign(1.0);
        pos.set_width_chars(7);
        len.set_width_chars(7);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
        seek.set_draw_value(false);
        seek.set_hexpand(true);
        seek.set_sensitive(false);
        let seek_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        seek_row.append(&pos);
        seek_row.append(&seek);
        seek_row.append(&len);

        let prev = button("media-skip-backward-symbolic", "Previous");
        let play = button("media-playback-start-symbolic", "Play/Pause (Space)");
        let stop = button("media-playback-stop-symbolic", "Stop");
        let next = button("media-skip-forward-symbolic", "Next");
        let shuffle = gtk::ToggleButton::new();
        shuffle.set_icon_name("media-playlist-shuffle-symbolic");
        shuffle.set_tooltip_text(Some(tr("Shuffle")));
        shuffle.add_css_class("flat");
        let repeat = button("media-playlist-repeat-symbolic", "Repeat: off");
        let eq = gtk::ToggleButton::with_label("EQ");
        eq.set_tooltip_text(Some(tr("Equalizer")));
        eq.add_css_class("flat");
        let volume_icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        volume.set_draw_value(false);
        volume.set_size_request(110, -1);
        volume.set_tooltip_text(Some(tr("Volume")));

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        for b in [&prev, &play, &stop, &next] {
            controls.append(b);
        }
        controls.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        controls.append(&shuffle);
        controls.append(&repeat);
        controls.append(&eq);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        controls.append(&spacer);
        controls.append(&volume_icon);
        controls.append(&volume);

        let right = gtk::Box::new(gtk::Orientation::Vertical, 2);
        right.set_hexpand(true);
        right.set_valign(gtk::Align::Center);
        right.append(&title);
        right.append(&info);
        right.append(&seek_row);
        right.append(&controls);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("now-playing");
        root.append(&frame);
        root.append(&right);

        let p = Panel {
            root,
            cover,
            title,
            info,
            pos,
            len,
            seek,
            prev,
            play,
            stop,
            next,
            shuffle,
            repeat,
            eq,
            volume,
            volume_icon,
        };
        p.clear();
        p
    }

    pub fn clear(&self) {
        self.title.set_text("haste");
        self.info.set_text(tr("Nothing playing"));
        self.set_time(0.0, 0.0);
        self.seek.set_sensitive(false);
        self.set_cover(None);
    }

    pub fn set_time(&self, pos: f64, len: f64) {
        use crate::util::fmt_time;
        self.pos.set_text(&fmt_time(pos.max(0.0)));
        self.len.set_text(&if len > 0.0 { fmt_time(len) } else { "--:--".into() });
    }

    pub fn set_playing(&self, playing: bool) {
        let icon = if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" };
        self.play.set_icon_name(icon);
    }

    pub fn set_repeat(&self, r: Repeat) {
        let (icon, tip) = match r {
            Repeat::Off => ("media-playlist-repeat-symbolic", "Repeat: off"),
            Repeat::All => ("media-playlist-repeat-symbolic", "Repeat: all"),
            Repeat::One => ("media-playlist-repeat-song-symbolic", "Repeat: one"),
        };
        self.repeat.set_icon_name(icon);
        self.repeat.set_tooltip_text(Some(tr(tip)));
        if r == Repeat::Off {
            self.repeat.remove_css_class("active");
            self.repeat.set_opacity(0.55);
        } else {
            self.repeat.add_css_class("active");
            self.repeat.set_opacity(1.0);
        }
    }

    pub fn set_volume_icon(&self, v: f64) {
        let icon = match v {
            v if v <= 0.0 => "audio-volume-muted-symbolic",
            v if v < 34.0 => "audio-volume-low-symbolic",
            v if v < 67.0 => "audio-volume-medium-symbolic",
            _ => "audio-volume-high-symbolic",
        };
        self.volume_icon.set_icon_name(Some(icon));
    }

    /// Shows a texture, or the generic audio icon when `None`.
    pub fn set_cover(&self, tex: Option<gdk::Texture>) {
        match tex {
            Some(t) => self.cover.set_paintable(Some(&t)),
            None => {
                let display = self.cover.display();
                let theme = gtk::IconTheme::for_display(&display);
                let icon = theme.lookup_icon(
                    "audio-x-generic",
                    &["audio-x-generic-symbolic"],
                    COVER,
                    self.cover.scale_factor(),
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                );
                self.cover.set_paintable(Some(&icon));
            }
        }
    }
}

/// Loads embedded art, falling back to cover/folder images next to the file.
pub fn load_cover(bytes: Option<Vec<u8>>, track_path: &std::path::Path) -> Option<gdk::Texture> {
    if let Some(b) = bytes {
        if let Ok(t) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)) {
            return Some(t);
        }
    }
    gdk::Texture::from_file(&gio::File::for_path(folder_cover(track_path)?)).ok()
}

/// Finds cover.jpg/folder.png/... next to a track.
pub fn folder_cover(track_path: &std::path::Path) -> Option<std::path::PathBuf> {
    const NAMES: &[&str] = &["cover", "folder", "front", "albumart", "album"];
    let dir = track_path.parent()?;
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("jpg" | "jpeg" | "png" | "webp")) {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        if let Some(rank) = NAMES.iter().position(|n| *n == stem) {
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, p));
            }
        }
    }
    best.map(|(_, p)| p)
}
