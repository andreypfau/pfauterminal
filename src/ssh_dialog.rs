use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextRenderer, Viewport,
};
use wgpu::*;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::colors::{hex_to_glyphon_color, hex_to_linear_f32};
use crate::font;
use crate::gpu::{pick_srgb_format, push_text_specs, RoundedRectPipeline};
use crate::layout::{push_stroked_rounded_rect, Rect, RoundedQuad};
use crate::terminal_panel::TextSpec;

// Design constants (logical pixels, from Pencil spec)
const DIALOG_WIDTH: f32 = 620.0;
const DIALOG_BORDER_WIDTH: f32 = 1.0;

const TITLE_BAR_HEIGHT: f32 = 40.0;

const FORM_PAD_V: f32 = 20.0;
const FORM_PAD_H: f32 = 28.0;
const FORM_ROW_GAP: f32 = 24.0;

const LABEL_WIDTH: f32 = 160.0;
const FIELD_HEIGHT: f32 = 36.0;
const FIELD_PAD_H: f32 = 10.0;
const FIELD_RADIUS: f32 = 4.0;
const FIELD_GAP: f32 = 16.0;

const PORT_FIELD_WIDTH: f32 = 56.0;
const PORT_SPACER_WIDTH: f32 = 112.0;

const BROWSE_BTN_SIZE: f32 = 36.0;

const FOOTER_PAD_V: f32 = 20.0;
const FOOTER_PAD_H: f32 = 28.0;
const FOOTER_GAP: f32 = 12.0;

const BUTTON_RADIUS: f32 = 6.0;
const CANCEL_PAD_H: f32 = 20.0;
const CANCEL_PAD_V: f32 = 8.0;
const OK_PAD_H: f32 = 28.0;

const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT_MULT: f32 = 1.2;
const SMALL_FONT_SIZE: f32 = 11.0;

const DROPDOWN_ITEM_HEIGHT: f32 = 32.0;
const DROPDOWN_PAD: f32 = 6.0;
const DROPDOWN_ITEM_PAD_H: f32 = 12.0;
const DROPDOWN_ITEM_RADIUS: f32 = 6.0;
const DROPDOWN_CORNER_RADIUS: f32 = 8.0;
const DROPDOWN_SHADOW_SPREAD: f32 = 20.0;
const DROPDOWN_SHADOW_OFFSET_Y: f32 = 4.0;
// Max rounded rects for the dialog
const MAX_ROUNDED_RECTS: usize = 80;

// Buffer indices
const BUF_TITLE: usize = 0;
const BUF_HOST_LABEL: usize = 1;
const BUF_PORT_LABEL: usize = 2;
const BUF_USERNAME_LABEL: usize = 3;
const BUF_AUTH_LABEL: usize = 4;
const BUF_AUTH_VALUE: usize = 5;
const BUF_CANCEL: usize = 6;
const BUF_OK: usize = 7;
const BUF_HOST_VALUE: usize = 8;
const BUF_PORT_VALUE: usize = 9;
const BUF_USERNAME_VALUE: usize = 10;
const BUF_PASSWORD_LABEL: usize = 11;
const BUF_PASSWORD_VALUE: usize = 12;
const BUF_KEYPATH_LABEL: usize = 13;
const BUF_KEYPATH_VALUE: usize = 14;
const BUF_PASSPHRASE_LABEL: usize = 15;
const BUF_PASSPHRASE_VALUE: usize = 16;
// Dropdown items
const BUF_DD_PASSWORD: usize = 17;
const BUF_DD_KEY: usize = 18;
const BUF_DD_KEY_HINT: usize = 19;
const BUF_DD_AGENT: usize = 20;
// Chevron placeholder (we draw it as a shape, but we need the buffer slot)
const BUF_BROWSE_ICON: usize = 21;
const BUF_COUNT: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialogField {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
}

impl DialogField {
    fn buf_index(self) -> usize {
        match self {
            DialogField::Host => BUF_HOST_VALUE,
            DialogField::Port => BUF_PORT_VALUE,
            DialogField::Username => BUF_USERNAME_VALUE,
            DialogField::Password => BUF_PASSWORD_VALUE,
            DialogField::KeyPath => BUF_KEYPATH_VALUE,
            DialogField::Passphrase => BUF_PASSPHRASE_VALUE,
        }
    }

    fn cursor_idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DialogHover {
    None,
    CancelButton,
    OkButton,
    AuthDropdown,
    BrowseButton,
}

#[derive(Debug)]
enum DialogHit {
    Field(DialogField),
    AuthDropdown,
    DropdownItem(AuthMethod),
    BrowseButton,
    CancelButton,
    OkButton,
    Inside,
    Outside, // clicked outside dropdown
}

struct SshDialogDrawCommands {
    rounded_quads: Vec<RoundedQuad>,
    text_areas: Vec<TextSpec>,
    /// Overlay layer (dropdown popup) — rendered after base text, on top of everything.
    overlay_rounded_quads: Vec<RoundedQuad>,
    overlay_text_areas: Vec<TextSpec>,
}

/// Authentication method selected in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    Key,
    Agent,
}

impl AuthMethod {
    fn display_text(self) -> &'static str {
        match self {
            AuthMethod::Password => "Password",
            AuthMethod::Key => "Key pair",
            AuthMethod::Agent => "OpenSSH config and authentication agent",
        }
    }
}

/// Result from the SSH dialog when user clicks OK.
#[derive(Debug)]
pub struct SshResult {
    pub host: String,
    pub port: String,
    pub username: String,
    pub auth_method: AuthMethod,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
}

impl SshResult {
    pub fn to_ssh_config(&self) -> crate::ssh::SshConfig {
        let port = self.port.parse::<u16>().unwrap_or(22);
        let auth = match self.auth_method {
            AuthMethod::Password => crate::ssh::SshAuth::Password(self.password.clone()),
            AuthMethod::Key => crate::ssh::SshAuth::Key {
                path: self.key_path.clone(),
                passphrase: if self.passphrase.is_empty() {
                    None
                } else {
                    Some(self.passphrase.clone())
                },
            },
            AuthMethod::Agent => crate::ssh::SshAuth::Agent,
        };
        crate::ssh::SshConfig {
            host: self.host.clone(),
            port,
            username: self.username.clone(),
            auth,
        }
    }
}

/// Dialog state: field values, focus, layout rects, text buffers.
struct SshDialog {
    host: String,
    port: String,
    username: String,
    password: String,
    key_path: String,
    passphrase: String,
    auth_method: AuthMethod,
    dropdown_open: bool,
    focused_field: DialogField,
    cursor_pos: [usize; 6], // Host, Port, Username, Password, KeyPath, Passphrase
    // Layout rects (physical pixels, relative to window origin)
    field_rects: [Rect; 6],   // indexed by DialogField as usize
    auth_dropdown_rect: Rect, // the dropdown trigger button
    browse_btn_rect: Rect,
    // Dropdown popup rects
    dropdown_popup_rect: Rect,
    dd_item_password_rect: Rect,
    dd_item_key_rect: Rect,
    dd_item_agent_rect: Rect,
    cancel_rect: Rect,
    ok_rect: Rect,
    hover: DialogHover,
    dd_hover_item: Option<AuthMethod>,
    char_width: f32,
    buffers: Vec<Buffer>,
    scale: f32,
}

