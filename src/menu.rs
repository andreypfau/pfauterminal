/// App version, read from Cargo.toml at compile time.
pub const APP_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_SHORT_HASH"), ")");
pub const APP_NAME: &str = "pfauterminal";
pub const APP_AUTHOR: &str = "Andrey Pfau";
pub const APP_YEAR: &str = env!("GIT_COMMIT_YEAR");

/// Set up the native application menu bar (macOS) or prepare About info (Windows).
/// Must be called once, before the event loop starts processing events.
pub fn setup_native_menu() {
    #[cfg(target_os = "macos")]
    macos::setup_menu_bar();
}

/// Show the native About dialog.
#[allow(dead_code)]
pub fn show_about() {
    #[cfg(target_os = "macos")]
    macos::show_about_panel();

    #[cfg(target_os = "windows")]
    windows::show_about_dialog();
}

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod macos {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CString;

    use super::{APP_AUTHOR, APP_NAME, APP_VERSION, APP_YEAR};

    /// NSEventModifierFlagCommand | NSEventModifierFlagOption
    const CMD_OPT_MASK: usize = (1 << 20) | (1 << 19);

    unsafe fn nsstring(s: &str) -> *mut Object {
        let Some(cls) = Class::get("NSString") else {
            return std::ptr::null_mut();
        };
        let Ok(cstr) = CString::new(s) else {
            return std::ptr::null_mut();
        };
        msg_send![cls, stringWithUTF8String: cstr.as_ptr()]
    }

    unsafe fn menu_item(title: &str, action: Sel, key: &str) -> *mut Object {
        unsafe {
            let Some(cls) = Class::get("NSMenuItem") else {
                return std::ptr::null_mut();
            };
            let ns_title = nsstring(title);
            let ns_key = nsstring(key);
            let item: *mut Object = msg_send![cls, alloc];
            msg_send![item, initWithTitle: ns_title action: action keyEquivalent: ns_key]
        }
    }

    unsafe fn separator() -> *mut Object {
        let Some(cls) = Class::get("NSMenuItem") else {
            return std::ptr::null_mut();
        };
        msg_send![cls, separatorItem]
    }

    unsafe fn about_options_dict() -> *mut Object {
        unsafe {
            let keys = [
                nsstring("ApplicationName"),
                nsstring("Version"),
                nsstring("Copyright"),
            ];
            let values = [
                nsstring(APP_NAME),
                nsstring(APP_VERSION),
                nsstring(&format!("Copyright © {APP_YEAR} {APP_AUTHOR}")),
            ];

            let Some(dict_cls) = Class::get("NSDictionary") else {
                return std::ptr::null_mut();
            };
            msg_send![
                dict_cls,
                dictionaryWithObjects: values.as_ptr()
                forKeys: keys.as_ptr()
                count: 3usize
            ]
        }
    }

    /// Objective-C method called when "About pfauterminal" is clicked.
    extern "C" fn handle_about(_this: &Object, _cmd: Sel, _sender: *mut Object) {
        unsafe {
            let Some(app_cls) = Class::get("NSApplication") else { return };
            let app: *mut Object = msg_send![app_cls, sharedApplication];
            let dict = about_options_dict();
            let _: () = msg_send![app, orderFrontStandardAboutPanelWithOptions: dict];
        }
    }

    /// Register a one-off ObjC class whose `showAbout:` method opens the About
    /// panel with our custom ApplicationName / Version / Copyright.
    unsafe fn register_about_handler() -> *mut Object {
        unsafe {
            let Some(superclass) = Class::get("NSObject") else {
                return std::ptr::null_mut();
            };
            let Some(mut decl) = ClassDecl::new("PFAUAboutHandler", superclass) else {
                return std::ptr::null_mut();
            };
            decl.add_method(
                sel!(showAbout:),
                handle_about as extern "C" fn(&Object, Sel, *mut Object),
            );
            let cls = decl.register();
            msg_send![cls, new]
        }
    }

    pub fn setup_menu_bar() {
        unsafe {
            let Some(app_cls) = Class::get("NSApplication") else { return };
            let app: *mut Object = msg_send![app_cls, sharedApplication];
            let Some(menu_cls) = Class::get("NSMenu") else { return };
            let Some(mi_cls) = Class::get("NSMenuItem") else { return };

            // Handler for the About action (leaked intentionally — lives for app lifetime)
            let about_handler = register_about_handler();

            let menu_bar: *mut Object = msg_send![menu_cls, new];

            // ── App menu ─────────────────────────────────────────────────
            let app_menu: *mut Object = msg_send![menu_cls, new];

            let about = menu_item(&format!("About {APP_NAME}"), sel!(showAbout:), "");
            let _: () = msg_send![about, setTarget: about_handler];
            let _: () = msg_send![app_menu, addItem: about];
            let _: () = msg_send![app_menu, addItem: separator()];

            let hide = menu_item(&format!("Hide {APP_NAME}"), sel!(hide:), "h");
            let _: () = msg_send![app_menu, addItem: hide];

            let hide_others = menu_item("Hide Others", sel!(hideOtherApplications:), "h");
            let _: () = msg_send![hide_others, setKeyEquivalentModifierMask: CMD_OPT_MASK];
            let _: () = msg_send![app_menu, addItem: hide_others];

            let show_all = menu_item("Show All", sel!(unhideAllApplications:), "");
            let _: () = msg_send![app_menu, addItem: show_all];

            let _: () = msg_send![app_menu, addItem: separator()];

            let quit = menu_item(&format!("Quit {APP_NAME}"), sel!(terminate:), "q");
            let _: () = msg_send![app_menu, addItem: quit];

            let app_menu_item: *mut Object = msg_send![mi_cls, new];
            let _: () = msg_send![app_menu_item, setSubmenu: app_menu];
            let _: () = msg_send![menu_bar, addItem: app_menu_item];

            // ── Edit menu ────────────────────────────────────────────────
            let edit_title = nsstring("Edit");
            let edit_menu: *mut Object = msg_send![menu_cls, alloc];
            let edit_menu: *mut Object = msg_send![edit_menu, initWithTitle: edit_title];

            let copy = menu_item("Copy", sel!(copy:), "c");
            let _: () = msg_send![edit_menu, addItem: copy];

            let paste = menu_item("Paste", sel!(paste:), "v");
            let _: () = msg_send![edit_menu, addItem: paste];

            let select_all = menu_item("Select All", sel!(selectAll:), "a");
            let _: () = msg_send![edit_menu, addItem: select_all];

            let edit_menu_item: *mut Object = msg_send![mi_cls, new];
            let _: () = msg_send![edit_menu_item, setSubmenu: edit_menu];
            let _: () = msg_send![menu_bar, addItem: edit_menu_item];

            // ── Window menu ──────────────────────────────────────────────
            let window_title = nsstring("Window");
            let window_menu: *mut Object = msg_send![menu_cls, alloc];
            let window_menu: *mut Object = msg_send![window_menu, initWithTitle: window_title];

            let minimize = menu_item("Minimize", sel!(performMiniaturize:), "m");
            let _: () = msg_send![window_menu, addItem: minimize];

            let zoom = menu_item("Zoom", sel!(performZoom:), "");
            let _: () = msg_send![window_menu, addItem: zoom];

            let window_menu_item: *mut Object = msg_send![mi_cls, new];
            let _: () = msg_send![window_menu_item, setSubmenu: window_menu];
            let _: () = msg_send![menu_bar, addItem: window_menu_item];

            // ── Activate ─────────────────────────────────────────────────
            let _: () = msg_send![app, setMainMenu: menu_bar];
        }
    }

    pub fn show_about_panel() {
        unsafe {
            let Some(app_cls) = Class::get("NSApplication") else { return };
            let app: *mut Object = msg_send![app_cls, sharedApplication];
            let dict = about_options_dict();
            let _: () = msg_send![app, orderFrontStandardAboutPanelWithOptions: dict];
        }
    }
}

// ── Windows ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use super::{APP_AUTHOR, APP_NAME, APP_VERSION, APP_YEAR};

    pub fn show_about_dialog() {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

        let title = HSTRING::from(format!("About {APP_NAME}"));
        let message = HSTRING::from(format!(
            "{APP_NAME}\nVersion {APP_VERSION}\n\nCopyright © 2025 {APP_AUTHOR}"
        ));

        unsafe {
            MessageBoxW(None, &message, &title, MB_OK | MB_ICONINFORMATION);
        }
    }
}
