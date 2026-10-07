mod audio;
mod config;
mod library;
mod playlist;
mod ui;
mod util;

fn main() -> gtk::glib::ExitCode {
    ui::run()
}
