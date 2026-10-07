//! MPRIS 2 (media keys, GNOME/XFCE sound menus) over GDBus. libgio is
//! already linked for GTK, so this costs a few KiB instead of a D-Bus crate.

use std::cell::RefCell;
use std::path::PathBuf;

use glib::Variant;
use glib::variant::ObjectPath;
use gtk::prelude::*;
use gtk::{gio, glib};

use super::{App, State, list::track, with};
use crate::playlist::Repeat;

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

const XML: &str = r#"<node>
<interface name="org.mpris.MediaPlayer2">
 <method name="Raise"/><method name="Quit"/>
 <property name="CanQuit" type="b" access="read"/>
 <property name="CanRaise" type="b" access="read"/>
 <property name="HasTrackList" type="b" access="read"/>
 <property name="Identity" type="s" access="read"/>
 <property name="DesktopEntry" type="s" access="read"/>
 <property name="SupportedUriSchemes" type="as" access="read"/>
 <property name="SupportedMimeTypes" type="as" access="read"/>
</interface>
<interface name="org.mpris.MediaPlayer2.Player">
 <method name="Next"/><method name="Previous"/><method name="Pause"/>
 <method name="PlayPause"/><method name="Stop"/><method name="Play"/>
 <method name="Seek"><arg direction="in" name="Offset" type="x"/></method>
 <method name="SetPosition"><arg direction="in" name="TrackId" type="o"/><arg direction="in" name="Position" type="x"/></method>
 <method name="OpenUri"><arg direction="in" name="Uri" type="s"/></method>
 <signal name="Seeked"><arg name="Position" type="x"/></signal>
 <property name="PlaybackStatus" type="s" access="read"/>
 <property name="LoopStatus" type="s" access="readwrite"/>
 <property name="Rate" type="d" access="readwrite"/>
 <property name="Shuffle" type="b" access="readwrite"/>
 <property name="Metadata" type="a{sv}" access="read"/>
 <property name="Volume" type="d" access="readwrite"/>
 <property name="Position" type="x" access="read"/>
 <property name="MinimumRate" type="d" access="read"/>
 <property name="MaximumRate" type="d" access="read"/>
 <property name="CanGoNext" type="b" access="read"/>
 <property name="CanGoPrevious" type="b" access="read"/>
 <property name="CanPlay" type="b" access="read"/>
 <property name="CanPause" type="b" access="read"/>
 <property name="CanSeek" type="b" access="read"/>
 <property name="CanControl" type="b" access="read"/>
</interface>
</node>"#;

const MIME: &[&str] = &[
    "audio/mpeg", "audio/flac", "audio/x-flac", "audio/ogg", "audio/x-vorbis+ogg",
    "audio/wav", "audio/x-wav", "audio/aac", "audio/mp4", "audio/x-m4a", "audio/x-mpegurl",
];

thread_local! {
    static CONN: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
}

/// Claims `org.mpris.MediaPlayer2.haste` on the session bus.
pub fn start() {
    let _ = gio::bus_own_name(
        gio::BusType::Session,
        "org.mpris.MediaPlayer2.haste",
        gio::BusNameOwnerFlags::DO_NOT_QUEUE,
        |conn, _| register(&conn),
        |_, _| {},
        |_, _| {},
    );
}

fn register(conn: &gio::DBusConnection) {
    let Ok(node) = gio::DBusNodeInfo::for_xml(XML) else { return };
    for name in [ROOT, PLAYER] {
        let Some(iface) = node.lookup_interface(name) else { continue };
        let _ = conn
            .register_object(PATH, &iface)
            .method_call(|_, _, _, iface, method, args, inv| {
                with(|a| call(a, iface.unwrap_or(PLAYER), method, &args));
                inv.return_value(None);
            })
            .property(|_, _, _, iface, prop| {
                let mut v = None;
                with(|a| v = Some(if iface == ROOT { root_prop(prop) } else { player_prop(a, prop) }));
                v.unwrap_or_else(|| false.to_variant())
            })
            .set_property(|_, _, _, _, prop, value| {
                with(|a| set(a, prop, &value));
                true
            })
            .build();
    }
    CONN.set(Some(conn.clone()));
}

fn call(a: &App, iface: &str, method: &str, args: &Variant) {
    match (iface, method) {
        (ROOT, "Raise") => a.window.present(),
        (ROOT, "Quit") => a.window.close(),
        (_, "Next") => a.next(false),
        (_, "Previous") => a.prev(),
        (_, "PlayPause") => a.toggle(),
        (_, "Pause") if a.state.get() == State::Playing => a.toggle(),
        (_, "Play") if a.state.get() != State::Playing => a.toggle(),
        (_, "Stop") => a.stop(),
        (_, "Seek") => {
            if let Some((off,)) = args.get::<(i64,)>() {
                a.seek_to(a.player.position() + off as f64 / 1e6);
            }
        }
        (_, "SetPosition") => {
            if let Some((id, pos)) = args.get::<(ObjectPath, i64)>() {
                if id.as_str() == track_id(a).as_str() {
                    a.seek_to(pos as f64 / 1e6);
                }
            }
        }
        (_, "OpenUri") => {
            if let Some(path) = args.get::<(String,)>().and_then(|(u,)| crate::util::file_uri_to_path(&u)) {
                a.add_paths(vec![path], true);
            }
        }
        _ => {}
    }
}