impl SshDialog {
    fn new(scale: f32, font_system: &mut FontSystem) -> Self {
        let mut dialog = Self {
            host: String::new(),
            port: "22".to_string(),
            username: String::new(),
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
            auth_method: AuthMethod::Agent,
            dropdown_open: false,
            focused_field: DialogField::Host,
            cursor_pos: [0, 2, 0, 0, 0, 0],
            field_rects: [Rect::ZERO; 6],
            auth_dropdown_rect: Rect::ZERO,
            browse_btn_rect: Rect::ZERO,
            dropdown_popup_rect: Rect::ZERO,
            dd_item_password_rect: Rect::ZERO,
            dd_item_key_rect: Rect::ZERO,
            dd_item_agent_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            ok_rect: Rect::ZERO,
            hover: DialogHover::None,
            dd_hover_item: None,
            char_width: 8.0,
            buffers: Vec::new(),
            scale,
        };
        dialog.compute_layout(scale, font_system);
        dialog
    }

    fn compute_layout(&mut self, scale: f32, font_system: &mut FontSystem) {
        self.scale = scale;
        let s = scale;
        let metrics = Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT_MULT);
        let attrs = Attrs::new().family(Family::Name("JetBrains Mono"));

        // Measure character width
        let mut test_buf = Buffer::new(font_system, metrics);
        test_buf.set_size(font_system, Some(100.0), Some(metrics.line_height));
        test_buf.set_text(font_system, "M", attrs, Shaping::Basic);
        test_buf.shape_until_scroll(font_system, false);
        self.char_width = test_buf
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.w))
            .unwrap_or(FONT_SIZE * 0.6);

        let line_h = FONT_SIZE * LINE_HEIGHT_MULT;
        let form_x = FORM_PAD_H * s;
        let form_w = DIALOG_WIDTH * s - 2.0 * FORM_PAD_H * s;
        let field_h = FIELD_HEIGHT * s;
        let field_gap = FIELD_GAP * s;
        let port_w = PORT_FIELD_WIDTH * s;
        let port_section_w = PORT_SPACER_WIDTH * s;
        let label_w = LABEL_WIDTH * s;
        let host_input_w = form_w - label_w - field_gap - port_section_w;

        let mut row_y = TITLE_BAR_HEIGHT * s + FORM_PAD_V * s;

        // Host row
        self.field_rects[DialogField::Host as usize] = Rect {
            x: form_x + label_w + field_gap,
            y: row_y,
            width: host_input_w,
            height: field_h,
        };
        self.field_rects[DialogField::Port as usize] = Rect {
            x: form_x + form_w - port_w,
            y: row_y,
            width: port_w,
            height: field_h,
        };
        row_y += field_h + FORM_ROW_GAP * s;

        // Username row
        let username_input_w = form_w - label_w - field_gap - PORT_SPACER_WIDTH * s;
        self.field_rects[DialogField::Username as usize] = Rect {
            x: form_x + label_w + field_gap,
            y: row_y,
            width: username_input_w,
            height: field_h,
        };
        row_y += field_h + FORM_ROW_GAP * s;

        // Auth dropdown row
        let auth_x = form_x + label_w + field_gap;
        let auth_w = form_w - label_w - field_gap;
        self.auth_dropdown_rect = Rect {
            x: auth_x,
            y: row_y,
            width: auth_w,
            height: field_h,
        };
        row_y += field_h + FORM_ROW_GAP * s;

        // Credential rows (conditional, but always laid out at same position)
        let cred_input_w = form_w - label_w - field_gap;

        // Password field
        self.field_rects[DialogField::Password as usize] = Rect {
            x: form_x + label_w + field_gap,
            y: row_y,
            width: cred_input_w,
            height: field_h,
        };

        // Key path field (narrower to make room for browse button)
        let browse_w = BROWSE_BTN_SIZE * s;
        self.field_rects[DialogField::KeyPath as usize] = Rect {
            x: form_x + label_w + field_gap,
            y: row_y,
            width: cred_input_w - browse_w - field_gap,
            height: field_h,
        };
        self.browse_btn_rect = Rect {
            x: form_x + label_w + field_gap + cred_input_w - browse_w,
            y: row_y,
            width: browse_w,
            height: field_h,
        };

        // Passphrase field (only for Key auth, row below key path)
        let passphrase_y = row_y + field_h + FORM_ROW_GAP * s;
        self.field_rects[DialogField::Passphrase as usize] = Rect {
            x: form_x + label_w + field_gap,
            y: passphrase_y,
            width: cred_input_w,
            height: field_h,
        };

        // Dropdown popup (positioned below the auth dropdown)
        let dd_total_h = DROPDOWN_PAD * 2.0 + DROPDOWN_ITEM_HEIGHT * 3.0;
        self.dropdown_popup_rect = Rect {
            x: auth_x,
            y: self.auth_dropdown_rect.y + self.auth_dropdown_rect.height + 4.0 * s,
            width: auth_w,
            height: dd_total_h * s,
        };
        let dd_item_x = self.dropdown_popup_rect.x + DROPDOWN_PAD * s;
        let dd_item_w = self.dropdown_popup_rect.width - 2.0 * DROPDOWN_PAD * s;
        let dd_item_h = DROPDOWN_ITEM_HEIGHT * s;
        let mut dd_y = self.dropdown_popup_rect.y + DROPDOWN_PAD * s;
        self.dd_item_password_rect = Rect {
            x: dd_item_x,
            y: dd_y,
            width: dd_item_w,
            height: dd_item_h,
        };
        dd_y += dd_item_h;
        self.dd_item_key_rect = Rect {
            x: dd_item_x,
            y: dd_y,
            width: dd_item_w,
            height: dd_item_h,
        };
        dd_y += dd_item_h;
        self.dd_item_agent_rect = Rect {
            x: dd_item_x,
            y: dd_y,
            width: dd_item_w,
            height: dd_item_h,
        };

        // Footer buttons
        let button_h = CANCEL_PAD_V * 2.0 + line_h;
        let dialog_h = self.compute_dialog_height();
        let footer_y = (dialog_h - FOOTER_PAD_V - button_h) * s;
        let cancel_text_w = self.char_width * 6.0;
        let ok_text_w = self.char_width * 2.0;
        let cancel_w = (CANCEL_PAD_H * 2.0 + cancel_text_w) * s;
        let ok_w = (OK_PAD_H * 2.0 + ok_text_w) * s;

        let btn_right = DIALOG_WIDTH * s - FOOTER_PAD_H * s;
        self.ok_rect = Rect {
            x: btn_right - ok_w,
            y: footer_y,
            width: ok_w,
            height: button_h * s,
        };
        self.cancel_rect = Rect {
            x: btn_right - ok_w - FOOTER_GAP * s - cancel_w,
            y: footer_y,
            width: cancel_w,
            height: button_h * s,
        };

        self.init_buffers(font_system);
    }

    fn compute_dialog_height(&self) -> f32 {
        let line_h = FONT_SIZE * LINE_HEIGHT_MULT;
        // Fixed height: always reserve space for the tallest auth mode (Key pair: 2 extra rows)
        let form_content_h = FIELD_HEIGHT       // Host + Port row
            + FORM_ROW_GAP
            + FIELD_HEIGHT                      // Username row
            + FORM_ROW_GAP
            + FIELD_HEIGHT                      // Auth dropdown row
            + FORM_ROW_GAP + FIELD_HEIGHT       // Key path / Password row
            + FORM_ROW_GAP + FIELD_HEIGHT; // Passphrase row

        let form_h = FORM_PAD_V + form_content_h + FORM_PAD_V;
        let button_h = CANCEL_PAD_V * 2.0 + line_h;
        let footer_h = FOOTER_PAD_V + button_h + FOOTER_PAD_V;
        TITLE_BAR_HEIGHT + form_h + footer_h
    }

    fn init_buffers(&mut self, font_system: &mut FontSystem) {
        let metrics = Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT_MULT);
        let small_metrics = Metrics::new(SMALL_FONT_SIZE, SMALL_FONT_SIZE * LINE_HEIGHT_MULT);
        let attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
        let semibold_attrs = attrs.weight(glyphon::Weight::SEMIBOLD);

        let password_masked = "*".repeat(self.password.chars().count());
        let passphrase_masked = "*".repeat(self.passphrase.chars().count());
        let auth_display = self.auth_method.display_text().to_string();
        let key_path_display = if self.key_path.is_empty() {
            "~/.ssh/id_ed25519"
        } else {
            &self.key_path
        };

        let entries: [(usize, &str, Metrics, Attrs); BUF_COUNT] = [
            (BUF_TITLE, "SSH Session", metrics, semibold_attrs),
            (BUF_HOST_LABEL, "Host:", metrics, attrs),
            (BUF_PORT_LABEL, "Port:", metrics, attrs),
            (BUF_USERNAME_LABEL, "Username:", metrics, attrs),
            (BUF_AUTH_LABEL, "Authentication type:", metrics, attrs),
            (BUF_AUTH_VALUE, "", metrics, attrs), // filled dynamically
            (BUF_CANCEL, "Cancel", metrics, attrs),
            (BUF_OK, "OK", metrics, semibold_attrs),
            (BUF_HOST_VALUE, "", metrics, attrs),
            (BUF_PORT_VALUE, "", metrics, attrs),
            (BUF_USERNAME_VALUE, "", metrics, attrs),
            (BUF_PASSWORD_LABEL, "Password:", metrics, attrs),
            (BUF_PASSWORD_VALUE, "", metrics, attrs),
            (
                BUF_KEYPATH_LABEL,
                "Private key file:",
                metrics,
                semibold_attrs,
            ),
            (BUF_KEYPATH_VALUE, "", metrics, attrs),
            (BUF_PASSPHRASE_LABEL, "Passphrase:", metrics, attrs),
            (BUF_PASSPHRASE_VALUE, "", metrics, attrs),
            (BUF_DD_PASSWORD, "Password", metrics, attrs),
            (BUF_DD_KEY, "Key pair", metrics, attrs),
            (BUF_DD_KEY_HINT, "OpenSSH or PuTTY", small_metrics, attrs),
            (
                BUF_DD_AGENT,
                "OpenSSH config and authentication agent",
                metrics,
                attrs,
            ),
            (BUF_BROWSE_ICON, "", metrics, attrs), // placeholder
        ];

        while self.buffers.len() < BUF_COUNT {
            self.buffers.push(Buffer::new(font_system, metrics));
        }

        for &(idx, static_text, m, a) in &entries {
            let text = match idx {
                BUF_HOST_VALUE => &self.host,
                BUF_PORT_VALUE => &self.port,
                BUF_USERNAME_VALUE => &self.username,
                BUF_PASSWORD_VALUE => &password_masked,
                BUF_KEYPATH_VALUE => key_path_display,
                BUF_PASSPHRASE_VALUE => &passphrase_masked,
                BUF_AUTH_VALUE => &auth_display,
                _ => static_text,
            };
            let buf = &mut self.buffers[idx];
            buf.set_metrics(font_system, m);
            buf.set_size(font_system, Some(600.0), Some(m.line_height));
            buf.set_text(font_system, text, a, Shaping::Basic);
            buf.shape_until_scroll(font_system, false);
        }
    }

    fn update_field_buffer(&mut self, field: DialogField, font_system: &mut FontSystem) {
        let metrics = Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT_MULT);
        let attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
        let idx = field.buf_index();
        let display_text;
        let text: &str = match field {
            DialogField::Host => &self.host,
            DialogField::Port => &self.port,
            DialogField::Username => &self.username,
            DialogField::Password => {
                display_text = "*".repeat(self.password.chars().count());
                &display_text
            }
            DialogField::KeyPath => {
                if self.key_path.is_empty() {
                    "~/.ssh/id_ed25519"
                } else {
                    &self.key_path
                }
            }
            DialogField::Passphrase => {
                display_text = "*".repeat(self.passphrase.chars().count());
                &display_text
            }
        };
        let buf = &mut self.buffers[idx];
        buf.set_metrics(font_system, metrics);
        buf.set_size(font_system, Some(600.0), Some(metrics.line_height));
        buf.set_text(font_system, text, attrs, Shaping::Basic);
        buf.shape_until_scroll(font_system, false);
    }

    fn update_auth_value_buffer(&mut self, font_system: &mut FontSystem) {
        let metrics = Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT_MULT);
        let attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
        let text = self.auth_method.display_text();
        let buf = &mut self.buffers[BUF_AUTH_VALUE];
        buf.set_metrics(font_system, metrics);
        buf.set_size(font_system, Some(600.0), Some(metrics.line_height));
        buf.set_text(font_system, text, attrs, Shaping::Basic);
        buf.shape_until_scroll(font_system, false);
    }

    fn field_value(&self, field: DialogField) -> &str {
        match field {
            DialogField::Host => &self.host,
            DialogField::Port => &self.port,
            DialogField::Username => &self.username,
            DialogField::Password => &self.password,
            DialogField::KeyPath => &self.key_path,
            DialogField::Passphrase => &self.passphrase,
        }
    }

    fn field_value_mut(&mut self, field: DialogField) -> &mut String {
        match field {
            DialogField::Host => &mut self.host,
            DialogField::Port => &mut self.port,
            DialogField::Username => &mut self.username,
            DialogField::Password => &mut self.password,
            DialogField::KeyPath => &mut self.key_path,
            DialogField::Passphrase => &mut self.passphrase,
        }
    }

    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(byte_pos, _)| byte_pos)
            .unwrap_or(s.len())
    }

    fn field_char_count(&self, field: DialogField) -> usize {
        self.field_value(field).chars().count()
    }

    fn field_rect(&self, field: DialogField) -> Rect {
        self.field_rects[field as usize]
    }

    fn compute_hover(&self, x: f32, y: f32) -> DialogHover {
        if self.cancel_rect.contains(x, y) {
            return DialogHover::CancelButton;
        }
        if self.ok_rect.contains(x, y) {
            return DialogHover::OkButton;
        }
        if self.auth_dropdown_rect.contains(x, y) {
            return DialogHover::AuthDropdown;
        }
        if self.auth_method == AuthMethod::Key && self.browse_btn_rect.contains(x, y) {
            return DialogHover::BrowseButton;
        }
        DialogHover::None
    }

    fn compute_dd_hover(&self, x: f32, y: f32) -> Option<AuthMethod> {
        if !self.dropdown_open {
            return None;
        }
        if self.dd_item_password_rect.contains(x, y) {
            return Some(AuthMethod::Password);
        }
        if self.dd_item_key_rect.contains(x, y) {
            return Some(AuthMethod::Key);
        }
        if self.dd_item_agent_rect.contains(x, y) {
            return Some(AuthMethod::Agent);
        }
        None
    }

    fn set_hover(&mut self, hover: DialogHover) -> bool {
        if self.hover != hover {
            self.hover = hover;
            true
        } else {
            false
        }
    }

    fn hit_test(&self, x: f32, y: f32) -> DialogHit {
        // If dropdown is open, test dropdown items first
        if self.dropdown_open {
            if self.dd_item_password_rect.contains(x, y) {
                return DialogHit::DropdownItem(AuthMethod::Password);
            }
            if self.dd_item_key_rect.contains(x, y) {
                return DialogHit::DropdownItem(AuthMethod::Key);
            }
            if self.dd_item_agent_rect.contains(x, y) {
                return DialogHit::DropdownItem(AuthMethod::Agent);
            }
            // Clicked somewhere else while dropdown open — close it
            return DialogHit::Outside;
        }

        for field in [DialogField::Host, DialogField::Port, DialogField::Username] {
            if self.field_rect(field).contains(x, y) {
                return DialogHit::Field(field);
            }
        }
        if self.auth_dropdown_rect.contains(x, y) {
            return DialogHit::AuthDropdown;
        }
        // Credential fields: only active based on auth method
        match self.auth_method {
            AuthMethod::Password => {
                if self.field_rect(DialogField::Password).contains(x, y) {
                    return DialogHit::Field(DialogField::Password);
                }
            }
            AuthMethod::Key => {
                if self.field_rect(DialogField::KeyPath).contains(x, y) {
                    return DialogHit::Field(DialogField::KeyPath);
                }
                if self.browse_btn_rect.contains(x, y) {
                    return DialogHit::BrowseButton;
                }
                if self.field_rect(DialogField::Passphrase).contains(x, y) {
                    return DialogHit::Field(DialogField::Passphrase);
                }
            }
            AuthMethod::Agent => {}
        }
        if self.cancel_rect.contains(x, y) {
            return DialogHit::CancelButton;
        }
        if self.ok_rect.contains(x, y) {
            return DialogHit::OkButton;
        }
        DialogHit::Inside
    }

    fn handle_click(&mut self, x: f32, y: f32, font_system: &mut FontSystem) -> Option<SshResult> {
        match self.hit_test(x, y) {
            DialogHit::Field(field) => {
                self.dropdown_open = false;
                self.focused_field = field;
                let rect = self.field_rect(field);
                let pad = FIELD_PAD_H * self.scale;
                let rel_x = (x - rect.x - pad).max(0.0);
                let char_w = self.char_width * self.scale;
                let pos = (rel_x / char_w).round() as usize;
                let char_count = self.field_char_count(field);
                self.cursor_pos[field.cursor_idx()] = pos.min(char_count);
                None
            }
            DialogHit::AuthDropdown => {
                self.dropdown_open = !self.dropdown_open;
                None
            }
            DialogHit::DropdownItem(method) => {
                let old_method = self.auth_method;
                self.auth_method = method;
                self.dropdown_open = false;
                if old_method != method {
                    self.update_auth_value_buffer(font_system);
                    // Focus the first credential field for the method
                    match method {
                        AuthMethod::Password => self.focused_field = DialogField::Password,
                        AuthMethod::Key => self.focused_field = DialogField::KeyPath,
                        AuthMethod::Agent => {}
                    }
                }
                None
            }
            DialogHit::BrowseButton => {
                // Open file picker (handled by caller)
                None
            }
            DialogHit::Outside => {
                self.dropdown_open = false;
                None
            }
            DialogHit::OkButton => Some(self.build_result()),
            DialogHit::CancelButton => None,
            DialogHit::Inside => {
                self.dropdown_open = false;
                None
            }
        }
    }

    fn handle_key(&mut self, event: &KeyEvent, font_system: &mut FontSystem) -> Option<SshResult> {
        if event.state != ElementState::Pressed {
            return None;
        }

        // Close dropdown on any key press
        if self.dropdown_open {
            self.dropdown_open = false;
            return None;
        }

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Enter) => {
                return Some(self.build_result());
            }
            Key::Named(NamedKey::Tab) => {
                self.focused_field = match (self.focused_field, self.auth_method) {
                    (DialogField::Username, AuthMethod::Password) => DialogField::Password,
                    (DialogField::Username, AuthMethod::Key) => DialogField::KeyPath,
                    (DialogField::Username, AuthMethod::Agent) => DialogField::Host,
                    (DialogField::Password, _) => DialogField::Host,
                    (DialogField::KeyPath, _) => DialogField::Passphrase,
                    (DialogField::Passphrase, _) => DialogField::Host,
                    (DialogField::Host, _) => DialogField::Port,
                    (DialogField::Port, _) => DialogField::Username,
                };
                return None;
            }
            Key::Named(NamedKey::Backspace) => {
                let ci = self.focused_field.cursor_idx();
                let char_pos = self.cursor_pos[ci];
                if char_pos > 0 {
                    let byte_start =
                        Self::char_to_byte(self.field_value(self.focused_field), char_pos - 1);
                    let byte_end =
                        Self::char_to_byte(self.field_value(self.focused_field), char_pos);
                    let val = self.field_value_mut(self.focused_field);
                    val.drain(byte_start..byte_end);
                    self.cursor_pos[ci] = char_pos - 1;
                    self.update_field_buffer(self.focused_field, font_system);
                }
                return None;
            }
            Key::Named(NamedKey::Delete) => {
                let ci = self.focused_field.cursor_idx();
                let char_pos = self.cursor_pos[ci];
                let char_count = self.field_char_count(self.focused_field);
                if char_pos < char_count {
                    let byte_start =
                        Self::char_to_byte(self.field_value(self.focused_field), char_pos);
                    let byte_end =
                        Self::char_to_byte(self.field_value(self.focused_field), char_pos + 1);
                    let val = self.field_value_mut(self.focused_field);
                    val.drain(byte_start..byte_end);
                    self.update_field_buffer(self.focused_field, font_system);
                }
                return None;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let ci = self.focused_field.cursor_idx();
                if self.cursor_pos[ci] > 0 {
                    self.cursor_pos[ci] -= 1;
                }
                return None;
            }
            Key::Named(NamedKey::ArrowRight) => {
                let ci = self.focused_field.cursor_idx();
                let char_count = self.field_char_count(self.focused_field);
                if self.cursor_pos[ci] < char_count {
                    self.cursor_pos[ci] += 1;
                }
                return None;
            }
            Key::Named(NamedKey::Home) => {
                self.cursor_pos[self.focused_field.cursor_idx()] = 0;
                return None;
            }
            Key::Named(NamedKey::End) => {
                let char_count = self.field_char_count(self.focused_field);
                self.cursor_pos[self.focused_field.cursor_idx()] = char_count;
                return None;
            }
            _ => {}
        }

        if let Some(text) = &event.text {
            let s: String = text.to_string();
            for c in s.chars() {
                if c.is_control() {
                    continue;
                }
                let ci = self.focused_field.cursor_idx();
                let char_pos = self.cursor_pos[ci];
                let byte_pos = Self::char_to_byte(self.field_value(self.focused_field), char_pos);
                let val = self.field_value_mut(self.focused_field);
                val.insert(byte_pos, c);
                self.cursor_pos[ci] = char_pos + 1;
            }
            self.update_field_buffer(self.focused_field, font_system);
        }

        None
    }

    fn insert_text(&mut self, text: &str, font_system: &mut FontSystem) {
        let ci = self.focused_field.cursor_idx();
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            let char_pos = self.cursor_pos[ci];
            let byte_pos = Self::char_to_byte(self.field_value(self.focused_field), char_pos);
            let val = self.field_value_mut(self.focused_field);
            val.insert(byte_pos, c);
            self.cursor_pos[ci] = char_pos + 1;
        }
        self.update_field_buffer(self.focused_field, font_system);
    }

    fn set_key_path(&mut self, path: String, font_system: &mut FontSystem) {
        self.cursor_pos[DialogField::KeyPath.cursor_idx()] = path.chars().count();
        self.key_path = path;
        self.update_field_buffer(DialogField::KeyPath, font_system);
    }

    fn build_result(&self) -> SshResult {
        SshResult {
            host: self.host.clone(),
            port: self.port.clone(),
            username: self.username.clone(),
            auth_method: self.auth_method,
            password: self.password.clone(),
            key_path: if self.key_path.is_empty() {
                "~/.ssh/id_ed25519".to_string()
            } else {
                self.key_path.clone()
            },
            passphrase: self.passphrase.clone(),
        }
    }

    fn draw_commands(
        &self,
        scale: f32,
        colors: &crate::colors::ColorScheme,
    ) -> SshDialogDrawCommands {
        let mut rq = Vec::new();
        let mut ta = Vec::new();

        let s = scale;
        let border = DIALOG_BORDER_WIDTH * s;
        let line_h = FONT_SIZE * LINE_HEIGHT_MULT;
        let field_pad = FIELD_PAD_H * s;
        let dialog_w = DIALOG_WIDTH * s;
        let dialog_h = self.compute_dialog_height() * s;
        let dialog_rect = Rect {
            x: 0.0,
            y: 0.0,
            width: dialog_w,
            height: dialog_h,
        };

        let form_x = FORM_PAD_H * s;
        let field_gap = FIELD_GAP * s;
        let field_h = FIELD_HEIGHT * s;
        let row_gap = FORM_ROW_GAP * s;

        // Title text
        let title_x = form_x;
        let title_y = (TITLE_BAR_HEIGHT * s - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_TITLE,
            left: title_x,
            top: title_y,
            bounds: dialog_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });

        // Title bar bottom border
        rq.push(RoundedQuad {
            rect: Rect {
                x: 0.0,
                y: TITLE_BAR_HEIGHT * s - border,
                width: dialog_w,
                height: border,
            },
            color: hex_to_linear_f32(&colors.dropdown_border),
            radius: 0.0,
            shadow_softness: 0.0,
        });

        let mut row_y = TITLE_BAR_HEIGHT * s + FORM_PAD_V * s;

        // --- Host row ---
        let label_y = row_y + (field_h - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_HOST_LABEL,
            left: form_x,
            top: label_y,
            bounds: dialog_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });
        let host_rect = self.field_rect(DialogField::Host);
        self.draw_input_field(
            &mut rq,
            &mut ta,
            &host_rect,
            BUF_HOST_VALUE,
            self.focused_field == DialogField::Host,
            DialogField::Host,
            s,
            false,
            colors,
        );

        // Port label
        let port_label_x = host_rect.x + host_rect.width + field_gap;
        ta.push(TextSpec {
            buffer_index: BUF_PORT_LABEL,
            left: port_label_x,
            top: label_y,
            bounds: dialog_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });
        self.draw_input_field(
            &mut rq,
            &mut ta,
            &self.field_rect(DialogField::Port),
            BUF_PORT_VALUE,
            self.focused_field == DialogField::Port,
            DialogField::Port,
            s,
            false,
            colors,
        );

        row_y += field_h + row_gap;

        // --- Username row ---
        let label_y = row_y + (field_h - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_USERNAME_LABEL,
            left: form_x,
            top: label_y,
            bounds: dialog_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });
        self.draw_input_field(
            &mut rq,
            &mut ta,
            &self.field_rect(DialogField::Username),
            BUF_USERNAME_VALUE,
            self.focused_field == DialogField::Username,
            DialogField::Username,
            s,
            false,
            colors,
        );

        row_y += field_h + row_gap;

        // --- Auth dropdown row ---
        let label_y = row_y + (field_h - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_AUTH_LABEL,
            left: form_x,
            top: label_y,
            bounds: dialog_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });

        let fr = FIELD_RADIUS * s;
        // Dropdown trigger button
        let dd = &self.auth_dropdown_rect;
        push_stroked_rounded_rect(
            &mut rq,
            dd,
            hex_to_linear_f32(&colors.text_dim),
            hex_to_linear_f32(&colors.tab_hover_bg),
            fr,
            1.0 * s,
        );
        let text_y = dd.y + (dd.height - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_AUTH_VALUE,
            left: dd.x + field_pad,
            top: text_y,
            bounds: *dd,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });

        // Chevron-down indicator (draw as a small V shape using two thin quads)
        let chevron_size = 14.0 * s;
        let chevron_x = dd.x + dd.width - field_pad - chevron_size;
        let chevron_cy = dd.y + dd.height / 2.0;
        let chev_half = chevron_size / 2.0;
        let chev_thick = 1.5 * s;
        // Left leg of chevron: goes from top-left to bottom-center
        // We approximate with a small rotated rect — but since we only have rounded rects,
        // draw two small lines
        // Draw chevron as horizontal bars forming a V shape
        let chev_steps = 4;
        let chev_col = hex_to_linear_f32(&colors.text_placeholder);
        for i in 0..chev_steps {
            let frac = i as f32 / (chev_steps - 1) as f32;
            let bar_y = chevron_cy - chev_half * 0.4 + chev_half * 0.8 * frac;
            let indent = chev_half * (1.0 - frac);
            let bar_w = chevron_size - 2.0 * indent;
            rq.push(RoundedQuad {
                rect: Rect {
                    x: chevron_x + indent,
                    y: bar_y,
                    width: bar_w,
                    height: chev_thick,
                },
                color: chev_col,
                radius: 0.0,
                shadow_softness: 0.0,
            });
        }

        row_y += field_h + row_gap;

        // --- Credential rows (always rendered; dropdown overlays on top) ---
        match self.auth_method {
            AuthMethod::Password => {
                let label_y = row_y + (field_h - line_h * s) / 2.0;
                ta.push(TextSpec {
                    buffer_index: BUF_PASSWORD_LABEL,
                    left: form_x,
                    top: label_y,
                    bounds: dialog_rect,
                    color: hex_to_glyphon_color(&colors.dropdown_text),
                });
                self.draw_input_field(
                    &mut rq,
                    &mut ta,
                    &self.field_rect(DialogField::Password),
                    BUF_PASSWORD_VALUE,
                    self.focused_field == DialogField::Password,
                    DialogField::Password,
                    s,
                    false,
                    colors,
                );
            }
            AuthMethod::Key => {
                // Private key file row
                let label_y = row_y + (field_h - line_h * s) / 2.0;
                ta.push(TextSpec {
                    buffer_index: BUF_KEYPATH_LABEL,
                    left: form_x,
                    top: label_y,
                    bounds: dialog_rect,
                    color: hex_to_glyphon_color(&colors.dropdown_text),
                });
                self.draw_input_field(
                    &mut rq,
                    &mut ta,
                    &self.field_rect(DialogField::KeyPath),
                    BUF_KEYPATH_VALUE,
                    self.focused_field == DialogField::KeyPath,
                    DialogField::KeyPath,
                    s,
                    self.key_path.is_empty(),
                    colors,
                );

                // Browse button
                let br = &self.browse_btn_rect;
                let is_browse_hover = self.hover == DialogHover::BrowseButton;
                push_stroked_rounded_rect(
                    &mut rq,
                    br,
                    hex_to_linear_f32(&colors.field_border),
                    if is_browse_hover {
                        hex_to_linear_f32(&colors.tab_hover_stroke)
                    } else {
                        hex_to_linear_f32(&colors.tab_hover_bg)
                    },
                    fr,
                    1.0 * s,
                );
                // Folder icon
                let icon_s = 16.0 * s;
                let icon_x = br.x + (br.width - icon_s) / 2.0;
                let icon_y = br.y + (br.height - icon_s) / 2.0;
                let folder_col = hex_to_linear_f32(&colors.text_placeholder);
                rq.push(RoundedQuad {
                    rect: Rect {
                        x: icon_x,
                        y: icon_y + icon_s * 0.25,
                        width: icon_s,
                        height: icon_s * 0.65,
                    },
                    color: folder_col,
                    radius: 2.0 * s,
                    shadow_softness: 0.0,
                });
                rq.push(RoundedQuad {
                    rect: Rect {
                        x: icon_x + 1.5 * s,
                        y: icon_y + icon_s * 0.25 + 1.5 * s,
                        width: icon_s - 3.0 * s,
                        height: icon_s * 0.65 - 3.0 * s,
                    },
                    color: hex_to_linear_f32(&colors.tab_hover_bg),
                    radius: 1.0 * s,
                    shadow_softness: 0.0,
                });
                rq.push(RoundedQuad {
                    rect: Rect {
                        x: icon_x,
                        y: icon_y + icon_s * 0.12,
                        width: icon_s * 0.45,
                        height: icon_s * 0.2,
                    },
                    color: folder_col,
                    radius: 1.5 * s,
                    shadow_softness: 0.0,
                });

                row_y += field_h + row_gap;

                // Passphrase row
                let label_y = row_y + (field_h - line_h * s) / 2.0;
                ta.push(TextSpec {
                    buffer_index: BUF_PASSPHRASE_LABEL,
                    left: form_x,
                    top: label_y,
                    bounds: dialog_rect,
                    color: hex_to_glyphon_color(&colors.dropdown_text),
                });
                self.draw_input_field(
                    &mut rq,
                    &mut ta,
                    &self.field_rect(DialogField::Passphrase),
                    BUF_PASSPHRASE_VALUE,
                    self.focused_field == DialogField::Passphrase,
                    DialogField::Passphrase,
                    s,
                    false,
                    colors,
                );
            }
            AuthMethod::Agent => {}
        }

        // --- Footer buttons ---
        let is_cancel_hover = self.hover == DialogHover::CancelButton;
        push_stroked_rounded_rect(
            &mut rq,
            &self.cancel_rect,
            hex_to_linear_f32(&colors.tab_hover_stroke),
            if is_cancel_hover {
                hex_to_linear_f32(&colors.tab_hover_stroke)
            } else {
                hex_to_linear_f32(&colors.tab_hover_bg)
            },
            BUTTON_RADIUS * s,
            1.0 * s,
        );
        let cancel_text_y = self.cancel_rect.y + (self.cancel_rect.height - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_CANCEL,
            left: self.cancel_rect.x + CANCEL_PAD_H * s,
            top: cancel_text_y,
            bounds: self.cancel_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text),
        });

        let is_ok_hover = self.hover == DialogHover::OkButton;
        rq.push(RoundedQuad {
            rect: self.ok_rect,
            color: if is_ok_hover {
                hex_to_linear_f32(&colors.ok_hover_bg)
            } else {
                hex_to_linear_f32(&colors.ok_bg)
            },
            radius: BUTTON_RADIUS * s,
            shadow_softness: 0.0,
        });
        let ok_text_y = self.ok_rect.y + (self.ok_rect.height - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: BUF_OK,
            left: self.ok_rect.x + OK_PAD_H * s,
            top: ok_text_y,
            bounds: self.ok_rect,
            color: hex_to_glyphon_color(&colors.dropdown_text_active),
        });

        // --- Dropdown popup (overlay layer — rendered after base text) ---
        let mut overlay_rq = Vec::new();
        let mut overlay_ta = Vec::new();

        if self.dropdown_open {
            let dp = &self.dropdown_popup_rect;
            let dd_r = DROPDOWN_CORNER_RADIUS * s;
            let shadow_spread = DROPDOWN_SHADOW_SPREAD * s;
            let shadow_offset_y = DROPDOWN_SHADOW_OFFSET_Y * s;

            // Drop shadow
            overlay_rq.push(RoundedQuad {
                rect: Rect {
                    x: dp.x,
                    y: dp.y + shadow_offset_y,
                    width: dp.width,
                    height: dp.height,
                },
                color: hex_to_linear_f32(&colors.dropdown_shadow),
                radius: dd_r,
                shadow_softness: shadow_spread,
            });

            // Border + fill
            push_stroked_rounded_rect(
                &mut overlay_rq,
                dp,
                hex_to_linear_f32(&colors.dropdown_border),
                hex_to_linear_f32(&colors.dropdown_bg),
                dd_r,
                1.0 * s,
            );

            let item_r = DROPDOWN_ITEM_RADIUS * s;
            let small_line_h = SMALL_FONT_SIZE * LINE_HEIGHT_MULT;

            let items: [(AuthMethod, Rect, usize, GlyphonColor); 3] = [
                (
                    AuthMethod::Password,
                    self.dd_item_password_rect,
                    BUF_DD_PASSWORD,
                    hex_to_glyphon_color(&colors.dropdown_text),
                ),
                (
                    AuthMethod::Key,
                    self.dd_item_key_rect,
                    BUF_DD_KEY,
                    hex_to_glyphon_color(&colors.dropdown_text),
                ),
                (
                    AuthMethod::Agent,
                    self.dd_item_agent_rect,
                    BUF_DD_AGENT,
                    if self.auth_method == AuthMethod::Agent {
                        hex_to_glyphon_color(&colors.dropdown_text_active)
                    } else {
                        hex_to_glyphon_color(&colors.dropdown_text)
                    },
                ),
            ];

            for (method, rect, buf_idx, text_color) in &items {
                draw_dropdown_item(
                    &mut overlay_rq,
                    &mut overlay_ta,
                    *rect,
                    *buf_idx,
                    self.auth_method == *method,
                    self.dd_hover_item == Some(*method),
                    item_r,
                    s,
                    line_h,
                    *text_color,
                    colors,
                );
            }

            // Key pair hint text
            let hint_offset = self.char_width * 9.0;
            let hint_text_y =
                self.dd_item_key_rect.y + (self.dd_item_key_rect.height - small_line_h * s) / 2.0;
            overlay_ta.push(TextSpec {
                buffer_index: BUF_DD_KEY_HINT,
                left: self.dd_item_key_rect.x + DROPDOWN_ITEM_PAD_H * s + hint_offset * s,
                top: hint_text_y,
                bounds: self.dd_item_key_rect,
                color: hex_to_glyphon_color(&colors.text_dim),
            });
        }

        SshDialogDrawCommands {
            rounded_quads: rq,
            text_areas: ta,
            overlay_rounded_quads: overlay_rq,
            overlay_text_areas: overlay_ta,
        }
    }

    fn draw_input_field(
        &self,
        rq: &mut Vec<RoundedQuad>,
        ta: &mut Vec<TextSpec>,
        rect: &Rect,
        buf_idx: usize,
        focused: bool,
        field: DialogField,
        s: f32,
        is_placeholder: bool,
        colors: &crate::colors::ColorScheme,
    ) {
        let fr = FIELD_RADIUS * s;
        let border_w = if focused { 2.0 * s } else { 1.0 * s };
        let border_col = if focused {
            &colors.field_focused
        } else {
            &colors.field_border
        };

        push_stroked_rounded_rect(
            rq,
            rect,
            hex_to_linear_f32(border_col),
            hex_to_linear_f32(&colors.background),
            fr,
            border_w,
        );

        let pad = FIELD_PAD_H * s;
        let line_h = FONT_SIZE * LINE_HEIGHT_MULT;
        let text_y = rect.y + (rect.height - line_h * s) / 2.0;
        ta.push(TextSpec {
            buffer_index: buf_idx,
            left: rect.x + pad,
            top: text_y,
            bounds: *rect,
            color: if is_placeholder {
                hex_to_glyphon_color(&colors.text_placeholder)
            } else {
                hex_to_glyphon_color(&colors.dropdown_text)
            },
        });

        if focused {
            let ci = field.cursor_idx();
            let cursor_x = rect.x + pad + self.cursor_pos[ci] as f32 * self.char_width * s;
            let cursor_h = line_h * s;
            let cursor_y = rect.y + (rect.height - cursor_h) / 2.0;
            rq.push(RoundedQuad {
                rect: Rect {
                    x: cursor_x,
                    y: cursor_y,
                    width: 1.5 * s,
                    height: cursor_h,
                },
                color: hex_to_linear_f32(&colors.cursor),
                radius: 0.0,
                shadow_softness: 0.0,
            });
        }
    }
}

