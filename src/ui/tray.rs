//! Tray icon: StatusNotifierItem + com.canonical.dbusmenu over GDBus.
//! Works with XFCE's status tray, KDE, and GNOME with the AppIndicator
//! extension. Left click shows/hides the window, middle click toggles
//! playback, scrolling changes the volume.

use std::cell::{Cell, RefCell};

use glib::{Variant, VariantTy};
use gtk::prelude::*;
use gtk::{gio, glib};

use super::{App, State, list::track, tr, with};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const XML: &str = r#"<node>
<interface name="org.kde.StatusNotifierItem">
 <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
 <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
 <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
 <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
 <signal name="NewTitle"/><signal name="NewIcon"/><signal name="NewToolTip"/>
 <signal name="NewStatus"><arg name="status" type="s"/></signal>
 <property name="Category" type="s" access="read"/>
 <property name="Id" type="s" access="read"/>
 <property name="Title" type="s" access="read"/>
 <property name="Status" type="s" access="read"/>
 <property name="WindowId" type="i" access="read"/>
 <property name="IconName" type="s" access="read"/>
 <property name="IconPixmap" type="a(iiay)" access="read"/>
 <property name="OverlayIconName" type="s" access="read"/>
 <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
 <property name="AttentionIconName" type="s" access="read"/>
 <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
 <property name="AttentionMovieName" type="s" access="read"/>
 <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
 <property name="ItemIsMenu" type="b" access="read"/>
 <property name="Menu" type="o" access="read"/>
</interface>
<interface name="com.canonical.dbusmenu">
 <method name="GetLayout">
  <arg type="i" name="parentId" direction="in"/><arg type="i" name="recursionDepth" direction="in"/>
  <arg type="as" name="propertyNames" direction="in"/>
  <arg type="u" name="revision" direction="out"/><arg type="(ia{sv}av)" name="layout" direction="out"/>
 </method>
 <method name="GetGroupProperties">
  <arg type="ai" name="ids" direction="in"/><arg type="as" name="propertyNames" direction="in"/>
  <arg type="a(ia{sv})" name="properties" direction="out"/>
 </method>
 <method name="GetProperty">
  <arg type="i" name="id" direction="in"/><arg type="s" name="name" direction="in"/>
  <arg type="v" name="value" direction="out"/>
 </method>
 <method name="Event">
  <arg type="i" name="id" direction="in"/><arg type="s" name="eventId" direction="in"/>
  <arg type="v" name="data" direction="in"/><arg type="u" name="timestamp" direction="in"/>
 </method>
 <method name="EventGroup">
  <arg type="a(isvu)" name="events" direction="in"/><arg type="ai" name="idErrors" direction="out"/>
 </method>
 <method name="AboutToShow">
  <arg type="i" name="id" direction="in"/><arg type="b" name="needUpdate" direction="out"/>
 </method>
 <method name="AboutToShowGroup">
  <arg type="ai" name="ids" direction="in"/><arg type="ai" name="updatesNeeded" direction="out"/>
  <arg type="ai" name="idErrors" direction="out"/>
 </method>
 <signal name="ItemsPropertiesUpdated">
  <arg type="a(ia{sv})" name="updatedProps"/><arg type="a(ias)" name="removedProps"/>
 </signal>
 <signal name="LayoutUpdated"><arg type="u" name="revision"/><arg type="i" name="parent"/></signal>
 <property name="Version" type="u" access="read"/>
 <property name="TextDirection" type="s" access="read"/>
 <property name="Status" type="s" access="read"/>
 <property name="IconThemePath" type="as" access="read"/>
</interface>
</node>"#;

/// Menu entries: (id, label); an empty label is a separator.
const ENTRIES: &[(i32, &str)] = &[
    (1, "Play/Pause"),
    (2, "Next"),
    (3, "Previous"),
    (4, "Stop"),
    (5, ""),
    (6, "Show/Hide window"),
    (7, ""),
    (8, "Quit"),
];

thread_local! {
    static CONN: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
    static REVISION: Cell<u32> = const { Cell::new(1) };
}

