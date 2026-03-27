use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use winit::keyboard::KeyCode;

// ---------------------------------------------------------------------------
// Action — hotkey IDs
// ---------------------------------------------------------------------------

/// All supported hotkey actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HotkeyAction {
    // Tab management
    NewTab,
    CloseTab,
    ReopenTab,
    NextTab,
    PreviousTab,
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    Tab5,
    Tab6,
    Tab7,
    Tab8,
    Tab9,
    Tab10,

    // Terminal
    Copy,
    Paste,
    SelectAll,
    Clear,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    Search,
    CtrlC,

    // Line editing
    Home,
    End,
    DeleteLine,

    // Application
    ToggleFullscreen,
    NewWindow,
    Settings,

    // Scrolling
    ScrollToTop,
    ScrollPageUp,
    ScrollUp,
    ScrollDown,
    ScrollPageDown,
    ScrollToBottom,
}

// ---------------------------------------------------------------------------
// Parsed keybinding from config string
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ParsedBinding {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
    key: String,
}

/// Parse a key binding string like "⌘-Shift-T", "Ctrl+⌘+F", "Alt-1", "F11".
/// Supports both `-` and `+` as separators.
fn parse_key_binding(s: &str) -> ParsedBinding {
    let mut tokens: Vec<&str> = Vec::new();
    for chunk in s.split('+') {
        for part in chunk.split('-') {
            if !part.is_empty() {
                tokens.push(part);
            }
        }
    }

    // Edge case: key is '-' (e.g. "⌘--" means Cmd+Minus).
    let key_is_minus = s.ends_with('-') && tokens.last().is_none_or(|t| is_modifier(t));

    let mut kb = ParsedBinding {
        ctrl: false,
        alt: false,
        shift: false,
        super_key: false,
        key: String::new(),
    };

    for (i, part) in tokens.iter().enumerate() {
        if i == tokens.len() - 1 && !is_modifier(part) && !key_is_minus {
            kb.key = part.to_string();
        } else {
            match *part {
                "Ctrl" => kb.ctrl = true,
                "Shift" => kb.shift = true,
                "Alt" | "⌥" => kb.alt = true,
                "⌘" => kb.super_key = true,
                _ => {
                    kb.key = part.to_string();
                }
            }
        }
    }

    if key_is_minus {
        kb.key = "-".to_string();
    }
    if kb.key.is_empty() {
        if let Some(last) = tokens.last() {
            kb.key = last.to_string();
        }
    }
    kb
}

fn is_modifier(s: &str) -> bool {
    matches!(s, "Ctrl" | "Shift" | "Alt" | "⌥" | "⌘")
}

// ---------------------------------------------------------------------------
// HotkeyConfig — the full mapping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub hotkeys: HashMap<HotkeyAction, Vec<String>>,
}

