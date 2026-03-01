#![windows_subsystem = "windows"]

mod app;
mod colors;
mod draw;
mod dropdown;
mod font;
mod gpu;
mod icons;
mod layout;
mod menu;
mod saved_sessions;
mod ssh;
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
        let cls = Class::get("NSProcessInfo").unwrap();
        let info: *mut Object = msg_send![cls, processInfo];
        // NSActivityUserInitiatedAllowingIdleSystemSleep = 0x00FFFFFF
        let reason = ns_string("Terminal I/O");
        let _: *mut Object =
            msg_send![info, beginActivityWithOptions:0x00FFFFFFu64 reason:reason];
    }

    unsafe fn ns_string(s: &str) -> *mut Object {
        use objc::runtime::Class;
        use std::ffi::CString;
        let cls = Class::get("NSString").unwrap();
        let cstr = CString::new(s).unwrap();
        msg_send![cls, stringWithUTF8String: cstr.as_ptr()]
    }
}

fn main() {
    env_logger::init();

    #[cfg(target_os = "macos")]
    disable_app_nap();

    let event_loop = EventLoop::<TerminalEvent>::with_user_event()
        .build()
        .expect("create event loop");

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);

    event_loop.run_app(&mut app).expect("run event loop");
}