fn bus_name() -> String {
    format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
}

pub fn start() {
    let _ = gio::bus_own_name(
        gio::BusType::Session,
        &bus_name(),
        gio::BusNameOwnerFlags::DO_NOT_QUEUE,
        |conn, _| register(&conn),
        |conn, _| {
            // (Re-)register whenever a tray host's watcher shows up.
            let _ = gio::bus_watch_name_on_connection(
                &conn,
                WATCHER,
                gio::BusNameWatcherFlags::NONE,
                |conn, _, _| announce(&conn),
                |_, _| {},
            );
        },
        |_, _| {},
    );
}

fn announce(conn: &gio::DBusConnection) {
    conn.call(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        WATCHER,
        "RegisterStatusNotifierItem",
        Some(&(bus_name(),).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |_| {},
    );
}

fn register(conn: &gio::DBusConnection) {
    let Ok(node) = gio::DBusNodeInfo::for_xml(XML) else { return };
    for (path, name) in [(ITEM_PATH, ITEM), (MENU_PATH, MENU)] {
        let Some(iface) = node.lookup_interface(name) else { continue };
        let _ = conn
            .register_object(path, &iface)
            .method_call(|_, _, _, iface, method, args, inv| {
                let mut ret = None;
                with(|a| ret = call(a, iface.unwrap_or(ITEM), method, &args));
                inv.return_value(ret.as_ref());
            })
            .property(|_, _, _, iface, prop| {
                let mut v = None;
                with(|a| v = Some(if iface == ITEM { item_prop(a, prop) } else { menu_prop(prop) }));
                v.unwrap_or_else(|| "".to_variant())
            })
            .build();
    }
    CONN.set(Some(conn.clone()));
}

fn icon_name(a: &App) -> &'static str {
    let theme = gtk::IconTheme::for_display(&WidgetExt::display(&a.window));
    if theme.has_icon(super::APP_ID) { super::APP_ID } else { "audio-x-generic" }
}

fn empty_pixmaps() -> Variant {
    Variant::array_from_iter_with_type(VariantTy::new("(iiay)").expect("type"), [] as [Variant; 0])
}

fn item_prop(a: &App, prop: &str) -> Variant {
    match prop {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" | "Title" => "haste".to_variant(),
        "Status" => "Active".to_variant(),
        "WindowId" => 0i32.to_variant(),
        "IconName" => icon_name(a).to_variant(),
        "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => empty_pixmaps(),
        "ToolTip" => {
            let text = a.current.borrow().as_ref().map(|o| track(o).display_name()).unwrap_or_default();
            Variant::tuple_from_iter([icon_name(a).to_variant(), empty_pixmaps(), "haste".to_variant(), text.to_variant()])
        }
        "ItemIsMenu" => false.to_variant(),
        "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH).expect("path").to_variant(),
        _ => "".to_variant(),
    }
}

fn menu_prop(prop: &str) -> Variant {
    match prop {
        "Version" => 3u32.to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "Status" => "normal".to_variant(),
        _ => Vec::<String>::new().to_variant(),
    }
}

fn entry_props(a: &App, id: i32) -> Variant {
    let d = glib::VariantDict::new(None);
    match ENTRIES.iter().find(|(i, _)| *i == id) {
        Some((_, "")) => d.insert_value("type", &"separator".to_variant()),
        Some((1, _)) => {
            let label = if a.state.get() == State::Playing { tr("Pause") } else { tr("Play") };
            d.insert_value("label", &label.to_variant());
        }
        Some((_, label)) => d.insert_value("label", &tr(label).to_variant()),
        None => d.insert_value("children-display", &"submenu".to_variant()),
    }
    d.end()
}

fn layout(a: &App) -> Variant {
    let no_children = || Variant::array_from_iter_with_type(VariantTy::VARIANT, [] as [Variant; 0]);
    let children = ENTRIES.iter().map(|(id, _)| {
        let item = Variant::tuple_from_iter([id.to_variant(), entry_props(a, *id), no_children()]);
        Variant::from_variant(&item)
    });
    let children = Variant::array_from_iter_with_type(VariantTy::VARIANT, children);
    Variant::tuple_from_iter([0i32.to_variant(), entry_props(a, 0), children])
}

