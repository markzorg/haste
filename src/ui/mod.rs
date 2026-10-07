//! GTK user interface. Everything here runs on the main thread; background
//! threads talk to it through an mpsc channel plus `MainContext::invoke`.

mod eq;
mod list;
#[cfg(feature = "mpris")]
mod mpris;
mod panel;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::audio::{Command, Event, Player};
use crate::config::{self, Settings};
use crate::library::{self, ScanEvent};
use crate::playlist::{Order, Repeat, Track, m3u};
use crate::util::{AUDIO_EXTS, PLAYLIST_EXTS, fmt_time};
use list::{List, track, wrap};
use panel::Panel;

pub const APP_ID: &str = "io.github.markzorg.Haste";

// ---------------------------------------------------------------- i18n --

const RU: &[(&str, &str)] = &[
    ("Artist", "Исполнитель"),
    ("Title", "Название"),
    ("Album", "Альбом"),
    ("Time", "Время"),
    ("Previous", "Предыдущий"),
    ("Play/Pause (Space)", "Играть/пауза (пробел)"),
    ("Stop", "Стоп"),
    ("Next", "Следующий"),
    ("Shuffle", "Перемешать"),
    ("Repeat: off", "Повтор: выкл"),
    ("Repeat: all", "Повтор: весь список"),
    ("Repeat: one", "Повтор: один трек"),
    ("Volume", "Громкость"),
    ("Nothing playing", "Ничего не играет"),
    ("Add files…", "Добавить файлы…"),
    ("Add folder…", "Добавить папку…"),
    ("Import playlist…", "Импорт плейлиста…"),
    ("Export playlist…", "Экспорт плейлиста…"),
    ("Remove selected", "Удалить выбранные"),
    ("Clear playlist", "Очистить плейлист"),
    ("Original order", "Исходный порядок"),
    ("Quit", "Выход"),
    ("Search (Ctrl+F)", "Поиск (Ctrl+F)"),
    ("Audio files and playlists", "Аудиофайлы и плейлисты"),
    ("Playlists", "Плейлисты"),
    ("Tracks", "Треков"),
    ("shown", "показано"),
    ("scanning…", "сканирование…"),
    ("End of playlist", "Конец плейлиста"),
    ("Cannot save playlist", "Не удалось сохранить плейлист"),
    ("Menu", "Меню"),
    ("Equalizer", "Эквалайзер"),
    ("Enabled", "Включён"),
    ("Preamp", "Предусил."),
    ("Reset", "Сброс"),
    ("Custom", "Свой"),
    ("Flat", "Ровно"),
    ("Rock", "Рок"),
    ("Pop", "Поп"),
    ("Jazz", "Джаз"),
    ("Classical", "Классика"),
    ("Bass boost", "Больше баса"),
    ("Treble boost", "Больше верхов"),
    ("Vocal", "Вокал"),
];

fn is_ru() -> bool {
    static RU_LOCALE: OnceLock<bool> = OnceLock::new();
    *RU_LOCALE.get_or_init(|| {
        ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty())
            .is_some_and(|v| v.starts_with("ru"))
    })
}