impl HotkeyConfig {
    /// Load platform defaults, then overlay user config per-action.
    /// User config replaces the binding array for each action it defines.
    /// An empty array `[]` disables the hotkey.
    pub fn load() -> Self {
        let mut cfg = Self::platform_defaults();
        if let Some(path) = config_path() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(user) = serde_json::from_str::<HotkeyConfig>(&data) {
                    for (action, bindings) in user.hotkeys {
                        cfg.hotkeys.insert(action, bindings);
                    }
                }
            }
        }
        cfg
    }

    /// Save current config to disk.
    pub fn save(&self) {
        if let Some(path) = config_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// Build a lookup table for fast runtime matching.
    pub fn build_lookup(&self) -> HotkeyLookup {
        let mut physical = HashMap::new();
        let mut logical = HashMap::new();

        for (action, keys) in &self.hotkeys {
            for key_str in keys {
                let kb = parse_key_binding(key_str);
                let mods = Modifiers {
                    ctrl: kb.ctrl,
                    alt: kb.alt,
                    shift: kb.shift,
                    super_key: kb.super_key,
                };
                // Letters, digits, named keys → physical code match.
                // Symbols (=, -, ,, ., etc.) → logical char match.
                if let Some(code) = key_name_to_physical(&kb.key) {
                    physical.insert(PhysicalCombo { mods, code }, *action);
                } else if let Some(ch) = key_name_to_logical_char(&kb.key) {
                    logical.insert(LogicalCombo { mods, ch }, *action);
                }
            }
        }
        HotkeyLookup { physical, logical }
    }

    #[cfg(target_os = "macos")]
    fn platform_defaults() -> Self {
        Self::macos_defaults()
    }

    #[cfg(not(target_os = "macos"))]
    fn platform_defaults() -> Self {
        Self::linux_defaults()
    }

    /// macOS defaults
    fn macos_defaults() -> Self {
        use HotkeyAction::*;
        let h: HashMap<HotkeyAction, Vec<String>> = HashMap::from([
            // Tab management
            (NewTab, vec!["⌘-T".into()]),
            (CloseTab, vec!["⌘-W".into()]),
            (ReopenTab, vec!["⌘-Shift-T".into()]),
            (NextTab, vec!["Ctrl-Tab".into()]),
            (PreviousTab, vec!["Ctrl-Shift-Tab".into()]),
            (Tab1, vec!["⌘-1".into()]),
            (Tab2, vec!["⌘-2".into()]),
            (Tab3, vec!["⌘-3".into()]),
            (Tab4, vec!["⌘-4".into()]),
            (Tab5, vec!["⌘-5".into()]),
            (Tab6, vec!["⌘-6".into()]),
            (Tab7, vec!["⌘-7".into()]),
            (Tab8, vec!["⌘-8".into()]),
            (Tab9, vec!["⌘-9".into()]),
            // Terminal
            (Copy, vec!["⌘-C".into()]),
            (Paste, vec!["⌘-V".into()]),
            (SelectAll, vec!["⌘-A".into()]),
            (Clear, vec!["⌘-K".into()]),
            (ZoomIn, vec!["⌘-=".into(), "⌘-Shift-=".into()]),
            (ZoomOut, vec!["⌘--".into(), "⌘-Shift--".into()]),
            (ResetZoom, vec!["⌘-0".into()]),
            (Search, vec!["⌘-F".into()]),
            (CtrlC, vec!["Ctrl-C".into()]),
            // Line editing
            (Home, vec!["⌘-Left".into()]),
            (End, vec!["⌘-Right".into()]),
            (DeleteLine, vec!["⌘-Backspace".into()]),
            // Application
            (ToggleFullscreen, vec!["Ctrl+⌘+F".into(), "F11".into()]),
            (NewWindow, vec!["⌘-N".into()]),
            (Settings, vec!["⌘-,".into()]),
            // Scrolling
            (ScrollToTop, vec!["Shift-PageUp".into()]),
            (ScrollPageUp, vec!["⌥-PageUp".into()]),
            (ScrollUp, vec!["Ctrl-Shift-Up".into()]),
            (ScrollDown, vec!["Ctrl-Shift-Down".into()]),
            (ScrollPageDown, vec!["⌥-PageDown".into()]),
            (ScrollToBottom, vec!["Shift-PageDown".into()]),
        ]);
        Self { hotkeys: h }
    }

    /// Linux/Windows defaults
    fn linux_defaults() -> Self {
        use HotkeyAction::*;
        let h: HashMap<HotkeyAction, Vec<String>> = HashMap::from([
            // Tab management
            (NewTab, vec!["Ctrl-Shift-T".into()]),
            (CloseTab, vec!["Ctrl-Shift-W".into()]),
            (ReopenTab, vec!["Ctrl-Shift-Z".into()]),
            (NextTab, vec!["Ctrl-Tab".into(), "Ctrl-Shift-Right".into()]),
            (PreviousTab, vec!["Ctrl-Shift-Tab".into(), "Ctrl-Shift-Left".into()]),
            (Tab1, vec!["Alt-1".into()]),
            (Tab2, vec!["Alt-2".into()]),
            (Tab3, vec!["Alt-3".into()]),
            (Tab4, vec!["Alt-4".into()]),
            (Tab5, vec!["Alt-5".into()]),
            (Tab6, vec!["Alt-6".into()]),
            (Tab7, vec!["Alt-7".into()]),
            (Tab8, vec!["Alt-8".into()]),
            (Tab9, vec!["Alt-9".into()]),
            (Tab10, vec!["Alt-0".into()]),
            // Terminal
            (Copy, vec!["Ctrl-Shift-C".into()]),
            (Paste, vec!["Ctrl-Shift-V".into(), "Shift-Insert".into()]),
            (SelectAll, vec!["Ctrl-Shift-A".into()]),
            (Clear, vec![]),
            (ZoomIn, vec!["Ctrl-=".into(), "Ctrl-Shift-=".into()]),
            (ZoomOut, vec!["Ctrl--".into(), "Ctrl-Shift--".into()]),
            (ResetZoom, vec!["Ctrl-0".into()]),
            (Search, vec!["Ctrl-Shift-F".into()]),
            (CtrlC, vec!["Ctrl-C".into()]),
            // Line editing
            (DeleteLine, vec!["Ctrl-Shift-Backspace".into()]),
            // Application
            (ToggleFullscreen, vec!["F11".into(), "Alt-Enter".into()]),
            (NewWindow, vec!["Ctrl-Shift-N".into()]),
            (Settings, vec!["Ctrl-,".into()]),
            // Scrolling
            (ScrollToTop, vec!["Ctrl-PageUp".into()]),
            (ScrollPageUp, vec!["Alt-PageUp".into()]),
            (ScrollUp, vec!["Ctrl-Shift-Up".into()]),
            (ScrollDown, vec!["Ctrl-Shift-Down".into()]),
            (ScrollPageDown, vec!["Alt-PageDown".into()]),
            (ScrollToBottom, vec!["Ctrl-PageDown".into()]),
        ]);
        Self { hotkeys: h }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("pfauterminal").join("hotkeys.json"))
}

