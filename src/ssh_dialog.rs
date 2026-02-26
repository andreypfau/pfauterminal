use std::sync::Arc;

use glyphon::{FontSystem, Metrics, TextArea};
use wgpu::*;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::colors::ColorScheme;
use crate::draw::DrawContext;
use crate::dropdown::{DropdownElement, DropdownMenu, MenuAction, MenuItem};
use crate::layout::Rect;
use crate::theme::{DialogTheme, Theme};
use crate::widgets::{Button, ButtonKind, Label, TextField};

use crate::font::LINE_HEIGHT as LINE_HEIGHT_MULT;

const DEFAULT_KEY_PATH: &str = "~/.ssh/id_ed25519";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedField {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
}

#[derive(Debug)]
enum DialogHit {
    Field(FocusedField),
    AuthDropdown,
    DropdownItem(usize),
    BrowseButton,
    CancelButton,
    OkButton,
    Inside,
    Outside,
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

    fn from_index(idx: usize) -> Option<AuthMethod> {
        match idx {
            0 => Some(AuthMethod::Password),
            1 => Some(AuthMethod::Key),
            2 => Some(AuthMethod::Agent),
            _ => None,
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

fn auth_menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem {
            label: AuthMethod::Password.display_text().to_string(),
            action: MenuAction::NewShell(String::new()),
        },
        MenuItem {
            label: AuthMethod::Key.display_text().to_string(),
            action: MenuAction::NewShell(String::new()),
        },
        MenuItem {
            label: AuthMethod::Agent.display_text().to_string(),
            action: MenuAction::NewShell(String::new()),
        },
    ]
}

/// Reusable context for positioning label+field rows in the dialog layout.
struct FormLayout {
    form_x: f32,
    label_w: f32,
    field_gap: f32,
    field_h: f32,
    line_h_scaled: f32,
    bounds: Rect,
    label_color: glyphon::Color,
}

impl FormLayout {
    fn row(&self, label: &mut Label, field: &mut TextField, row_y: f32, field_w: f32) {
        self.label(label, self.form_x, row_y);
        field.set_rect(Rect {
            x: self.form_x + self.label_w + self.field_gap,
            y: row_y,
            width: field_w,
            height: self.field_h,
        });
    }

    fn label(&self, label: &mut Label, x: f32, row_y: f32) {
        label.set_position(x, row_y + (self.field_h - self.line_h_scaled) / 2.0, self.bounds);
        label.set_color(self.label_color);
    }
}

/// Dialog state: widgets, focus, layout rects, text buffers.
struct SshDialog {
    // Labels
    title: Label,
    host_label: Label,
    port_label: Label,
    username_label: Label,
    auth_label: Label,
    password_label: Label,
    keypath_label: Label,
    passphrase_label: Label,
    auth_value_label: Label,

    // Text fields
    host_field: TextField,
    port_field: TextField,
    username_field: TextField,
    password_field: TextField,
    keypath_field: TextField,
    passphrase_field: TextField,

    // Buttons
    cancel_button: Button,
    ok_button: Button,