/// Translates a UI string (English → Russian when the locale is Russian).
pub fn tr(s: &'static str) -> &'static str {
    if !is_ru() {
        return s;
    }
    RU.iter().find(|(en, _)| *en == s).map_or(s, |(_, ru)| ru)
}

// --------------------------------------------------------------- state --

enum Msg {
    Audio(Event),
    Scan(ScanEvent),
    /// Decoded cover art (and a file with it, for MPRIS) for a track id.
    Cover(u64, Option<gdk::Texture>, Option<PathBuf>),
}

/// Tells D-Bus listeners (MPRIS) that player properties changed.
fn notify(props: &[&str]) {
    #[cfg(feature = "mpris")]
    mpris::changed(props);
    let _ = props;
}

fn notify_seeked(secs: f64) {
    #[cfg(feature = "mpris")]
    mpris::seeked(secs);
    #[cfg(not(feature = "mpris"))]
    let _ = secs;
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum State {
    Stopped,
    Playing,
    Paused,
}

struct App {
    window: gtk::ApplicationWindow,
    player: Player,
    list: List,
    panel: Panel,
    eq: eq::EqView,
    search: gtk::SearchEntry,
    status: gtk::Label,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    current: RefCell<Option<glib::Object>>,
    state: Cell<State>,
    duration: Cell<f64>,
    order: RefCell<Order>,
    scan_seq: Cell<u64>,
    /// Scans with a smaller id were cancelled by "clear".
    valid_from: Cell<u64>,
    scanning: Cell<u32>,
    cancel: RefCell<Arc<AtomicBool>>,
    /// Scan whose first track should start playing (0 = none).
    play_scan: Cell<u64>,
    seek_hold: Cell<Option<Instant>>,
    ticking: Cell<bool>,
    failures: Cell<u32>,
    total: Cell<f64>,
    last_dir: RefCell<Option<PathBuf>>,
    flash_until: Cell<Option<Instant>>,
    /// Image file with the current cover (for `mpris:artUrl`).
    art: RefCell<Option<PathBuf>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn with(f: impl FnOnce(&App)) {
    if let Some(app) = APP.with_borrow(|a| a.clone()) {
        f(&app);
    }
}

fn post(tx: &Sender<Msg>, msg: Msg) {
    if tx.send(msg).is_ok() {
        glib::MainContext::default().invoke_with_priority(glib::Priority::DEFAULT, dispatch);
    }
}

/// Runs on the main loop; drains messages from background threads.
fn dispatch() {
    with(|a| {
        while let Ok(msg) = a.rx.try_recv() {
            a.handle(msg);
        }
    });
}

const CSS: &str = "
.now-playing { padding: 10px 12px 6px 12px; }
.track-title { font-weight: bold; font-size: 1.2em; }
.playing { font-weight: bold; }
.statusbar { padding: 2px 8px; font-size: 0.9em; }
";

// g_unix_signal_add() lives in libglib (already linked); the Rust wrapper
// moved to a separate crate, so declare the one function we need.
unsafe extern "C" {
    fn g_unix_signal_add(
        signum: std::ffi::c_int,
        handler: extern "C" fn(*mut std::ffi::c_void) -> std::ffi::c_int,
        data: *mut std::ffi::c_void,
    ) -> std::ffi::c_uint;
}

/// SIGTERM/SIGINT/SIGHUP (session logout, Ctrl+C): save and quit cleanly.
/// Dispatched by the GLib main loop, so touching widgets is safe.
extern "C" fn on_signal(_: *mut std::ffi::c_void) -> std::ffi::c_int {
    with(|a| a.window.close());
    1 // G_SOURCE_CONTINUE
}

pub fn run() -> glib::ExitCode {
    for sig in [1, 2, 15] {
        // SAFETY: plain FFI call with a valid callback and no user data.
        unsafe { g_unix_signal_add(sig, on_signal, std::ptr::null_mut()) };
    }
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_startup(|app| {
        gtk::Window::set_default_icon_name(APP_ID);
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        add_actions(app);
    });
    app.connect_activate(|app| window(app).present());
    app.connect_open(|app, files, _| {
        window(app).present();
        let paths = files.iter().filter_map(|f| f.path()).collect();
        with(|a| a.add_paths(paths, true));
    });
    let code = app.run();
    // Dropping the player stops and joins the audio thread.
    APP.with_borrow_mut(|a| a.take());
    code
}

/// Action name, handler and keyboard accelerators.
type ActionDef = (&'static str, fn(&App), &'static [&'static str]);

fn add_actions(app: &gtk::Application) {
    let actions: [ActionDef; 10] = [
        ("add-files", App::add_files, &["<Control>o"]),
        ("add-folder", App::add_folder, &["<Control><Shift>o"]),
        ("import", App::import_playlist, &["<Control>i"]),
        ("export", App::export_playlist, &["<Control>s"]),
        ("find", |a| { a.search.grab_focus(); }, &["<Control>f"]),
        ("remove", App::remove_selected, &[]),
        ("clear", App::clear, &[]),
        ("unsort", |a| a.list.set_sort(None), &[]),
        ("next", |a| a.next(false), &["<Control>Right"]),
        ("prev", App::prev, &["<Control>Left"]),
    ];
    for (name, f, accels) in actions {
        let act = gio::SimpleAction::new(name, None);
        act.connect_activate(move |_, _| with(f));
        app.add_action(&act);
        app.set_accels_for_action(&format!("app.{name}"), accels);
    }
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(|_, _| with(|a| a.window.close()));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let sections: [&[(&'static str, &str)]; 4] = [
        &[("Add files…", "app.add-files"), ("Add folder…", "app.add-folder")],
        &[("Import playlist…", "app.import"), ("Export playlist…", "app.export")],
        &[("Remove selected", "app.remove"), ("Clear playlist", "app.clear"), ("Original order", "app.unsort")],
        &[("Quit", "app.quit")],
    ];
    for items in sections {
        let s = gio::Menu::new();
        for (label, action) in items {
            s.append(Some(tr(label)), Some(action));
        }
        menu.append_section(None, &s);
    }
    menu
}

/// Returns the main window, building it on first use.
fn window(app: &gtk::Application) -> gtk::ApplicationWindow {
    if let Some(w) = APP.with_borrow(|a| a.as_ref().map(|a| a.window.clone())) {
        return w;
    }
    let (tx, rx) = mpsc::channel();
    let ptx = tx.clone();
    let player = Player::new(move |ev| post(&ptx, Msg::Audio(ev)));
    let list = List::new();
    let panel = Panel::new();

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr("Search (Ctrl+F)")));
    search.set_hexpand(true);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    toolbar.add_css_class("toolbar");
    for (icon, action, tip) in [
        ("list-add-symbolic", "app.add-files", "Add files…"),
        ("folder-open-symbolic", "app.add-folder", "Add folder…"),
    ] {
        let b = gtk::Button::from_icon_name(icon);
        b.set_action_name(Some(action));
        b.set_tooltip_text(Some(tr(tip)));
        toolbar.append(&b);
    }
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_menu_model(Some(&menu()));
    menu_button.set_tooltip_text(Some(tr("Menu")));
    toolbar.append(&menu_button);
    toolbar.append(&search);

    let scroller = gtk::ScrolledWindow::builder().child(&list.view).vexpand(true).build();
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("statusbar");
    status.add_css_class("dim-label");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&panel.root);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&toolbar);
    root.append(&scroller);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&status);

    let window = gtk::ApplicationWindow::builder().application(app).title("haste").icon_name(APP_ID).child(&root).build();

    let eq = eq::EqView::new(&window);
    let app_state = Rc::new(App {
        window: window.clone(),
        player,
        list,
        panel,
        eq,
        search,
        status,
        rx,
        tx,
        current: RefCell::new(None),
        state: Cell::new(State::Stopped),
        duration: Cell::new(0.0),
        order: RefCell::new(Order::default()),
        scan_seq: Cell::new(0),
        valid_from: Cell::new(0),
        scanning: Cell::new(0),
        cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
        play_scan: Cell::new(0),
        seek_hold: Cell::new(None),
        ticking: Cell::new(false),
        failures: Cell::new(0),
        total: Cell::new(0.0),
        last_dir: RefCell::new(None),
        flash_until: Cell::new(None),
        art: RefCell::new(None),
    });
    APP.set(Some(app_state.clone()));
    connect(&app_state);
    app_state.restore(config::load());
    #[cfg(feature = "mpris")]
    mpris::start();
    window
}

fn connect(a: &App) {
    let p = &a.panel;
    p.play.connect_clicked(|_| with(App::toggle));
    p.stop.connect_clicked(|_| with(App::stop));
    p.next.connect_clicked(|_| with(|a| a.next(false)));
    p.prev.connect_clicked(|_| with(App::prev));
    p.shuffle.connect_toggled(|b| {
        let on = b.is_active();
        with(|a| a.order.borrow_mut().set_shuffle(on));
        notify(&["Shuffle"]);
    });
    p.repeat.connect_clicked(|_| {
        with(|a| {
            let r = a.order.borrow().repeat.cycle();
            a.set_repeat(r);
        })
    });
    p.eq.connect_toggled(|b| {
        let on = b.is_active();
        with(|a| if on { a.eq.window.present() } else { a.eq.window.set_visible(false) });
    });
    a.eq.window.connect_close_request(|_| {
        with(|a| a.panel.eq.set_active(false));
        glib::Propagation::Proceed
    });
    a.eq.connect_changed(|| {
        with(|a| {
            let (on, pre, gains) = a.eq.values();
            a.player.set_eq(on, pre, &gains);
        })
    });
    p.volume.connect_value_changed(|s| {
        let v = s.value();
        with(|a| {
            a.player.set_volume(v / 100.0);
            a.panel.set_volume_icon(v);
        });
        notify(&["Volume"]);
    });
    p.seek.connect_change_value(|_, _, v| {
        with(|a| a.seek_to(v));
        glib::Propagation::Proceed
    });

    a.list.view.connect_activate(|_, pos| {
        with(|a| {
            if let Some(obj) = a.list.filtered.item(pos) {
                a.play_obj(obj, 0.0, true);
            }
        })
    });
    a.search.connect_search_changed(|e| {
        let q = e.text();
        with(|a| {
            a.list.set_query(&q);
            a.update_status();
        });
    });
    a.search.connect_stop_search(|e| {
        e.set_text("");
        with(|a| {
            a.list.set_query("");
            a.update_status();
            a.reveal_current();
            a.list.view.grab_focus();
        });
    });
    // After re-sorting, jump to the playing track instead of wherever the
    // previous first visible row ended up.
    if let Some(sorter) = a.list.view.sorter() {
        sorter.connect_changed(|_, _| {
            glib::idle_add_local_once(|| with(App::reveal_current));
        });
    }

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, mods| {
        let mut handled = false;
        with(|a| handled = a.on_key(key, mods));
        if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    });
    a.window.add_controller(keys);

    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(|_, value, _, _| {
        let Ok(files) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<PathBuf> = files.files().iter().filter_map(|f| f.path()).collect();
        with(|a| a.add_paths(paths, false));
        true
    });
    a.window.add_controller(drop);

    a.window.connect_close_request(|_| {
        with(App::save_session);
        glib::Propagation::Proceed
    });
}

impl App {
    fn handle(&self, msg: Msg) {
        match msg {
            Msg::Audio(Event::Loaded { duration, cover }) => {
                self.failures.set(0);
                if let Some(d) = duration.filter(|d| *d > 0.0) {
                    self.set_duration(d);
                }
                // Image decoding can take a while for big covers: do it off
                // the main thread (textures are immutable and thread-safe).
                let cur = self.current.borrow().as_ref().map(|o| (track(o).id, track(o).path.clone()));
                if let Some((id, path)) = cur {
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        #[cfg(feature = "mpris")]
                        let art = match &cover {
                            Some(bytes) => mpris::write_art(id, bytes),
                            None => panel::folder_cover(&path),
                        };
                        #[cfg(not(feature = "mpris"))]
                        let art = None;
                        post(&tx, Msg::Cover(id, panel::load_cover(cover, &path), art));
                    });
                }
            }
            Msg::Cover(id, tex, art) => {
                if self.current.borrow().as_ref().is_some_and(|o| track(o).id == id) {
                    self.panel.set_cover(tex);
                    *self.art.borrow_mut() = art;
                    notify(&["Metadata"]);
                }
            }
            Msg::Audio(Event::Finished) => self.next(true),
            Msg::Audio(Event::LoadFailed(e)) => {
                self.flash(&e);
                let n = self.failures.get() + 1;
                self.failures.set(n);
                if self.state.get() == State::Playing && n < 5 && self.list.sorted.n_items() > 1 {
                    self.next(true);
                } else {
                    self.stop();
                }
            }
            Msg::Audio(Event::Error(e)) => self.flash(&e),
            Msg::Scan(ScanEvent::Batch { scan, tracks, first }) => {
                if scan < self.valid_from.get() {
                    return;
                }
                let objs: Vec<glib::Object> = tracks
                    .into_iter()
                    .map(|t| {
                        self.total.set(self.total.get() + t.duration);
                        wrap(t)
                    })
                    .collect();
                self.list.store.extend_from_slice(&objs);
                if first && self.play_scan.get() == scan {
                    self.play_scan.set(0);
                    if let Some(obj) = objs.first() {
                        self.play_obj(obj.clone(), 0.0, true);
                        self.reveal(obj);
                    }
                }
                self.update_status();
            }
            Msg::Scan(ScanEvent::Done { scan }) => {
                if scan >= self.valid_from.get() {
                    self.scanning.set(self.scanning.get().saturating_sub(1));
                    self.update_status();
                }
            }
        }
    }

    fn on_key(&self, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        let typing = gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|w| w.is::<gtk::Text>());
        let plain = !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
        let step = if mods.contains(gdk::ModifierType::SHIFT_MASK) { 30.0 } else { 5.0 };
        match key {
            gdk::Key::AudioPlay | gdk::Key::AudioPause => self.toggle(),
            gdk::Key::AudioStop => self.stop(),
            gdk::Key::AudioNext => self.next(false),
            gdk::Key::AudioPrev => self.prev(),
            _ if !plain || typing => return false,
            gdk::Key::space => self.toggle(),
            gdk::Key::Left | gdk::Key::KP_Left => self.seek_to(self.player.position() - step),
            gdk::Key::Right | gdk::Key::KP_Right => self.seek_to(self.player.position() + step),
            gdk::Key::Delete | gdk::Key::KP_Delete => self.remove_selected(),
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------ playback --

    fn play_obj(&self, obj: glib::Object, start: f64, play: bool) {
        let t = track(&obj);
        self.player.send(Command::Load { path: t.path.clone(), start, play });
        self.order.borrow_mut().started(t.id);
        self.panel.title.set_text(&t.title);
        let info = match (t.artist.is_empty(), t.album.is_empty()) {
            (false, false) => format!("{} — {}", t.artist, t.album),
            (false, true) => t.artist.clone(),
            (true, false) => t.album.clone(),
            (true, true) => t.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
        };
        self.panel.info.set_text(&info);
        self.window.set_title(Some(&format!("{} — haste", t.display_name())));
        let duration = t.duration;
        drop(t);
        self.list.set_playing(Some(&obj));
        *self.current.borrow_mut() = Some(obj);
        *self.art.borrow_mut() = None;
        self.set_duration(duration);
        self.panel.seek.set_value(start);
        self.panel.set_time(start, duration);
        self.set_state(if play { State::Playing } else { State::Paused });
    }

    fn set_duration(&self, d: f64) {
        self.duration.set(d);
        self.panel.seek.set_range(0.0, d.max(1.0));
        self.panel.seek.set_sensitive(d > 0.0);
        self.panel.set_time(self.player.position(), d);
        notify(&["Metadata", "CanSeek"]);
    }

    fn set_repeat(&self, r: Repeat) {
        self.order.borrow_mut().repeat = r;
        self.panel.set_repeat(r);
        notify(&["LoopStatus"]);
    }

    fn set_state(&self, s: State) {
        self.state.set(s);
        self.panel.set_playing(s == State::Playing);
        notify(&["PlaybackStatus"]);
        if s == State::Playing && !self.ticking.replace(true) {
            glib::timeout_add_local(Duration::from_millis(200), || {
                let mut more = false;
                with(|a| {
                    more = a.state.get() == State::Playing;
                    if more {
                        a.update_time();
                    } else {
                        a.ticking.set(false);
                    }
                });
                if more { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
            });
        }
    }

    fn update_time(&self) {
        let pos = self.player.position();
        if self.seek_hold.get().is_none_or(|t| Instant::now() > t) {
            self.panel.seek.set_value(pos);
        }
        self.panel.set_time(pos, self.duration.get());
    }

    fn toggle(&self) {
        match self.state.get() {
            State::Playing => {
                self.player.send(Command::Pause);
                self.set_state(State::Paused);
            }
            State::Paused => {
                self.player.send(Command::Play);
                self.set_state(State::Playing);
            }
            State::Stopped => {
                let cur = self.current.borrow().clone();
                let pick = cur.or_else(|| self.list.selected().into_iter().next());
                match pick {
                    Some(obj) => self.play_obj(obj, 0.0, true),
                    None => self.next(false),
                }
            }
        }
    }

    fn stop(&self) {
        self.player.send(Command::Stop);
        self.set_state(State::Stopped);
        self.panel.seek.set_value(0.0);
        self.panel.set_time(0.0, self.duration.get());
    }

    fn current_pos(&self) -> Option<usize> {
        let cur = self.current.borrow();
        List::position_in(&self.list.sorted, cur.as_ref()?).map(|p| p as usize)
    }

    fn next(&self, auto: bool) {
        let sorted = &self.list.sorted;
        let len = sorted.n_items() as usize;
        let id_at = |i: usize| sorted.item(i as u32).map_or(0, |o| track(&o).id);
        let next = self.order.borrow_mut().next(self.current_pos(), len, id_at, auto);
        match next.and_then(|i| sorted.item(i as u32)) {
            Some(obj) => {
                self.play_obj(obj.clone(), 0.0, true);
                self.reveal(&obj);
            }
            None => {
                self.stop();
                if auto && len > 0 {
                    self.flash(tr("End of playlist"));
                }
            }
        }
    }

    fn prev(&self) {
        if self.state.get() != State::Stopped && self.duration.get() > 0.0 && self.player.position() > 3.0 {
            self.seek_to(0.0);
            return;
        }
        let sorted = &self.list.sorted;
        let len = sorted.n_items();
        let find = |id: u64| (0..len).find(|&i| sorted.item(i).is_some_and(|o| track(&o).id == id)).map(|i| i as usize);
        let prev = self.order.borrow_mut().prev(self.current_pos(), len as usize, find);
        if let Some(obj) = prev.and_then(|i| sorted.item(i as u32)) {
            self.play_obj(obj.clone(), 0.0, true);
            self.reveal(&obj);
        }
    }

    fn seek_to(&self, secs: f64) {
        if self.state.get() == State::Stopped || self.duration.get() <= 0.0 {
            return;
        }
        let secs = secs.clamp(0.0, self.duration.get());
        self.player.send(Command::Seek(secs));
        self.seek_hold.set(Some(Instant::now() + Duration::from_millis(400)));
        self.panel.seek.set_value(secs);
        self.panel.set_time(secs, self.duration.get());
        notify_seeked(secs);
    }

    /// Scrolls the list so that `obj` is visible.
    fn reveal(&self, obj: &glib::Object) {
        if let Some(pos) = List::position_in(&self.list.filtered, obj) {
            self.list.view.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// Shows the playing track, or the top of the list.
    fn reveal_current(&self) {
        let cur = self.current.borrow().clone();
        let pos = cur.and_then(|o| List::position_in(&self.list.filtered, &o)).unwrap_or(0);
        if pos < self.list.filtered.n_items() {
            self.list.view.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
        }
    }

    // ------------------------------------------------------ playlist --

    fn add_paths(&self, paths: Vec<PathBuf>, play: bool) {
        if paths.is_empty() {
            return;
        }
        let id = self.scan_seq.get() + 1;
        self.scan_seq.set(id);
        if play {
            self.play_scan.set(id);
        }
        self.scanning.set(self.scanning.get() + 1);
        let tx = self.tx.clone();
        library::scan(id, paths, self.cancel.borrow().clone(), move |ev| post(&tx, Msg::Scan(ev)));
        self.update_status();
    }

    fn remove_selected(&self) {
        let sel = self.list.selected();
        if sel.is_empty() {
            return;
        }
        let gone: HashSet<*mut _> = sel.iter().map(|o| o.as_ptr()).collect();
        let removed: f64 = sel.iter().map(|o| track(o).duration).sum();
        self.list.store.retain(|o| !gone.contains(&o.as_ptr()));
        self.total.set((self.total.get() - removed).max(0.0));
        self.update_status();
    }

    fn clear(&self) {
        self.cancel.borrow().store(true, Relaxed);
        *self.cancel.borrow_mut() = Arc::new(AtomicBool::new(false));
        self.valid_from.set(self.scan_seq.get() + 1);
        self.scanning.set(0);
        self.play_scan.set(0);
        self.list.store.remove_all();
        self.total.set(0.0);
        self.update_status();
    }

    fn update_status(&self) {
        if self.flash_until.get().is_some_and(|t| Instant::now() < t) {
            return;
        }
        let n = self.list.store.n_items();
        let mut s = format!("{}: {n} · {}", tr("Tracks"), fmt_time(self.total.get()));
        if self.list.is_filtered() {
            s += &format!(" · {}: {}", tr("shown"), self.list.filtered.n_items());
        }
        if self.scanning.get() > 0 {
            s += " · ";
            s += tr("scanning…");
        }
        self.status.set_text(&s);
    }

    /// Shows a message in the status bar for a few seconds.
    fn flash(&self, msg: &str) {
        self.status.set_text(msg);
        let until = Instant::now() + Duration::from_secs(5);
        self.flash_until.set(Some(until));
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            with(|a| {
                if a.flash_until.get() == Some(until) {
                    a.flash_until.set(None);
                    a.update_status();
                }
            })
        });
    }

    // ------------------------------------------------------- dialogs --

    fn dialog(&self, title: &str, suffixes: &[&[&str]], filter_name: &str) -> gtk::FileDialog {
        let d = gtk::FileDialog::new();
        d.set_title(title);
        if !suffixes.is_empty() {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(filter_name));
            for s in suffixes.iter().flat_map(|s| s.iter()) {
                filter.add_suffix(s);
            }
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            d.set_filters(Some(&filters));
            d.set_default_filter(Some(&filter));
        }
        if let Some(dir) = self.last_dir.borrow().as_ref() {
            d.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        d
    }

    fn remember_dir(&self, paths: &[PathBuf]) {
        if let Some(p) = paths.first() {
            *self.last_dir.borrow_mut() = Some(p.parent().unwrap_or(p).to_path_buf());
        }
    }

    fn files_of(res: Result<gio::ListModel, glib::Error>) -> Vec<PathBuf> {
        let Ok(model) = res else { return Vec::new() };
        (0..model.n_items()).filter_map(|i| model.item(i).and_downcast::<gio::File>()?.path()).collect()
    }

    fn add_files(&self) {
        let d = self.dialog(tr("Add files…"), &[AUDIO_EXTS, PLAYLIST_EXTS], tr("Audio files and playlists"));
        d.open_multiple(Some(&self.window), gio::Cancellable::NONE, |res| {
            let paths = App::files_of(res);
            with(|a| {
                a.remember_dir(&paths);
                a.add_paths(paths, false);
            });
        });
    }

    fn add_folder(&self) {
        let d = self.dialog(tr("Add folder…"), &[], "");
        d.select_multiple_folders(Some(&self.window), gio::Cancellable::NONE, |res| {
            let paths = App::files_of(res);
            with(|a| {
                a.remember_dir(&paths);
                a.add_paths(paths, false);
            });
        });
    }

    fn import_playlist(&self) {
        let d = self.dialog(tr("Import playlist…"), &[PLAYLIST_EXTS], tr("Playlists"));
        d.open(Some(&self.window), gio::Cancellable::NONE, |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                with(|a| {
                    a.remember_dir(std::slice::from_ref(&path));
                    a.add_paths(vec![path], false);
                });
            }
        });
    }

    fn export_playlist(&self) {
        let d = self.dialog(tr("Export playlist…"), &[PLAYLIST_EXTS], tr("Playlists"));
        d.set_initial_name(Some("playlist.m3u8"));
        d.save(Some(&self.window), gio::Cancellable::NONE, |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            with(|a| {
                let objs: Vec<glib::Object> = a.list.sorted.iter::<glib::Object>().flatten().collect();
                let tracks: Vec<_> = objs.iter().map(track).collect();
                let text = m3u::write(tracks.iter().map(|t| &**t), path.parent());
                if let Err(e) = std::fs::write(&path, text) {
                    a.flash(&format!("{}: {e}", tr("Cannot save playlist")));
                }
                a.remember_dir(std::slice::from_ref(&path));
            });
        });
    }

    // ------------------------------------------------------- session --

    fn restore(&self, (s, tracks): (Settings, Vec<Track>)) {
        self.window.set_default_size(s.width, s.height);
        if s.maximized {
            self.window.maximize();
        }
        self.panel.volume.set_value(s.volume * 100.0);
        self.player.set_volume(s.volume);
        self.panel.set_volume_icon(s.volume * 100.0);
        self.panel.shuffle.set_active(s.shuffle);
        {
            let mut o = self.order.borrow_mut();
            o.set_shuffle(s.shuffle);
            o.repeat = s.repeat;
        }
        self.panel.set_repeat(s.repeat);
        self.eq.set_values(s.eq_enabled, s.eq_preamp, &s.eq_gains);
        self.player.set_eq(s.eq_enabled, s.eq_preamp, &s.eq_gains);
        *self.last_dir.borrow_mut() = s.last_dir;
        self.total.set(tracks.iter().map(|t| t.duration).sum());
        let objs: Vec<glib::Object> = tracks.into_iter().map(wrap).collect();
        self.list.store.extend_from_slice(&objs);
        self.list.set_sort(s.sort);
        if let Some(obj) = s.current.and_then(|i| objs.get(i)) {
            self.play_obj(obj.clone(), s.position, false);
        }
        // Scrolling before the first frame confuses the list view; wait
        // until the window is laid out.
        let once = Cell::new(false);
        self.window.connect_map(move |_| {
            if !once.replace(true) {
                glib::timeout_add_local_once(Duration::from_millis(50), || with(App::reveal_current));
            }
        });
        self.update_status();
    }

    fn save_session(&self) {
        let (width, height) = self.window.default_size();
        let current = self.current.borrow().as_ref().and_then(|o| List::position_in(&self.list.store, o));
        let position = if self.state.get() == State::Stopped { 0.0 } else { self.player.position() };
        let (eq_enabled, eq_preamp, eq_gains) = self.eq.values();
        let settings = Settings {
            volume: self.panel.volume.value() / 100.0,
            shuffle: self.order.borrow().shuffle,
            repeat: self.order.borrow().repeat,
            width,
            height,
            maximized: self.window.is_maximized(),
            current: current.map(|c| c as usize),
            position,
            sort: self.list.sort_state(),
            last_dir: self.last_dir.borrow().clone(),
            eq_enabled,
            eq_preamp,
            eq_gains,
        };
        let objs: Vec<glib::Object> = self.list.store.iter::<glib::Object>().flatten().collect();
        let tracks: Vec<_> = objs.iter().map(track).collect();
        let playlist = config::serialize_playlist(tracks.iter().map(|t| &**t));
        if let Err(e) = config::save(&settings, &playlist) {
            eprintln!("haste: cannot save session: {e}");
        }
    }
}
