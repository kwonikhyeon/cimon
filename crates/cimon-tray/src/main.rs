//! `cimon-tray` — the notification-area app on the build machine (docs/tray.md): the
//! service's state, starting and stopping it, and admin commands through UAC. It runs
//! with the logged-on user's rights only and opens no channel that takes commands.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod model;
#[cfg(windows)]
mod tray;

fn main() {
    #[cfg(windows)]
    tray::run();
    #[cfg(not(windows))]
    eprintln!("cimon-tray runs on Windows");
}
