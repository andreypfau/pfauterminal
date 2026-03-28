#![windows_subsystem = "windows"]

mod app;
mod colors;
mod draw;
mod dropdown;
mod font;
mod gpu;
mod hotkeys;
mod icons;
mod layout;
mod menu;
mod saved_sessions;
mod ssh;
mod ssh_config;
mod ssh_dialog;
mod tab_bar;
mod terminal_panel;
mod theme;
mod widgets;

use winit::event_loop::EventLoop;

use app::App;
use terminal_panel::TerminalEvent;

/// Prevent macOS App Nap from throttling background threads (e.g. the
/// alacritty I/O thread that writes keystrokes to the PTY).  Without this,
/// macOS may delay kqueue wake-ups by many seconds, causing intermittent
/// hangs where typed commands don't execute for 10-20 s.
#[cfg(target_os = "macos")]
fn disable_app_nap() {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let Some(cls) = Class::get("NSProcessInfo") else { return };
        let info: *mut Object = msg_send![cls, processInfo];
        let Some(reason) = ns_string("Terminal I/O") else { return };
        let _: *mut Object =
            // NSActivityUserInitiated (0x00FFFFFF) minus NSActivityIdleSystemSleepDisabled (1<<20):
            // Prevents App Nap throttling without preventing system sleep.
            msg_send![info, beginActivityWithOptions:0x00EFFFFEu64 reason:reason];
    }

    unsafe fn ns_string(s: &str) -> Option<*mut Object> {
        use objc::runtime::Class;
        use std::ffi::CString;
        let cls = Class::get("NSString")?;
        let cstr = CString::new(s).ok()?;
        Some(msg_send![cls, stringWithUTF8String: cstr.as_ptr()])
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    disable_app_nap();

    let event_loop = match EventLoop::<TerminalEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(_) => {
            std::process::exit(1);
        }
    };

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);

    if event_loop.run_app(&mut app).is_err() {
        std::process::exit(1);
    }
}
