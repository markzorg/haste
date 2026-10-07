mod audio;

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver};

use audio::{Command, Event, Player};
use gtk::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.markzorg.Haste";

struct Ui {
    player: Player,
    events: Receiver<Event>,
    label: gtk::Label,
    playing: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn dispatch() {
    UI.with_borrow_mut(|ui| {
        let Some(ui) = ui else { return };
        while let Ok(ev) = ui.events.try_recv() {
            match ev {
                Event::Loaded { duration, .. } => ui.label.set_text(&format!("Loaded, {:.1}s", duration.unwrap_or(0.0))),
                Event::Finished => ui.label.set_text("Finished"),
                Event::Error(e) => ui.label.set_text(&e),
            }
        }
    });
}

fn build(app: &gtk::Application) -> gtk::ApplicationWindow {
    let (tx, rx) = mpsc::channel();
    let player = Player::new(move |ev| {
        let _ = tx.send(ev);
        glib::MainContext::default().invoke_with_priority(glib::Priority::DEFAULT, dispatch);
    });
    let label = gtk::Label::new(Some("haste"));
    let button = gtk::Button::from_icon_name("media-playback-start-symbolic");
    button.connect_clicked(|_| {
        UI.with_borrow_mut(|ui| {
            if let Some(ui) = ui {
                ui.playing = !ui.playing;
                ui.player.send(if ui.playing { Command::Play } else { Command::Pause });
            }
        })
    });
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.append(&label);
    vbox.append(&button);
    UI.set(Some(Ui { player, events: rx, label, playing: false }));
    gtk::ApplicationWindow::builder().application(app).title("haste").default_width(320).child(&vbox).build()
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_activate(|app| build(app).present());
    app.connect_open(|app, files, _| {
        let win = build(app);
        win.present();
        if let Some(path) = files.first().and_then(|f| f.path()) {
            UI.with_borrow_mut(|ui| {
                if let Some(ui) = ui {
                    ui.playing = true;
                    ui.player.send(Command::Load { path, start: 0.0, play: true });
                }
            });
        }
    });
    let code = app.run();
    UI.set(None);
    code
}