    // Non-widget state
    auth_method: AuthMethod,
    focused_field: FocusedField,
    auth_dropdown_rect: Rect,
    browse_btn_rect: Rect,
    hover: DialogHit,
    auth_dropdown: DropdownMenu,
    scale: f32,
    dialog_theme: DialogTheme,
    label_color: glyphon::Color,
}

impl SshDialog {
    fn new(scale: f32, theme: &Theme, font_system: &mut FontSystem) -> Self {
        let t = &theme.dialog;
        let colors = &theme.colors;
        let metrics = Metrics::new(t.font_size, t.font_size * LINE_HEIGHT_MULT);
        let attrs = crate::font::default_attrs();
        let semibold_attrs = attrs.weight(glyphon::Weight::SEMIBOLD);
        let char_width = crate::font::measure_cell(font_system).width;
        let label_color = colors.dropdown_text.to_glyphon();

        let title = Label::new("SSH Session", semibold_attrs, metrics, font_system);
        let host_label = Label::new("Host:", attrs, metrics, font_system);
        let port_label = Label::new("Port:", attrs, metrics, font_system);
        let username_label = Label::new("Username:", attrs, metrics, font_system);
        let auth_label = Label::new("Authentication type:", attrs, metrics, font_system);
        let password_label = Label::new("Password:", attrs, metrics, font_system);
        let keypath_label = Label::new("Private key file:", semibold_attrs, metrics, font_system);
        let passphrase_label = Label::new("Passphrase:", attrs, metrics, font_system);
        let auth_value_label = Label::new(
            AuthMethod::Agent.display_text(),
            attrs,
            metrics,
            font_system,
        );

        let fr = t.field_radius;
        let fp = t.field_pad_h;
        let mut host_field = TextField::new("", false, metrics, char_width, fr, fp, font_system);
        let mut port_field = TextField::new("", false, metrics, char_width, fr, fp, font_system);
        port_field.set_value("22", font_system);
        let username_field = TextField::new("", false, metrics, char_width, fr, fp, font_system);
        let password_field = TextField::new("", true, metrics, char_width, fr, fp, font_system);
        let keypath_field =
            TextField::new(DEFAULT_KEY_PATH, false, metrics, char_width, fr, fp, font_system);
        let passphrase_field = TextField::new("", true, metrics, char_width, fr, fp, font_system);

        let cancel_button = Button::new(
            "Cancel",
            ButtonKind::Stroked {
                fill: colors.tab_hover_bg,
                fill_hover: colors.tab_hover_stroke,
                stroke: colors.tab_hover_stroke,
            },
            colors.dropdown_text.to_glyphon(),
            t.button_radius,
            t.cancel_pad_h,
            attrs,
            metrics,
            font_system,
        );

        let ok_button = Button::new(
            "OK",
            ButtonKind::Filled {
                bg: colors.ok_bg,
                bg_hover: colors.ok_hover_bg,
            },
            colors.dropdown_text_active.to_glyphon(),
            t.button_radius,
            t.ok_pad_h,
            semibold_attrs,
            metrics,
            font_system,
        );

        host_field.set_focused(true);

        let mut dialog = Self {
            title,
            host_label,
            port_label,
            username_label,
            auth_label,
            password_label,
            keypath_label,
            passphrase_label,
            auth_value_label,
            host_field,
            port_field,
            username_field,
            password_field,
            keypath_field,
            passphrase_field,
            cancel_button,
            ok_button,
            auth_method: AuthMethod::Agent,
            focused_field: FocusedField::Host,
            auth_dropdown_rect: Rect::ZERO,
            browse_btn_rect: Rect::ZERO,
            hover: DialogHit::Inside,
            auth_dropdown: DropdownMenu::new(),
            scale,
            dialog_theme: t.clone(),
            label_color,
        };
        dialog.compute_layout(scale, font_system);
        dialog
    }