fn draw_dropdown_item(
    rq: &mut Vec<RoundedQuad>,
    ta: &mut Vec<TextSpec>,
    rect: Rect,
    buf_idx: usize,
    selected: bool,
    hovered: bool,
    radius: f32,
    s: f32,
    line_h: f32,
    text_color: GlyphonColor,
    colors: &crate::colors::ColorScheme,
) {
    if selected {
        rq.push(RoundedQuad {
            rect,
            color: hex_to_linear_f32(&colors.dropdown_item_hover),
            radius,
            shadow_softness: 0.0,
        });
    } else if hovered {
        rq.push(RoundedQuad {
            rect,
            color: hex_to_linear_f32(&colors.tab_hover_bg),
            radius,
            shadow_softness: 0.0,
        });
    }
    let text_y = rect.y + (rect.height - line_h * s) / 2.0;
    ta.push(TextSpec {
        buffer_index: buf_idx,
        left: rect.x + DROPDOWN_ITEM_PAD_H * s,
        top: text_y,
        bounds: rect,
        color: text_color,
    });
}

/// Separate OS window for the SSH session dialog with its own GPU context.
pub struct SshDialogWindow {
    window: Arc<Window>,
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    render_format: TextureFormat,

    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_text_renderer: TextRenderer,
    viewport: Viewport,
    _cache: Cache,