fn activate(a: &App, id: i32) {
    match id {
        1 => a.toggle(),
        2 => a.next(false),
        3 => a.prev(),
        4 => a.stop(),
        6 => toggle_window(a),
        8 => a.window.close(),
        _ => {}
    }
}

fn toggle_window(a: &App) {
    if a.window.is_visible() && a.window.is_active() {
        a.window.set_visible(false);
    } else {
        a.window.present();
    }
}

fn call(a: &App, iface: &str, method: &str, args: &Variant) -> Option<Variant> {
    let ids = |v: Option<Variant>| v.and_then(|v| v.get::<Vec<i32>>()).unwrap_or_default();
    match (iface, method) {
        (ITEM, "Activate") => toggle_window(a),
        (ITEM, "SecondaryActivate") => a.toggle(),
        (ITEM, "Scroll") => {
            if let Some((delta, orientation)) = args.get::<(i32, String)>() {
                if orientation.eq_ignore_ascii_case("vertical") {
                    let v = a.panel.volume.value() + f64::from(delta.signum()) * 5.0;
                    a.panel.volume.set_value(v.clamp(0.0, 100.0));
                }
            }
        }
        (MENU, "GetLayout") => return Some(Variant::tuple_from_iter([REVISION.get().to_variant(), layout(a)])),
        (MENU, "GetGroupProperties") => {
            let want = ids(args.try_child_value(0));
            let all: Vec<i32> = std::iter::once(0).chain(ENTRIES.iter().map(|(i, _)| *i)).collect();
            let items = all
                .into_iter()
                .filter(|i| want.is_empty() || want.contains(i))
                .map(|i| Variant::tuple_from_iter([i.to_variant(), entry_props(a, i)]));
            let ty = VariantTy::new("(ia{sv})").expect("type");
            return Some(Variant::tuple_from_iter([Variant::array_from_iter_with_type(ty, items)]));
        }
        (MENU, "GetProperty") => {
            let (id, name) = args.get::<(i32, String)>().unwrap_or_default();
            let props = glib::VariantDict::new(Some(&entry_props(a, id)));
            let v = props.lookup_value(&name, None).unwrap_or_else(|| "".to_variant());
            return Some(Variant::tuple_from_iter([Variant::from_variant(&v)]));
        }
        (MENU, "Event") => {
            if let Some((id, event)) = args.try_child_value(0).zip(args.try_child_value(1)) {
                if event.str() == Some("clicked") {
                    activate(a, id.get::<i32>().unwrap_or(0));
                }
            }
            return None;
        }
        (MENU, "EventGroup") => {
            if let Some(events) = args.try_child_value(0) {
                for e in events.iter() {
                    if e.try_child_value(1).and_then(|v| v.str().map(str::to_owned)).as_deref() == Some("clicked") {
                        activate(a, e.try_child_value(0).and_then(|v| v.get::<i32>()).unwrap_or(0));
                    }
                }
            }
            return Some((Vec::<i32>::new(),).to_variant());
        }
        (MENU, "AboutToShow") => return Some((false,).to_variant()),
        (MENU, "AboutToShowGroup") => return Some((Vec::<i32>::new(), Vec::<i32>::new()).to_variant()),
        _ => {}
    }
    None
}

/// Refreshes the menu label and tooltip after playback changes.
pub fn changed(props: &[&str]) {
    let Some(conn) = CONN.with_borrow(|c| c.clone()) else { return };
    if props.contains(&"PlaybackStatus") {
        let rev = REVISION.get() + 1;
        REVISION.set(rev);
        let _ = conn.emit_signal(None, MENU_PATH, MENU, "LayoutUpdated", Some(&(rev, 0i32).to_variant()));
    }
    if props.contains(&"Metadata") {
        let _ = conn.emit_signal(None, ITEM_PATH, ITEM, "NewToolTip", None);
    }
}