    fn compute_layout(&mut self, scale: f32, font_system: &mut FontSystem) {
        self.scale = scale;
        let s = scale;
        let t = &self.dialog_theme;

        let char_width = crate::font::measure_cell(font_system).width;
        for field in [
            &mut self.host_field,
            &mut self.port_field,
            &mut self.username_field,
            &mut self.password_field,
            &mut self.keypath_field,
            &mut self.passphrase_field,
        ] {
            field.set_char_width(char_width);
        }

        let line_h = t.font_size * LINE_HEIGHT_MULT;
        let form_x = t.form_pad_h * s;
        let form_w = t.width * s - 2.0 * form_x;
        let field_h = t.field_height * s;
        let field_gap = t.field_gap * s;
        let label_w = t.label_width * s;
        let port_section_w = t.port_spacer_width * s;

        let dialog_w = t.width * s;
        let dialog_h = self.compute_dialog_height() * s;
        let dialog_rect = Rect { x: 0.0, y: 0.0, width: dialog_w, height: dialog_h };
        let label_color = self.label_color;

        let fl = FormLayout {
            form_x,
            label_w,
            field_gap,
            field_h,
            line_h_scaled: line_h * s,
            bounds: dialog_rect,
            label_color,
        };

        // Title
        let title_y = (t.title_bar_height * s - fl.line_h_scaled) / 2.0;
        self.title.set_position(form_x, title_y, dialog_rect);
        self.title.set_color(label_color);

        let mut row_y = t.title_bar_height * s + t.form_pad_v * s;
        let row_step = field_h + t.form_row_gap * s;

        // Host + Port row
        let host_input_w = form_w - label_w - field_gap - port_section_w;
        fl.row(&mut self.host_label, &mut self.host_field, row_y, host_input_w);
        let port_label_x = form_x + label_w + field_gap + host_input_w + field_gap;
        fl.label(&mut self.port_label, port_label_x, row_y);
        self.port_field.set_rect(Rect {
            x: form_x + form_w - t.port_field_width * s,
            y: row_y,
            width: t.port_field_width * s,
            height: field_h,
        });
        row_y += row_step;

        // Username row
        let username_input_w = form_w - label_w - field_gap - port_section_w;
        fl.row(&mut self.username_label, &mut self.username_field, row_y, username_input_w);
        row_y += row_step;

        // Auth dropdown row
        let auth_x = form_x + label_w + field_gap;
        let auth_w = form_w - label_w - field_gap;
        fl.label(&mut self.auth_label, form_x, row_y);
        self.auth_dropdown_rect = Rect { x: auth_x, y: row_y, width: auth_w, height: field_h };
        let auth_pad = t.field_pad_h * s;
        self.auth_value_label.set_position(
            auth_x + auth_pad,
            row_y + (field_h - fl.line_h_scaled) / 2.0,
            self.auth_dropdown_rect,
        );
        self.auth_value_label.set_color(label_color);
        row_y += row_step;

        // Credential rows
        let cred_input_w = form_w - label_w - field_gap;

        // Password field
        fl.row(&mut self.password_label, &mut self.password_field, row_y, cred_input_w);

        // Key path field (narrower for browse button)
        let browse_w = t.browse_btn_size * s;
        fl.label(&mut self.keypath_label, form_x, row_y);
        self.keypath_field.set_rect(Rect {
            x: form_x + label_w + field_gap,
            y: row_y,
            width: cred_input_w - browse_w - field_gap,
            height: field_h,
        });
        self.browse_btn_rect = Rect {
            x: form_x + label_w + field_gap + cred_input_w - browse_w,
            y: row_y,
            width: browse_w,
            height: field_h,
        };

        // Passphrase field
        let passphrase_y = row_y + row_step;
        fl.row(&mut self.passphrase_label, &mut self.passphrase_field, passphrase_y, cred_input_w);

        // Footer buttons
        let button_h = t.cancel_pad_v * 2.0 + line_h;
        let footer_y = (self.compute_dialog_height() - t.footer_pad_v - button_h) * s;
        let cancel_w = (t.cancel_pad_h * 2.0 + char_width * 6.0) * s;
        let ok_w = (t.ok_pad_h * 2.0 + char_width * 2.0) * s;
        let btn_right = dialog_w - t.footer_pad_h * s;
        self.ok_button.set_rect(Rect {
            x: btn_right - ok_w,
            y: footer_y,
            width: ok_w,
            height: button_h * s,
        });
        self.cancel_button.set_rect(Rect {
            x: btn_right - ok_w - t.footer_gap * s - cancel_w,
            y: footer_y,
            width: cancel_w,
            height: button_h * s,
        });
    }

