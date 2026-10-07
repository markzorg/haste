//! Equalizer window: preamp + 10 band sliders, presets, on/off switch.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use super::tr;
use crate::audio::eq::{BANDS, MAX_DB};

const PRESETS: &[(&str, [f32; 10])] = &[
    ("Flat", [0.0; 10]),
    ("Rock", [5.0, 4.0, 3.0, 1.0, -1.0, -1.0, 1.0, 3.0, 4.0, 5.0]),
    ("Pop", [-1.0, 1.0, 3.0, 4.0, 3.0, 0.0, -1.0, -1.0, 1.0, 2.0]),
    ("Jazz", [3.0, 2.0, 1.0, 2.0, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0]),
    ("Classical", [4.0, 3.0, 2.0, 1.0, -1.0, -1.0, 0.0, 2.0, 3.0, 4.0]),
    ("Bass boost", [7.0, 6.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Treble boost", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 7.0]),
    ("Vocal", [-2.0, -2.0, -1.0, 1.0, 3.0, 4.0, 3.0, 1.0, 0.0, -1.0]),
];

pub struct EqView {
    pub window: gtk::Window,
    enabled: gtk::Switch,
    preamp: gtk::Scale,
    bands: Vec<gtk::Scale>,
    presets: gtk::DropDown,
    /// Set while sliders are moved programmatically.
    applying: Rc<Cell<bool>>,
}

fn slider(label: &str) -> (gtk::Box, gtk::Scale) {
    let s = gtk::Scale::with_range(gtk::Orientation::Vertical, -f64::from(MAX_DB), f64::from(MAX_DB), 1.0);
    s.set_inverted(true);
    s.set_draw_value(true);
    s.set_value_pos(gtk::PositionType::Top);
    s.set_digits(0);
    s.set_size_request(-1, 170);
    s.set_vexpand(true);
    s.add_mark(0.0, gtk::PositionType::Right, None);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("caption");
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.append(&s);
    b.append(&l);
    (b, s)
}

fn band_label(f: f32) -> String {
    if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") }
}

impl EqView {
    pub fn new(parent: &gtk::ApplicationWindow) -> EqView {
        let enabled = gtk::Switch::new();
        enabled.set_valign(gtk::Align::Center);
        let mut names = vec![tr("Custom")];
        names.extend(PRESETS.iter().map(|(n, _)| tr(n)));
        let presets = gtk::DropDown::from_strings(&names);
        let reset = gtk::Button::with_label(tr("Reset"));
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.append(&gtk::Label::new(Some(tr("Enabled"))));
        top.append(&enabled);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        top.append(&spacer);
        top.append(&presets);
        top.append(&reset);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let (pb, preamp) = slider(tr("Preamp"));
        row.append(&pb);
        row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let mut bands = Vec::new();
        for f in BANDS {
            let (b, s) = slider(&band_label(f));
            row.append(&b);
            bands.push(s);
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        for side in [gtk::Widget::set_margin_top, gtk::Widget::set_margin_bottom, gtk::Widget::set_margin_start, gtk::Widget::set_margin_end] {
            side(root.upcast_ref(), 12);
        }
        root.append(&top);
        root.append(&row);

        let window = gtk::Window::builder()
            .title(tr("Equalizer"))
            .transient_for(parent)
            .hide_on_close(true)
            .resizable(false)
            .child(&root)
            .build();
        let view = EqView { window, enabled, preamp, bands, presets, applying: Rc::default() };
        let (bands, applying) = (view.bands.clone(), view.applying.clone());
        view.presets.connect_selected_notify(move |d| {
            let Some((_, gains)) = (d.selected() as usize).checked_sub(1).and_then(|i| PRESETS.get(i)) else { return };
            applying.set(true);
            for (s, g) in bands.iter().zip(gains) {
                s.set_value(f64::from(*g));
            }
            applying.set(false);
        });
        let (bands, preamp, presets) = (view.bands.clone(), view.preamp.clone(), view.presets.clone());
        let applying = view.applying.clone();
        reset.connect_clicked(move |_| {
            applying.set(true);
            preamp.set_value(0.0);
            for s in &bands {
                s.set_value(0.0);
            }
            presets.set_selected(1);
            applying.set(false);
        });
        view
    }

    /// Calls `f` whenever any setting changes.
    pub fn connect_changed(&self, f: impl Fn() + Clone + 'static) {
        let g = f.clone();
        self.enabled.connect_active_notify(move |_| g());
        let g = f.clone();
        self.preamp.connect_value_changed(move |_| g());
        for s in &self.bands {
            let (g, applying, presets) = (f.clone(), self.applying.clone(), self.presets.clone());
            s.connect_value_changed(move |_| {
                if !applying.get() {
                    presets.set_selected(0);
                }
                g();
            });
        }
    }

    /// (enabled, preamp dB, band gains dB)
    pub fn values(&self) -> (bool, f32, [f32; 10]) {
        let mut gains = [0.0; 10];
        for (g, s) in gains.iter_mut().zip(&self.bands) {
            *g = s.value() as f32;
        }
        (self.enabled.is_active(), self.preamp.value() as f32, gains)
    }

    pub fn set_values(&self, enabled: bool, preamp: f32, gains: &[f32; 10]) {
        self.applying.set(true);
        self.enabled.set_active(enabled);
        self.preamp.set_value(f64::from(preamp));
        for (s, g) in self.bands.iter().zip(gains) {
            s.set_value(f64::from(*g));
        }
        let preset = PRESETS.iter().position(|(_, p)| p == gains).map_or(0, |i| i + 1);
        self.presets.set_selected(preset as u32);
        self.applying.set(false);
    }
}