fn root_prop(prop: &str) -> Variant {
    match prop {
        "CanQuit" | "CanRaise" => true.to_variant(),
        "HasTrackList" => false.to_variant(),
        "Identity" => "haste".to_variant(),
        "DesktopEntry" => super::APP_ID.to_variant(),
        "SupportedUriSchemes" => vec!["file"].to_variant(),
        "SupportedMimeTypes" => MIME.to_vec().to_variant(),
        _ => false.to_variant(),
    }
}

fn track_id(a: &App) -> ObjectPath {
    let path = match a.current.borrow().as_ref() {
        Some(o) => format!("/io/github/markzorg/Haste/track/{}", track(o).id),
        None => "/org/mpris/MediaPlayer2/TrackList/NoTrack".into(),
    };
    ObjectPath::try_from(path).expect("valid object path")
}

fn metadata(a: &App) -> Variant {
    let dict = glib::VariantDict::new(None);
    dict.insert_value("mpris:trackid", &track_id(a).to_variant());
    if let Some(o) = a.current.borrow().as_ref() {
        let t = track(o);
        if a.duration.get() > 0.0 {
            dict.insert_value("mpris:length", &((a.duration.get() * 1e6) as i64).to_variant());
        }
        dict.insert_value("xesam:title", &t.title.to_variant());
        if !t.artist.is_empty() {
            dict.insert_value("xesam:artist", &vec![t.artist.clone()].to_variant());
        }
        if !t.album.is_empty() {
            dict.insert_value("xesam:album", &t.album.to_variant());
        }
        let file = gio::File::for_path(&t.path);
        dict.insert_value("xesam:url", &file.uri().to_variant());
        if let Some(art) = a.art.borrow().as_ref() {
            dict.insert_value("mpris:artUrl", &gio::File::for_path(art).uri().to_variant());
        }
    }
    dict.end()
}

fn player_prop(a: &App, prop: &str) -> Variant {
    match prop {
        "PlaybackStatus" => match a.state.get() {
            State::Playing => "Playing",
            State::Paused => "Paused",
            State::Stopped => "Stopped",
        }
        .to_variant(),
        "LoopStatus" => match a.order.borrow().repeat {
            Repeat::Off => "None",
            Repeat::One => "Track",
            Repeat::All => "Playlist",
        }
        .to_variant(),
        "Rate" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
        "Shuffle" => a.order.borrow().shuffle.to_variant(),
        "Metadata" => metadata(a),
        "Volume" => (a.panel.volume.value() / 100.0).to_variant(),
        "Position" => ((a.player.position() * 1e6) as i64).to_variant(),
        "CanSeek" => (a.duration.get() > 0.0).to_variant(),
        _ => true.to_variant(),
    }
}

fn set(a: &App, prop: &str, value: &Variant) {
    match prop {
        "LoopStatus" => {
            let r = match value.get::<String>().as_deref() {
                Some("Track") => Repeat::One,
                Some("Playlist") => Repeat::All,
                _ => Repeat::Off,
            };
            a.set_repeat(r);
        }
        "Shuffle" => a.panel.shuffle.set_active(value.get::<bool>().unwrap_or(false)),
        "Volume" => a.panel.volume.set_value(value.get::<f64>().unwrap_or(0.0).clamp(0.0, 1.0) * 100.0),
        _ => {}
    }
}

/// Emits `PropertiesChanged` for the given player properties.
pub fn changed(props: &[&str]) {
    let Some(conn) = CONN.with_borrow(|c| c.clone()) else { return };
    let dict = glib::VariantDict::new(None);
    with(|a| {
        for p in props {
            dict.insert_value(p, &player_prop(a, p));
        }
    });
    let args = Variant::tuple_from_iter([PLAYER.to_variant(), dict.end(), Vec::<String>::new().to_variant()]);
    let _ = conn.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&args));
}

/// Emits `Seeked` after a jump in position.
pub fn seeked(secs: f64) {
    let Some(conn) = CONN.with_borrow(|c| c.clone()) else { return };
    let args = Variant::tuple_from_iter([((secs * 1e6) as i64).to_variant()]);
    let _ = conn.emit_signal(None, PATH, PLAYER, "Seeked", Some(&args));
}

/// Stores embedded cover art where `mpris:artUrl` can point to it.
pub fn write_art(id: u64, bytes: &[u8]) -> Option<PathBuf> {
    let dir = glib::user_runtime_dir().join("haste");
    std::fs::create_dir_all(&dir).ok()?;
    // Keep only the latest file; a new name per track defeats URL caching.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let path = dir.join(format!("cover-{id}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}