    fn compute_dialog_height(&self) -> f32 {
        let t = &self.dialog_theme;
        let line_h = t.font_size * LINE_HEIGHT_MULT;
        let form_content_h = t.field_height
            + t.form_row_gap
            + t.field_height
            + t.form_row_gap
            + t.field_height
            + t.form_row_gap
            + t.field_height
            + t.form_row_gap
            + t.field_height;
        let form_h = t.form_pad_v + form_content_h + t.form_pad_v;
        let button_h = t.cancel_pad_v * 2.0 + line_h;
        let footer_h = t.footer_pad_v + button_h + t.footer_pad_v;
        t.title_bar_height + form_h + footer_h
    }

    fn focused_field_widget_mut(&mut self) -> &mut TextField {
        match self.focused_field {
            FocusedField::Host => &mut self.host_field,
            FocusedField::Port => &mut self.port_field,
            FocusedField::Username => &mut self.username_field,
            FocusedField::Password => &mut self.password_field,
            FocusedField::KeyPath => &mut self.keypath_field,
            FocusedField::Passphrase => &mut self.passphrase_field,
        }
    }

    fn set_focus(&mut self, field: FocusedField) {
        // Unfocus old
        self.focused_field_widget_mut().set_focused(false);
        self.focused_field = field;
        // Focus new
        self.focused_field_widget_mut().set_focused(true);
    }

    fn set_hover(&mut self, hit: DialogHit) {
        self.cancel_button
            .set_hovered(matches!(hit, DialogHit::CancelButton));
        self.ok_button
            .set_hovered(matches!(hit, DialogHit::OkButton));
        self.hover = hit;
    }

    fn hit_test(&self, x: f32, y: f32) -> DialogHit {
        if self.auth_dropdown.is_open() {
            if let DropdownElement::Item(idx) = self.auth_dropdown.hit_test(x, y) {
                return DialogHit::DropdownItem(idx);
            }
            return if self.auth_dropdown.is_outside(x, y) {
                DialogHit::Outside
            } else {
                DialogHit::Inside
            };
        }

        if self.host_field.contains(x, y) {
            return DialogHit::Field(FocusedField::Host);
        }
        if self.port_field.contains(x, y) {
            return DialogHit::Field(FocusedField::Port);
        }
        if self.username_field.contains(x, y) {
            return DialogHit::Field(FocusedField::Username);
        }
        if self.auth_dropdown_rect.contains(x, y) {
            return DialogHit::AuthDropdown;
        }
        match self.auth_method {
            AuthMethod::Password => {
                if self.password_field.contains(x, y) {
                    return DialogHit::Field(FocusedField::Password);
                }
            }
            AuthMethod::Key => {
                if self.keypath_field.contains(x, y) {
                    return DialogHit::Field(FocusedField::KeyPath);
                }
                if self.browse_btn_rect.contains(x, y) {
                    return DialogHit::BrowseButton;
                }
                if self.passphrase_field.contains(x, y) {
                    return DialogHit::Field(FocusedField::Passphrase);
                }
            }
            AuthMethod::Agent => {}
        }
        if self.cancel_button.contains(x, y) {
            return DialogHit::CancelButton;
        }
        if self.ok_button.contains(x, y) {
            return DialogHit::OkButton;
        }
        DialogHit::Inside
    }