// ---------------------------------------------------------------------------
// HotkeyLookup — dual physical/logical matching
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Modifiers {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PhysicalCombo {
    mods: Modifiers,
    code: KeyCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LogicalCombo {
    mods: Modifiers,
    ch: char,
}

pub struct HotkeyLookup {
    /// Letters (A-Z), digits (0-9), named keys (F1-F12, arrows, etc.)
    physical: HashMap<PhysicalCombo, HotkeyAction>,
    /// Symbols (=, -, ,, ., etc.) — matched via logical key / character
    logical: HashMap<LogicalCombo, HotkeyAction>,
}

impl HotkeyLookup {
    /// Match a key event against the hotkey table.
    /// `physical_code` — from `event.physical_key`
    /// `logical_char` — first char from `event.logical_key` (if it's a character)
    pub fn match_key(
        &self,
        physical_code: KeyCode,
        logical_char: Option<char>,
        ctrl: bool,
        alt: bool,
        shift: bool,
        super_key: bool,
    ) -> Option<HotkeyAction> {
        let mods = Modifiers { ctrl, alt, shift, super_key };

        // Try physical match first (letters, digits, named keys)
        if let Some(action) = self.physical.get(&PhysicalCombo { mods, code: physical_code }) {
            return Some(*action);
        }

        // Try logical match (symbols like =, -, ,)
        if let Some(ch) = logical_char {
            if let Some(action) = self.logical.get(&LogicalCombo { mods, ch }) {
                return Some(*action);
            }
            // Try lowercase variant
            let lower = ch.to_ascii_lowercase();
            if lower != ch {
                if let Some(action) = self.logical.get(&LogicalCombo { mods, ch: lower }) {
                    return Some(*action);
                }
            }
        }

        // Fallback: for symbol keys, also try matching the physical key's
        // *unshifted* character against logical bindings. This handles cases
        // like "⌘-Shift-=" where the actual logical_char is '+' (shifted).
        // We derive the base symbol from the physical key code.
        if let Some(base_ch) = physical_code_to_base_symbol(physical_code) {
            if let Some(action) = self.logical.get(&LogicalCombo { mods, ch: base_ch }) {
                return Some(*action);
            }
            // Also try without shift — config says "⌘-Shift-=" but the char
            // is always '=' regardless of shift state in our lookup table.
            if shift {
                let mods_no_shift = Modifiers { ctrl, alt, shift: false, super_key };
                if let Some(action) = self.logical.get(&LogicalCombo { mods: mods_no_shift, ch: base_ch }) {
                    return Some(*action);
                }
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Key name classification: physical vs logical
// ---------------------------------------------------------------------------

/// Keys that should match on physical key code.
/// Letters, digits, and named keys (arrows, F-keys, Tab, etc.)
fn key_name_to_physical(name: &str) -> Option<KeyCode> {
    if name.len() == 1 {
        let ch = name.chars().next().unwrap();
        if let Some(code) = letter_to_code(ch) {
            return Some(code);
        }
        if let Some(code) = digit_to_code(ch) {
            return Some(code);
        }
        // Single-char symbols go to logical matching, not here
        return None;
    }
    named_key_to_code(name)
}

/// Keys that should match on logical character (the symbol the keyboard produces).
/// This handles layout differences: on AZERTY, '=' is on a different physical key.
fn key_name_to_logical_char(name: &str) -> Option<char> {
    if name.len() == 1 {
        let ch = name.chars().next().unwrap();
        // Only symbols — letters and digits go through physical matching
        if ch.is_ascii_alphabetic() || ch.is_ascii_digit() {
            return None;
        }
        return Some(ch);
    }
    None
}

fn letter_to_code(ch: char) -> Option<KeyCode> {
    match ch.to_ascii_uppercase() {
        'A' => Some(KeyCode::KeyA),
        'B' => Some(KeyCode::KeyB),
        'C' => Some(KeyCode::KeyC),
        'D' => Some(KeyCode::KeyD),
        'E' => Some(KeyCode::KeyE),
        'F' => Some(KeyCode::KeyF),
        'G' => Some(KeyCode::KeyG),
        'H' => Some(KeyCode::KeyH),
        'I' => Some(KeyCode::KeyI),
        'J' => Some(KeyCode::KeyJ),
        'K' => Some(KeyCode::KeyK),
        'L' => Some(KeyCode::KeyL),
        'M' => Some(KeyCode::KeyM),
        'N' => Some(KeyCode::KeyN),
        'O' => Some(KeyCode::KeyO),
        'P' => Some(KeyCode::KeyP),
        'Q' => Some(KeyCode::KeyQ),
        'R' => Some(KeyCode::KeyR),
        'S' => Some(KeyCode::KeyS),
        'T' => Some(KeyCode::KeyT),
        'U' => Some(KeyCode::KeyU),
        'V' => Some(KeyCode::KeyV),
        'W' => Some(KeyCode::KeyW),
        'X' => Some(KeyCode::KeyX),
        'Y' => Some(KeyCode::KeyY),
        'Z' => Some(KeyCode::KeyZ),
        _ => None,
    }
}

fn digit_to_code(ch: char) -> Option<KeyCode> {
    match ch {
        '0' => Some(KeyCode::Digit0),
        '1' => Some(KeyCode::Digit1),
        '2' => Some(KeyCode::Digit2),
        '3' => Some(KeyCode::Digit3),
        '4' => Some(KeyCode::Digit4),
        '5' => Some(KeyCode::Digit5),
        '6' => Some(KeyCode::Digit6),
        '7' => Some(KeyCode::Digit7),
        '8' => Some(KeyCode::Digit8),
        '9' => Some(KeyCode::Digit9),
        _ => None,
    }
}

/// Map physical key code back to its base (unshifted) symbol on a US layout.
/// Used as fallback when logical_char doesn't match due to Shift being held.
fn physical_code_to_base_symbol(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Equal => Some('='),
        KeyCode::Minus => Some('-'),
        KeyCode::Comma => Some(','),
        KeyCode::Period => Some('.'),
        KeyCode::Slash => Some('/'),
        KeyCode::Semicolon => Some(';'),
        KeyCode::Quote => Some('\''),
        KeyCode::BracketLeft => Some('['),
        KeyCode::BracketRight => Some(']'),
        KeyCode::Backslash => Some('\\'),
        KeyCode::Backquote => Some('`'),
        _ => None,
    }
}

fn named_key_to_code(name: &str) -> Option<KeyCode> {
    match name {
        "Tab" => Some(KeyCode::Tab),
        "Enter" | "Return" => Some(KeyCode::Enter),
        "Space" => Some(KeyCode::Space),
        "Backspace" => Some(KeyCode::Backspace),
        "Delete" => Some(KeyCode::Delete),
        "Insert" => Some(KeyCode::Insert),
        "Escape" | "Esc" => Some(KeyCode::Escape),
        "Up" => Some(KeyCode::ArrowUp),
        "Down" => Some(KeyCode::ArrowDown),
        "Left" => Some(KeyCode::ArrowLeft),
        "Right" => Some(KeyCode::ArrowRight),
        "Home" => Some(KeyCode::Home),
        "End" => Some(KeyCode::End),
        "PageUp" => Some(KeyCode::PageUp),
        "PageDown" => Some(KeyCode::PageDown),
        "F1" => Some(KeyCode::F1),
        "F2" => Some(KeyCode::F2),
        "F3" => Some(KeyCode::F3),
        "F4" => Some(KeyCode::F4),
        "F5" => Some(KeyCode::F5),
        "F6" => Some(KeyCode::F6),
        "F7" => Some(KeyCode::F7),
        "F8" => Some(KeyCode::F8),
        "F9" => Some(KeyCode::F9),
        "F10" => Some(KeyCode::F10),
        "F11" => Some(KeyCode::F11),
        "F12" => Some(KeyCode::F12),
        _ => None,
    }
}