    rounded_rect: RoundedRectPipeline,
    colors: crate::colors::ColorScheme,

    dialog: SshDialog,
    cursor_position: (f32, f32),
    super_pressed: bool,
}

impl SshDialogWindow {
    pub fn open(event_loop: &ActiveEventLoop) -> Self {
        // Use a reasonable initial height (Agent mode, smallest)
        let initial_h = 280.0; // will resize after layout

        let attrs = WindowAttributes::default()
            .with_title("SSH Session")
            .with_inner_size(winit::dpi::LogicalSize::new(DIALOG_WIDTH, initial_h))
            .with_resizable(false);

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create SSH dialog window"),
        );
        let actual_scale = window.scale_factor() as f32;
        let size = window.inner_size();

        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            compatible_surface: Some(&surface),
            power_preference: PowerPreference::LowPower,
            ..Default::default()
        }))
        .expect("no adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &DeviceDescriptor {
                label: Some("ssh dialog device"),
                ..Default::default()
            },
            None,
        ))
        .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        let render_format = pick_srgb_format(surface_format);

        let view_formats = if render_format != surface_format {
            vec![render_format]
        } else {
            vec![]
        };

        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats,
        };
        surface.configure(&device, &surface_config);

        // glyphon
        let mut font_system = font::create_font_system();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, render_format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let overlay_text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let rounded_rect = RoundedRectPipeline::new(&device, render_format, MAX_ROUNDED_RECTS);
        let colors = crate::colors::ColorScheme::load();

        let dialog = SshDialog::new(actual_scale, &mut font_system);

        // Resize window to match actual dialog height
        let dialog_h = dialog.compute_dialog_height();
        let _ = window.request_inner_size(winit::dpi::LogicalSize::new(DIALOG_WIDTH, dialog_h));

        Self {
            window,
            device,
            queue,
            surface,
            surface_config,
            render_format,
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            overlay_text_renderer,
            viewport,
            _cache: cache,
            rounded_rect,
            colors,
            dialog,
            cursor_position: (0.0, 0.0),
            super_pressed: false,
        }
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    fn resize_to_fit(&mut self) {
        let dialog_h = self.dialog.compute_dialog_height();
        let _ = self
            .window
            .request_inner_size(winit::dpi::LogicalSize::new(DIALOG_WIDTH, dialog_h));
    }

    /// Handle a window event. Returns Some(SshResult) on submit, or None.
    /// Returns Err(()) if the window should be closed (cancel/escape/close button).
    pub fn handle_event(&mut self, event: WindowEvent) -> Result<Option<SshResult>, ()> {
        match event {
            WindowEvent::CloseRequested => Err(()),

            WindowEvent::RedrawRequested => {
                self.render();
                Ok(None)
            }

            WindowEvent::Resized(new_size) => {
                if new_size.width > 0 && new_size.height > 0 {
                    self.surface_config.width = new_size.width;
                    self.surface_config.height = new_size.height;
                    self.surface.configure(&self.device, &self.surface_config);
                }
                self.request_redraw();
                Ok(None)
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dialog
                    .compute_layout(scale_factor as f32, &mut self.font_system);
                self.resize_to_fit();
                self.request_redraw();
                Ok(None)
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = (position.x as f32, position.y as f32);
                let (cx, cy) = self.cursor_position;
                let hover = self.dialog.compute_hover(cx, cy);
                let dd_hover = self.dialog.compute_dd_hover(cx, cy);
                let mut changed = self.dialog.set_hover(hover);
                if self.dialog.dd_hover_item != dd_hover {
                    self.dialog.dd_hover_item = dd_hover;
                    changed = true;
                }
                if changed {
                    self.request_redraw();
                }
                Ok(None)
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (cx, cy) = self.cursor_position;
                let hit = self.dialog.hit_test(cx, cy);

                // Check for browse button before delegating
                let is_browse = matches!(hit, DialogHit::BrowseButton);
                let is_cancel = matches!(hit, DialogHit::CancelButton);

                if is_cancel {
                    return Err(());
                }

                if is_browse {
                    self.open_file_picker();
                    self.request_redraw();
                    return Ok(None);
                }

                if let Some(result) = self.dialog.handle_click(cx, cy, &mut self.font_system) {
                    if !result.host.is_empty() {
                        return Ok(Some(result));
                    }
                }

                self.request_redraw();
                Ok(None)
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.super_pressed = new_modifiers.state().super_key();
                Ok(None)
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Key::Named(NamedKey::Escape) = event.logical_key.as_ref() {
                        if self.dialog.dropdown_open {
                            self.dialog.dropdown_open = false;
                            self.request_redraw();
                            return Ok(None);
                        }
                        return Err(());
                    }
                    // Cmd+V paste
                    if self.super_pressed {
                        if let Key::Character(c) = event.logical_key.as_ref() {
                            if c == "v" {
                                if let Ok(mut clip) = arboard::Clipboard::new() {
                                    if let Ok(text) = clip.get_text() {
                                        self.dialog.insert_text(&text, &mut self.font_system);
                                        self.request_redraw();
                                        return Ok(None);
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(result) = self.dialog.handle_key(&event, &mut self.font_system) {
                    if !result.host.is_empty() {
                        return Ok(Some(result));
                    }
                }
                self.request_redraw();
                Ok(None)
            }

            _ => Ok(None),
        }
    }

    fn open_file_picker(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Select Private Key File");

        // Start in ~/.ssh if it exists
        if let Some(home) = dirs::home_dir() {
            let ssh_dir = home.join(".ssh");
            if ssh_dir.exists() {
                dialog = dialog.set_directory(&ssh_dir);
            } else {
                dialog = dialog.set_directory(&home);
            }
        }

        if let Some(path) = dialog.pick_file() {
            let path_str = path.to_string_lossy().to_string();
            // Shorten to ~/... if possible
            let display = if let Some(home) = dirs::home_dir() {
                if let Ok(rest) = path.strip_prefix(&home) {
                    format!("~/{}", rest.display())
                } else {
                    path_str
                }
            } else {
                path_str
            };
            self.dialog.set_key_path(display, &mut self.font_system);
        }
    }

    fn render(&mut self) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                let w = self.surface_config.width;
                let h = self.surface_config.height;
                self.surface_config.width = w;
                self.surface_config.height = h;
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            Err(e) => {
                log::error!("ssh dialog render error: {e}");
                return;
            }
        };

        let view = frame.texture.create_view(&TextureViewDescriptor {
            format: Some(self.render_format),
            ..Default::default()
        });

        let scale = self.window.scale_factor() as f32;
        let draw = self.dialog.draw_commands(scale, &self.colors);

        // Upload rounded rect uniforms: base layer first, then overlay layer
        let base_rr_count = self
            .rounded_rect
            .upload_quads(&self.queue, &draw.rounded_quads, 0);
        let total_rr_count =
            self.rounded_rect
                .upload_quads(&self.queue, &draw.overlay_rounded_quads, base_rr_count);

        // Build text areas
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        let bufs = &self.dialog.buffers;

        let mut text_areas: Vec<TextArea> = Vec::new();
        push_text_specs(&mut text_areas, &draw.text_areas, bufs, scale);

        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            log::error!("ssh dialog text prepare error: {e}");
            return;
        }

        let mut overlay_areas: Vec<TextArea> = Vec::new();
        push_text_specs(&mut overlay_areas, &draw.overlay_text_areas, bufs, scale);

        if let Err(e) = self.overlay_text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            overlay_areas,
            &mut self.swash_cache,
        ) {
            log::error!("ssh dialog overlay text prepare error: {e}");
            return;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("ssh dialog encoder"),
            });

        {
            let bg = hex_to_linear_f32(&self.colors.background);
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ssh dialog pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: bg[3] as f64,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            // === Base layer ===
            self.rounded_rect.draw_range(&mut pass, 0, base_rr_count);

            // Base text (labels, field values)
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render ssh dialog text");

            // === Overlay layer (dropdown popup) ===
            self.rounded_rect
                .draw_range(&mut pass, base_rr_count, total_rr_count - base_rr_count);

            // Overlay text (dropdown menu items)
            self.overlay_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render ssh dialog overlay text");
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.atlas.trim();
    }
}