    fn handle_click(
        &mut self,
        hit: DialogHit,
        x: f32,
        font_system: &mut FontSystem,
        dropdown_theme: &crate::theme::DropdownTheme,
    ) -> Option<SshResult> {
        match hit {
            DialogHit::Field(field) => {
                self.auth_dropdown.close();
                self.set_focus(field);
                let scale = self.scale;
                self.focused_field_widget_mut().click(x, scale);
                None
            }
            DialogHit::AuthDropdown => {
                if self.auth_dropdown.is_open() {
                    self.auth_dropdown.close();
                } else {
                    self.open_auth_dropdown(font_system, dropdown_theme);
                }
                None
            }
            DialogHit::DropdownItem(idx) => {
                if let Some(method) = AuthMethod::from_index(idx) {
                    let old_method = self.auth_method;
                    self.auth_method = method;
                    self.auth_dropdown.close();
                    if old_method != method {
                        let t = &self.dialog_theme;
                        let metrics =
                            Metrics::new(t.font_size, t.font_size * LINE_HEIGHT_MULT);
                        let attrs = crate::font::default_attrs();
                        self.auth_value_label = Label::new(
                            method.display_text(),
                            attrs,
                            metrics,
                            font_system,
                        );
                        // Reposition label
                        let dd = &self.auth_dropdown_rect;
                        let line_h = t.font_size * LINE_HEIGHT_MULT;
                        let text_y = dd.y + (dd.height - line_h * self.scale) / 2.0;
                        let pad = t.field_pad_h * self.scale;
                        self.auth_value_label
                            .set_position(dd.x + pad, text_y, *dd);
                        self.auth_value_label
                            .set_color(self.label_color);
                        match method {
                            AuthMethod::Password => self.set_focus(FocusedField::Password),
                            AuthMethod::Key => self.set_focus(FocusedField::KeyPath),
                            AuthMethod::Agent => {}
                        }
                    }
                }
                None
            }
            DialogHit::BrowseButton => None,
            DialogHit::Outside => {
                self.auth_dropdown.close();
                None
            }
            DialogHit::OkButton => Some(self.build_result()),
            DialogHit::CancelButton => None,
            DialogHit::Inside => {
                self.auth_dropdown.close();
                None
            }
        }
    }

    fn open_auth_dropdown(
        &mut self,
        font_system: &mut FontSystem,
        dropdown_theme: &crate::theme::DropdownTheme,
    ) {
        let dialog_w = self.dialog_theme.width * self.scale;
        let dialog_h = self.compute_dialog_height() * self.scale;
        let auth_logical_w = self.auth_dropdown_rect.width / self.scale;
        self.auth_dropdown.open(
            auth_menu_items(),
            self.auth_dropdown_rect,
            Some(auth_logical_w),
            self.scale,
            dialog_w,
            dialog_h,
            font_system,
            dropdown_theme,
        );
    }

    fn handle_key(
        &mut self,
        event: &KeyEvent,
        font_system: &mut FontSystem,
    ) -> Option<SshResult> {
        if event.state != ElementState::Pressed {
            return None;
        }

        if self.auth_dropdown.is_open() {
            self.auth_dropdown.close();
            return None;
        }

        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Enter) => {
                return Some(self.build_result());
            }
            Key::Named(NamedKey::Tab) => {
                let next = match (self.focused_field, self.auth_method) {
                    (FocusedField::Username, AuthMethod::Password) => FocusedField::Password,
                    (FocusedField::Username, AuthMethod::Key) => FocusedField::KeyPath,
                    (FocusedField::Username, AuthMethod::Agent) => FocusedField::Host,
                    (FocusedField::Password, _) => FocusedField::Host,
                    (FocusedField::KeyPath, _) => FocusedField::Passphrase,
                    (FocusedField::Passphrase, _) => FocusedField::Host,
                    (FocusedField::Host, _) => FocusedField::Port,
                    (FocusedField::Port, _) => FocusedField::Username,
                };
                self.set_focus(next);
                return None;
            }
            Key::Named(NamedKey::Backspace) => {
                self.focused_field_widget_mut().delete_back(font_system);
                return None;
            }
            Key::Named(NamedKey::Delete) => {
                self.focused_field_widget_mut()
                    .delete_forward(font_system);
                return None;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.focused_field_widget_mut().move_left();
                return None;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.focused_field_widget_mut().move_right();
                return None;
            }
            Key::Named(NamedKey::Home) => {
                self.focused_field_widget_mut().move_home();
                return None;
            }
            Key::Named(NamedKey::End) => {
                self.focused_field_widget_mut().move_end();
                return None;
            }
            _ => {}
        }

        if let Some(text) = &event.text {
            self.insert_text(&text.to_string(), font_system);
        }

        None
    }

    fn insert_text(&mut self, text: &str, font_system: &mut FontSystem) {
        self.focused_field_widget_mut()
            .insert_text(text, font_system);
    }

    fn set_key_path(&mut self, path: String, font_system: &mut FontSystem) {
        self.keypath_field.set_value(&path, font_system);
    }

    fn build_result(&self) -> SshResult {
        SshResult {
            host: self.host_field.value().to_string(),
            port: self.port_field.value().to_string(),
            username: self.username_field.value().to_string(),
            auth_method: self.auth_method,
            password: self.password_field.value().to_string(),
            key_path: if self.keypath_field.value().is_empty() {
                DEFAULT_KEY_PATH.to_string()
            } else {
                self.keypath_field.value().to_string()
            },
            passphrase: self.passphrase_field.value().to_string(),
        }
    }

    fn cursor_for_hit(hit: &DialogHit) -> winit::window::CursorIcon {
        match hit {
            DialogHit::CancelButton
            | DialogHit::OkButton
            | DialogHit::AuthDropdown
            | DialogHit::BrowseButton => winit::window::CursorIcon::Pointer,
            DialogHit::Field(_) => winit::window::CursorIcon::Text,
            _ => winit::window::CursorIcon::Default,
        }
    }

    fn draw<'a>(
        &'a self,
        ctx: &mut DrawContext,
        text_areas: &mut Vec<TextArea<'a>>,
        scale: f32,
        colors: &ColorScheme,
    ) {
        let s = scale;
        let t = &self.dialog_theme;
        let border = t.border_width * s;
        let field_pad = t.field_pad_h * s;
        let dialog_w = t.width * s;

        // Title
        self.title.draw(text_areas, s);
        ctx.rounded_rect(
            Rect {
                x: 0.0,
                y: t.title_bar_height * s - border,
                width: dialog_w,
                height: border,
            },
            colors.dropdown_border.to_linear_f32(),
            0.0,
        );

        // Host row
        self.host_label.draw(text_areas, s);
        self.host_field.draw(ctx, text_areas, s, colors);
        self.port_label.draw(text_areas, s);
        self.port_field.draw(ctx, text_areas, s, colors);

        // Username row
        self.username_label.draw(text_areas, s);
        self.username_field.draw(ctx, text_areas, s, colors);

        // Auth dropdown row
        self.auth_label.draw(text_areas, s);
        let dd = &self.auth_dropdown_rect;
        ctx.stroked_rect(
            dd,
            colors.text_dim.to_linear_f32(),
            colors.tab_hover_bg.to_linear_f32(),
            t.field_radius * s,
            1.0 * s,
        );
        self.auth_value_label.draw(text_areas, s);
        draw_chevron(ctx, dd, field_pad, s, colors);

        // Credential rows
        match self.auth_method {
            AuthMethod::Password => {
                self.password_label.draw(text_areas, s);
                self.password_field.draw(ctx, text_areas, s, colors);
            }
            AuthMethod::Key => {
                self.keypath_label.draw(text_areas, s);
                self.keypath_field.draw(ctx, text_areas, s, colors);

                // Browse button
                let br = &self.browse_btn_rect;
                let is_browse_hover = matches!(self.hover, DialogHit::BrowseButton);
                ctx.stroked_rect(
                    br,
                    colors.field_border.to_linear_f32(),
                    if is_browse_hover {
                        colors.tab_hover_stroke.to_linear_f32()
                    } else {
                        colors.tab_hover_bg.to_linear_f32()
                    },
                    t.field_radius * s,
                    1.0 * s,
                );
                draw_folder_icon(ctx, br, s, colors);

                self.passphrase_label.draw(text_areas, s);
                self.passphrase_field.draw(ctx, text_areas, s, colors);
            }
            AuthMethod::Agent => {}
        }

        // Footer buttons
        self.cancel_button.draw(ctx, text_areas, s);
        self.ok_button.draw(ctx, text_areas, s);
    }
}

fn draw_chevron(
    ctx: &mut DrawContext,
    rect: &Rect,
    field_pad: f32,
    s: f32,
    colors: &ColorScheme,
) {
    let size = 14.0 * s;
    let x = rect.x + rect.width - field_pad - size;
    let cy = rect.y + rect.height / 2.0;
    let half = size / 2.0;
    let thick = 1.5 * s;
    let col = colors.text_placeholder.to_linear_f32();
    for i in 0..4_usize {
        let frac = i as f32 / 3.0;
        let bar_y = cy - half * 0.4 + half * 0.8 * frac;
        let indent = half * (1.0 - frac);
        ctx.rounded_rect(
            Rect {
                x: x + indent,
                y: bar_y,
                width: size - 2.0 * indent,
                height: thick,
            },
            col,
            0.0,
        );
    }
}

fn draw_folder_icon(
    ctx: &mut DrawContext,
    btn_rect: &Rect,
    s: f32,
    colors: &ColorScheme,
) {
    let icon_s = 16.0 * s;
    let ix = btn_rect.x + (btn_rect.width - icon_s) / 2.0;
    let iy = btn_rect.y + (btn_rect.height - icon_s) / 2.0;
    let folder_col = colors.text_placeholder.to_linear_f32();
    ctx.rounded_rect(
        Rect {
            x: ix,
            y: iy + icon_s * 0.25,
            width: icon_s,
            height: icon_s * 0.65,
        },
        folder_col,
        2.0 * s,
    );
    ctx.rounded_rect(
        Rect {
            x: ix + 1.5 * s,
            y: iy + icon_s * 0.25 + 1.5 * s,
            width: icon_s - 3.0 * s,
            height: icon_s * 0.65 - 3.0 * s,
        },
        colors.tab_hover_bg.to_linear_f32(),
        1.0 * s,
    );
    ctx.rounded_rect(
        Rect {
            x: ix,
            y: iy + icon_s * 0.12,
            width: icon_s * 0.45,
            height: icon_s * 0.2,
        },
        folder_col,
        1.5 * s,
    );
}

/// Separate OS window for the SSH session dialog with its own GPU context.
pub struct SshDialogWindow {
    window: Arc<Window>,
    gpu: crate::gpu::GpuSimple,
    dialog: SshDialog,
    theme: Theme,
    cursor_position: (f32, f32),
    super_pressed: bool,
}

impl SshDialogWindow {
    pub fn open(event_loop: &ActiveEventLoop, theme: &Theme) -> Self {
        let theme = theme.clone();
        let dialog_width = theme.dialog.width;
        let initial_h = 280.0;

        let attrs = WindowAttributes::default()
            .with_title("SSH Session")
            .with_inner_size(winit::dpi::LogicalSize::new(dialog_width, initial_h))
            .with_resizable(false);

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create SSH dialog window"),
        );
        let actual_scale = window.scale_factor() as f32;

        let mut gpu =
            crate::gpu::GpuSimple::new(window.clone(), theme.colors.clone(), theme.dialog.max_rounded_rects);

        let dialog = SshDialog::new(actual_scale, &theme, &mut gpu.font_system);

        let dialog_h = dialog.compute_dialog_height();
        let _ =
            window.request_inner_size(winit::dpi::LogicalSize::new(dialog_width, dialog_h));

        Self {
            window,
            gpu,
            dialog,
            theme,
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
        let dialog_w = self.dialog.dialog_theme.width;
        let _ = self
            .window
            .request_inner_size(winit::dpi::LogicalSize::new(dialog_w, dialog_h));
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
                self.gpu.resize(new_size.width, new_size.height);
                self.request_redraw();
                Ok(None)
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dialog
                    .compute_layout(scale_factor as f32, &mut self.gpu.font_system);
                self.resize_to_fit();
                self.request_redraw();
                Ok(None)
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = (position.x as f32, position.y as f32);
                let (cx, cy) = self.cursor_position;

                if self.dialog.auth_dropdown.is_open() {
                    let dd_hover = self.dialog.auth_dropdown.hit_test(cx, cy);
                    if self.dialog.auth_dropdown.set_hover(dd_hover) {
                        self.request_redraw();
                    }
                }

                let hit = self.dialog.hit_test(cx, cy);
                self.window.set_cursor(SshDialog::cursor_for_hit(&hit));
                self.dialog.set_hover(hit);
                self.request_redraw();
                Ok(None)
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (cx, cy) = self.cursor_position;
                let hit = self.dialog.hit_test(cx, cy);

                if matches!(hit, DialogHit::CancelButton) {
                    return Err(());
                }

                if matches!(hit, DialogHit::BrowseButton) {
                    self.open_file_picker();
                    self.request_redraw();
                    return Ok(None);
                }

                if let Some(result) =
                    self.dialog
                        .handle_click(hit, cx, &mut self.gpu.font_system, &self.theme.dropdown)
                    && !result.host.is_empty()
                {
                    return Ok(Some(result));
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
                        if self.dialog.auth_dropdown.is_open() {
                            self.dialog.auth_dropdown.close();
                            self.request_redraw();
                            return Ok(None);
                        }
                        return Err(());
                    }
                    if self.super_pressed
                        && let Key::Character(c) = event.logical_key.as_ref()
                        && c == "v"
                        && let Ok(mut clip) = arboard::Clipboard::new()
                        && let Ok(text) = clip.get_text()
                    {
                        self.dialog
                            .insert_text(&text, &mut self.gpu.font_system);
                        self.request_redraw();
                        return Ok(None);
                    }
                }
                if let Some(result) =
                    self.dialog
                        .handle_key(&event, &mut self.gpu.font_system)
                    && !result.host.is_empty()
                {
                    return Ok(Some(result));
                }
                self.request_redraw();
                Ok(None)
            }

            _ => Ok(None),
        }
    }

    fn open_file_picker(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Select Private Key File");

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
            let display = if let Some(home) = dirs::home_dir() {
                if let Ok(rest) = path.strip_prefix(&home) {
                    format!("~/{}", rest.display())
                } else {
                    path_str
                }
            } else {
                path_str
            };
            self.dialog
                .set_key_path(display, &mut self.gpu.font_system);
        }
    }

    fn render(&mut self) {
        let scale = self.window.scale_factor() as f32;
        let colors = &self.gpu.colors.clone();
        let theme = &self.theme.clone();

        let mut base_ctx = DrawContext::new();
        let mut base_text: Vec<TextArea> = Vec::new();
        self.dialog
            .draw(&mut base_ctx, &mut base_text, scale, colors);

        // Overlay: auth dropdown
        let mut overlay_ctx = DrawContext::new();
        let mut overlay_text_specs = Vec::new();
        self.dialog.auth_dropdown.draw(
            &mut overlay_ctx,
            &mut overlay_text_specs,
            theme,
            scale,
        );
        let mut overlay_text: Vec<TextArea> = Vec::new();
        crate::gpu::push_text_specs(
            &mut overlay_text,
            &overlay_text_specs,
            self.dialog.auth_dropdown.item_buffers(),
            scale,
        );

        match self.gpu.render_simple(
            self.gpu.colors.background.to_wgpu_color(),
            &base_ctx.rounded_quads,
            &overlay_ctx.rounded_quads,
            base_text,
            overlay_text,
        ) {
            Ok(()) => {}
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
            }
            Err(e) => {
                log::error!("ssh dialog render error: {e}");
            }
        }
    }
}
