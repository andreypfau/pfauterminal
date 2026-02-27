use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use alacritty_terminal::tty;
use alacritty_terminal::Term;
use glyphon::{Buffer, Color as GlyphonColor, FontSystem, Shaping};
use tokio::sync::mpsc;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

use crate::colors::ColorScheme;
use crate::draw::DrawContext;
use crate::font::{self, CellMetrics};
use crate::layout::{CursorData, Rect, TextSpec};
use crate::ssh::{SshConfig, SshMsg};
use crate::theme::PanelTheme;

// --- Events ---

/// Custom event sent from the terminal I/O thread to the winit event loop.
#[derive(Debug)]
pub enum TerminalEvent {
    Wakeup,
    Title(PanelId, String),
    Exit(PanelId),
    /// Deferred SSH dialog close — carries an optional result (None = cancelled).
    SshDialogClose(Option<crate::ssh_dialog::SshResult>),
}

// --- Panel types ---

static NEXT_PANEL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelId(u64);

impl PanelId {
    pub fn next() -> Self {
        Self(NEXT_PANEL_ID.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct PanelViewport {
    pub rect: Rect,
    pub content_rect: Rect,
    pub cols: usize,
    pub rows: usize,
    pub scale_factor: f32,
}

// --- Terminal dimensions ---

#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl TermSize {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns: columns.max(2),
            screen_lines: screen_lines.max(2),
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

// --- Event proxy (bridges alacritty events to winit) ---

/// I/O backend: either a local PTY or an SSH channel.
enum Backend {
    Local(EventLoopSender),
    Ssh(mpsc::UnboundedSender<SshMsg>),
}

impl Backend {
    fn send_input(&self, data: Cow<'static, [u8]>) {
        match self {
            Backend::Local(ch) => {
                let _ = ch.send(Msg::Input(data));
            }
            Backend::Ssh(tx) => {
                let _ = tx.send(SshMsg::Input(data));
            }
        }
    }

    fn send_resize(&self, size: TermSize, cell_width: u16, cell_height: u16) {
        match self {
            Backend::Local(ch) => {
                let _ = ch.send(Msg::Resize(WindowSize {
                    num_lines: size.screen_lines as u16,
                    num_cols: size.columns as u16,
                    cell_width,
                    cell_height,
                }));
            }
            Backend::Ssh(tx) => {
                let _ = tx.send(SshMsg::Resize {
                    cols: size.columns as u16,
                    rows: size.screen_lines as u16,
                });
            }
        }
    }
}

/// Bridges alacritty's EventListener to winit's EventLoopProxy.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<TerminalEvent>,
    panel_id: PanelId,
    backend: Arc<Mutex<Option<Backend>>>,
}

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<TerminalEvent>, panel_id: PanelId) -> Self {
        Self {
            proxy,
            panel_id,
            backend: Arc::new(Mutex::new(None)),
        }
    }

    fn set_backend(&self, backend: Backend) {
        if let Ok(mut guard) = self.backend.lock() {
            *guard = Some(backend);
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = match event {
            Event::Wakeup => self.proxy.send_event(TerminalEvent::Wakeup),
            Event::Title(t) => self
                .proxy
                .send_event(TerminalEvent::Title(self.panel_id, t)),
            Event::Exit | Event::ChildExit(_) => {
                self.proxy.send_event(TerminalEvent::Exit(self.panel_id))
            }
            Event::PtyWrite(text) => {
                if let Ok(guard) = self.backend.lock()
                    && let Some(backend) = guard.as_ref()
                {
                    backend.send_input(Cow::Owned(text.into_bytes()));
                }
                Ok(())
            }
            _ => Ok(()),
        };
    }
}

// --- Cursor animation ---

const CURSOR_SPEED_TYPING: f32 = 50.0;
const CURSOR_SPEED_GLIDE: f32 = 12.0;
const CURSOR_RADIUS: f32 = 2.0;

struct CursorAnimation {
    prev_x: f32,
    prev_y: f32,
    target_x: f32,
    target_y: f32,
    prev_w: f32,
    prev_h: f32,
    target_w: f32,
    target_h: f32,
    move_time: Instant,
    last_input_time: Instant,
    move_speed: f32,
    first_frame: bool,
}

impl CursorAnimation {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            prev_x: 0.0,
            prev_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            prev_w: 0.0,
            prev_h: 0.0,
            target_w: 0.0,
            target_h: 0.0,
            move_time: now,
            last_input_time: now,
            move_speed: CURSOR_SPEED_GLIDE,
            first_frame: true,
        }
    }

    fn set_target(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.first_frame {
            self.prev_x = x;
            self.prev_y = y;
            self.prev_w = w;
            self.prev_h = h;
            self.target_x = x;
            self.target_y = y;
            self.target_w = w;
            self.target_h = h;
            self.move_time = Instant::now();
            self.first_frame = false;
            return;
        }

        let x_changed = (x - self.target_x).abs() > 0.5;
        let y_changed = (y - self.target_y).abs() > 0.5;
        let size_changed = (w - self.target_w).abs() > 0.5 || (h - self.target_h).abs() > 0.5;
        if x_changed || y_changed || size_changed {
            if y_changed || size_changed {
                // Vertical movement or shape change — snap instantly
                self.prev_x = x;
                self.prev_y = y;
                self.prev_w = w;
                self.prev_h = h;
            } else {
                // Horizontal only — animate from current interpolated position
                let elapsed = self.move_time.elapsed().as_secs_f32();
                let factor = (-self.move_speed * elapsed).exp();
                self.prev_x = self.target_x + (self.prev_x - self.target_x) * factor;
                self.prev_y = y;
                self.prev_w = w;
                self.prev_h = h;
            }
            self.target_x = x;
            self.target_y = y;
            self.target_w = w;
            self.target_h = h;
            self.move_time = Instant::now();

            // Pick speed for this new segment: typing = fast snap, otherwise glide
            let input_age = self.last_input_time.elapsed().as_secs_f32();
            self.move_speed = if input_age < 0.05 {
                CURSOR_SPEED_TYPING
            } else {
                CURSOR_SPEED_GLIDE
            };
        }
    }

    fn on_input(&mut self) {
        self.last_input_time = Instant::now();
    }

    fn snap(&mut self) {
        self.prev_x = self.target_x;
        self.prev_y = self.target_y;
        self.prev_w = self.target_w;
        self.prev_h = self.target_h;
        self.move_time = Instant::now();
    }
}

// --- Terminal panel ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CharKey {
    ch: char,
    bold: bool,
    italic: bool,
}

pub struct TerminalPanel {
    id: PanelId,
    term: Arc<FairMutex<Term<EventProxy>>>,
    backend: Backend,
    viewport: Option<PanelViewport>,
    char_buffers: Vec<Buffer>,
    char_key_map: HashMap<CharKey, usize>,
    title: String,
    /// Sub-cell pixel offset for smooth trackpad scrolling (physical pixels).
    scroll_pixel_offset: f32,
    /// Raw pixel accumulator not yet committed as whole lines (physical pixels).
    scroll_accumulator: f64,
    /// Our own copy of the selection, kept in sync manually.
    /// alacritty_terminal's internal processing can clear `term.selection`
    /// at any time (line clears, scrolling, etc.), so we maintain our own
    /// copy and re-apply it before each use.
    active_selection: Option<Selection>,
    cursor_anim: CursorAnimation,
    /// Whether the cursor was visible in the last draw call.
    cursor_visible: bool,
}

impl TerminalPanel {
    pub fn new(
        id: PanelId,
        size: TermSize,
        cell_px: (u16, u16),
        event_proxy: EventProxy,
        shell: Option<String>,
        args: Vec<String>,
    ) -> Self {
        let config = alacritty_terminal::term::Config::default();
        let term = Term::new(config, &size, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_config = tty::Options {
            shell: shell.map(|program| tty::Shell::new(program, args)),
            working_directory: None,
            drain_on_exit: true,
            env: {
                let mut env = std::collections::HashMap::new();
                env.insert("TERM".into(), "xterm-256color".into());
                env.insert("COLORTERM".into(), "truecolor".into());
                env
            },
            #[cfg(target_os = "windows")]
            escape_args: false,
        };

        let window_size = WindowSize {
            num_lines: size.screen_lines as u16,
            num_cols: size.columns as u16,
            cell_width: cell_px.0,
            cell_height: cell_px.1,
        };

        let pty = tty::new(&pty_config, window_size, 0).expect("failed to create PTY");
        let event_loop = EventLoop::new(term.clone(), event_proxy.clone(), pty, false, false)
            .expect("failed to create event loop");
        let channel = event_loop.channel();

        event_proxy.set_backend(Backend::Local(channel.clone()));

        event_loop.spawn();

        Self {
            id,
            term,
            backend: Backend::Local(channel),
            viewport: None,
            char_buffers: Vec::new(),
            char_key_map: HashMap::new(),
            title: String::from("Terminal"),
            scroll_pixel_offset: 0.0,
            scroll_accumulator: 0.0,
            active_selection: None,
            cursor_anim: CursorAnimation::new(),
            cursor_visible: false,
        }
    }

    pub fn new_ssh(
        id: PanelId,
        size: TermSize,
        event_proxy: EventProxy,
        ssh_config: SshConfig,
    ) -> Self {
        let (term, sender) = crate::ssh::spawn_ssh_thread(ssh_config, size, event_proxy.clone());

        event_proxy.set_backend(Backend::Ssh(sender.clone()));

        Self {
            id,
            term,
            backend: Backend::Ssh(sender),
            viewport: None,
            char_buffers: Vec::new(),
            char_key_map: HashMap::new(),
            title: String::from("SSH"),
            scroll_pixel_offset: 0.0,
            scroll_accumulator: 0.0,
            active_selection: None,
            cursor_anim: CursorAnimation::new(),
            cursor_visible: false,
        }
    }

    pub fn compute_viewport(
        rect: &Rect,
        cell: &CellMetrics,
        scale_factor: f32,
        tab_bar_height: f32,
        panel_theme: &PanelTheme,
    ) -> PanelViewport {
        let p = panel_theme.island_padding * scale_factor;
        let content = Rect {
            x: rect.x + p,
            y: rect.y + tab_bar_height + p,
            width: rect.width - 2.0 * p,
            height: rect.height - tab_bar_height - 2.0 * p,
        };
        let pcw = cell.width * scale_factor;
        let pch = cell.height * scale_factor;
        let cols = (content.width / pcw).floor().max(1.0) as usize;
        let rows = (content.height / pch).floor().max(1.0) as usize;
        PanelViewport {
            rect: *rect,
            content_rect: content,
            cols,
            rows,
            scale_factor,
        }
    }

    pub fn id(&self) -> PanelId {
        self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    pub fn set_viewport(&mut self, viewport: PanelViewport, cell: &CellMetrics) {
        let dims_changed = self
            .viewport
            .as_ref()
            .is_none_or(|v| v.cols != viewport.cols || v.rows != viewport.rows);

        if dims_changed {
            let cell_w = (cell.width * viewport.scale_factor) as u16;
            let cell_h = (cell.height * viewport.scale_factor) as u16;
            let size = TermSize::new(viewport.cols, viewport.rows);
            self.backend.send_resize(size, cell_w, cell_h);
            self.term.lock().resize(size);
            self.char_buffers.clear();
            self.char_key_map.clear();
            self.cursor_anim.snap();
        }

        self.viewport = Some(viewport);
    }

    /// Notify the cursor animation that user input occurred (resets blink).
    pub fn notify_input(&mut self) {
        self.cursor_anim.on_input();
    }

    /// Returns true if the cursor was visible in the last draw call.
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn handle_key(&mut self, event: &KeyEvent, ctrl: bool, alt: bool) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }

        // Don't clear selection for modifier-only keys (Cmd, Ctrl, Alt, Shift).
        let is_modifier = matches!(
            event.physical_key,
            PhysicalKey::Code(
                KeyCode::SuperLeft
                    | KeyCode::SuperRight
                    | KeyCode::ControlLeft
                    | KeyCode::ControlRight
                    | KeyCode::AltLeft
                    | KeyCode::AltRight
                    | KeyCode::ShiftLeft
                    | KeyCode::ShiftRight
            )
        );
        if is_modifier {
            return false;
        }

        self.cursor_anim.on_input();
        self.active_selection = None;
        {
            let mut term = self.term.lock();
            term.selection = None;
            if term.grid().display_offset() != 0 {
                term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
            }
        }
        self.scroll_pixel_offset = 0.0;
        self.scroll_accumulator = 0.0;

        let mode = *self.term.lock().mode();
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        // SS3 prefix for application cursor mode, CSI for normal mode
        let cursor_prefix: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };

        // Handle Ctrl+key → control characters (0x00–0x1F)
        if ctrl {
            let ctrl_byte = match event.logical_key.as_ref() {
                Key::Character(c) if c.len() == 1 => {
                    let ch = c.chars().next().unwrap();
                    match ch {
                        'a'..='z' => Some(ch as u8 - b'a' + 1),
                        'A'..='Z' => Some(ch as u8 - b'A' + 1),
                        '@' | ' ' => Some(0x00),
                        '[' => Some(0x1B),
                        '\\' => Some(0x1C),
                        ']' => Some(0x1D),
                        '^' | '~' => Some(0x1E),
                        '_' | '/' => Some(0x1F),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(b) = ctrl_byte {
                let data = if alt { vec![0x1B, b] } else { vec![b] };
                self.backend.send_input(Cow::Owned(data));
                return true;
            }
        }

        // Handle Alt+key → ESC prefix + key
        if alt {
            if let Some(t) = &event.text
                && !t.is_empty()
            {
                let mut data = vec![0x1B];
                data.extend_from_slice(t.as_bytes());
                self.backend.send_input(Cow::Owned(data));
                return true;
            }
        }

        let bytes = match event.logical_key.as_ref() {
            Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
            Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
            Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
            Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
            Key::Named(NamedKey::ArrowUp) => Some([cursor_prefix, b"A"].concat()),
            Key::Named(NamedKey::ArrowDown) => Some([cursor_prefix, b"B"].concat()),
            Key::Named(NamedKey::ArrowRight) => Some([cursor_prefix, b"C"].concat()),
            Key::Named(NamedKey::ArrowLeft) => Some([cursor_prefix, b"D"].concat()),
            Key::Named(NamedKey::Home) => Some([cursor_prefix, b"H"].concat()),
            Key::Named(NamedKey::End) => Some([cursor_prefix, b"F"].concat()),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
            Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
            Key::Named(NamedKey::F1) => Some(b"\x1bOP".to_vec()),
            Key::Named(NamedKey::F2) => Some(b"\x1bOQ".to_vec()),
            Key::Named(NamedKey::F3) => Some(b"\x1bOR".to_vec()),
            Key::Named(NamedKey::F4) => Some(b"\x1bOS".to_vec()),
            Key::Named(NamedKey::F5) => Some(b"\x1b[15~".to_vec()),
            Key::Named(NamedKey::F6) => Some(b"\x1b[17~".to_vec()),
            Key::Named(NamedKey::F7) => Some(b"\x1b[18~".to_vec()),
            Key::Named(NamedKey::F8) => Some(b"\x1b[19~".to_vec()),
            Key::Named(NamedKey::F9) => Some(b"\x1b[20~".to_vec()),
            Key::Named(NamedKey::F10) => Some(b"\x1b[21~".to_vec()),
            Key::Named(NamedKey::F11) => Some(b"\x1b[23~".to_vec()),
            Key::Named(NamedKey::F12) => Some(b"\x1b[24~".to_vec()),
            _ => None,
        };

        if let Some(b) = bytes {
            self.backend.send_input(Cow::Owned(b));
            true
        } else if let Some(t) = &event.text
            && !t.is_empty()
        {
            self.backend
                .send_input(Cow::Owned(t.to_string().into_bytes()));
            true
        } else {
            false
        }
    }

    pub fn handle_scroll(&mut self, delta: MouseScrollDelta, cell_height: f64) -> bool {
        match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                let lines = y as i32;
                if lines != 0 {
                    let mut term = self.term.lock();
                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                    self.scroll_pixel_offset = 0.0;
                    self.scroll_accumulator = 0.0;
                    true
                } else {
                    false
                }
            }
            MouseScrollDelta::PixelDelta(pos) => {
                self.scroll_accumulator += pos.y;
                let mut term = self.term.lock();
                let lines = (self.scroll_accumulator / cell_height) as i32;
                if lines != 0 {
                    let offset_before = term.grid().display_offset();
                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                    let offset_after = term.grid().display_offset();
                    let actual = offset_after as i32 - offset_before as i32;
                    self.scroll_accumulator -= actual as f64 * cell_height;
                    if actual != lines {
                        // Scroll was clamped (hit top/bottom of scrollback)
                        self.scroll_accumulator = 0.0;
                    }
                }
                // Clamp at scroll boundaries
                let offset = term.grid().display_offset();
                if offset == 0 && self.scroll_accumulator < 0.0 {
                    self.scroll_accumulator = 0.0;
                }
                let max_offset = term.grid().total_lines().saturating_sub(term.grid().screen_lines());
                if offset >= max_offset && self.scroll_accumulator > 0.0 {
                    self.scroll_accumulator = 0.0;
                }
                drop(term);
                self.scroll_pixel_offset = self.scroll_accumulator as f32;
                true
            }
        }
    }

    /// Returns true if a smooth scroll animation is in progress.
    pub fn is_smooth_scrolling(&self) -> bool {
        self.scroll_pixel_offset != 0.0
    }

    /// Map physical-pixel coordinates to a grid Point + Side.
    /// Returns None if the position is outside the content area.
    pub fn pixel_to_point(&self, px: f32, py: f32) -> Option<(Point, Side)> {
        let vp = self.viewport.as_ref()?;
        let cr = &vp.content_rect;

        if px < cr.x || px >= cr.x + cr.width || py < cr.y || py >= cr.y + cr.height {
            return None;
        }

        let pcw = cr.width / vp.cols as f32;
        let pch = cr.height / vp.rows as f32;

        let rel_x = px - cr.x;
        let rel_y = py - cr.y - self.scroll_pixel_offset;

        let col = (rel_x / pcw).floor().clamp(0.0, (vp.cols - 1) as f32) as usize;
        let row = (rel_y / pch).floor().clamp(0.0, (vp.rows - 1) as f32) as usize;

        let side = if (rel_x - col as f32 * pcw) < pcw / 2.0 {
            Side::Left
        } else {
            Side::Right
        };

        let term = self.term.lock();
        let display_offset = term.grid().display_offset();
        let point = term::viewport_to_point(display_offset, Point::new(row, Column(col)));
        Some((point, side))
    }

    /// Returns true if the pixel position is inside the terminal content area.
    pub fn is_in_content_area(&self, px: f32, py: f32) -> bool {
        self.viewport.as_ref().is_some_and(|vp| {
            let cr = &vp.content_rect;
            px >= cr.x && px < cr.x + cr.width && py >= cr.y && py < cr.y + cr.height
        })
    }

    pub fn start_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        let sel = Selection::new(ty, point, side);
        self.active_selection = Some(sel.clone());
        self.term.lock().selection = Some(sel);
    }

    pub fn update_selection(&mut self, point: Point, side: Side) {
        if let Some(ref mut sel) = self.active_selection {
            sel.update(point, side);
            self.term.lock().selection = Some(sel.clone());
        }
    }

    pub fn clear_selection(&mut self) {
        self.active_selection = None;
        self.term.lock().selection = None;
    }

    pub fn selection_to_string(&self) -> Option<String> {
        // Re-apply our selection since terminal output may have cleared it.
        let mut term = self.term.lock();
        if let Some(sel) = &self.active_selection {
            term.selection = Some(sel.clone());
        }
        term.selection_to_string()
    }

    pub fn has_selection(&self) -> bool {
        self.active_selection.is_some()
    }

    pub fn draw(
        &mut self,
        ctx: &mut DrawContext,
        text_specs: &mut Vec<TextSpec>,
        font_system: &mut FontSystem,
        colors: &ColorScheme,
        cell_metrics: &CellMetrics,
        panel_theme: &PanelTheme,
    ) {
        let vp = match &self.viewport {
            Some(vp) => vp,
            None => return,
        };

        let scale = vp.scale_factor;
        let rows = vp.rows;
        let cols = vp.cols;
        let pcw = vp.content_rect.width / cols as f32;
        let pch = vp.content_rect.height / rows as f32;
        let content_x = vp.content_rect.x;
        let content_y = vp.content_rect.y;
        let content_clip = vp.content_rect;
        let content_bottom = content_y + vp.content_rect.height;
        let island_rect = vp.rect;
        let pixel_offset = self.scroll_pixel_offset;

        // Island background (stroke + fill)
        let island_radius = panel_theme.island_radius * scale;
        let island_stroke = panel_theme.island_stroke_width * scale;
        if island_stroke > 0.0 {
            ctx.stroked_rect(
                &island_rect,
                colors.panel_stroke.to_linear_f32(),
                colors.background.to_linear_f32(),
                island_radius,
                island_stroke,
            );
        } else {
            ctx.rounded_rect(island_rect, colors.background.to_linear_f32(), island_radius);
        }

        let metrics = font::metrics();

        {
            use alacritty_terminal::term::cell::Flags;

            let mut term = self.term.lock();
            // Re-apply our selection — terminal output may have cleared it.
            if let Some(sel) = &self.active_selection {
                term.selection = Some(sel.clone());
            }
            let content = term.renderable_content();
            let display_offset = content.display_offset;
            let cursor_point = content.cursor.point;
            let cursor_shape = content.cursor.shape;
            let selection_range = content.selection;
            let selection_color = colors.selection.to_linear_f32();

            // display_iter provides screen_lines+1 rows: one extra above the
            // viewport (viewport_line == -1). We render it when pixel_offset > 0
            // to fill the gap at the top during smooth upward scroll.
            let min_vline = if pixel_offset > 0.0 { -1 } else { 0 };

            for indexed in content.display_iter {
                let viewport_line = indexed.point.line.0 + display_offset as i32;
                let col = indexed.point.column.0;
                if viewport_line < min_vline || viewport_line >= rows as i32 || col >= cols {
                    continue;
                }

                let cell = &*indexed;
                let flags = cell.flags;

                let (fg_color, bg_color) = if flags.contains(Flags::INVERSE) {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };

                let selected = selection_range.is_some_and(|r| r.contains(indexed.point));
                let cy = content_y + viewport_line as f32 * pch + pixel_offset;

                if selected {
                    let quad = Rect {
                        x: content_x + col as f32 * pcw,
                        y: cy,
                        width: pcw,
                        height: pch,
                    };
                    if let Some(clipped) = quad.clip_y(content_y, content_bottom) {
                        ctx.flat_quad(clipped, selection_color);
                    }
                } else if !colors.is_default_bg(bg_color) {
                    let quad = Rect {
                        x: content_x + col as f32 * pcw,
                        y: cy,
                        width: pcw,
                        height: pch,
                    };
                    if let Some(clipped) = quad.clip_y(content_y, content_bottom) {
                        ctx.flat_quad(clipped, colors.to_rgba(bg_color));
                    }
                }

                let is_invisible = flags.intersects(
                    Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN,
                );
                let c = if is_invisible { ' ' } else { cell.c };
                if c == ' ' || c == '\0' {
                    continue;
                }

                let dim = flags.contains(Flags::DIM);
                let fg = if dim {
                    let base = colors.to_glyphon_fg(fg_color);
                    GlyphonColor::rgba(base.r() / 2, base.g() / 2, base.b() / 2, base.a())
                } else {
                    colors.to_glyphon_fg(fg_color)
                };

                let cx = content_x + col as f32 * pcw;

                if let Some(rects) = block_char_rects(c, cx, cy, pcw, pch) {
                    let color = glyphon_to_linear(fg);
                    for r in rects {
                        if let Some(clipped) = r.clip_y(content_y, content_bottom) {
                            ctx.flat_quad(clipped, color);
                        }
                    }
                    continue;
                }

                let bold = flags.contains(Flags::BOLD);
                let italic = flags.contains(Flags::ITALIC);
                let key = CharKey { ch: c, bold, italic };
                let buf_idx = get_or_create_buffer(
                    &mut self.char_buffers,
                    &mut self.char_key_map,
                    key,
                    font_system,
                    metrics,
                    cell_metrics,
                );

                text_specs.push(TextSpec {
                    left: cx,
                    top: cy,
                    color: fg,
                    buffer_index: buf_idx,
                    bounds: content_clip,
                });
            }

            // Animated cursor (hidden when TUI apps send ESC[?25l)
            if cursor_shape != CursorShape::Hidden {
                let cur_viewport_line = cursor_point.line.0 + display_offset as i32;
                let cur_col = cursor_point.column.0;
                if cur_viewport_line >= 0 {
                    let cur_row = cur_viewport_line as usize;
                    if cur_row < rows && cur_col < cols {
                        let cx = content_x + cur_col as f32 * pcw;
                        let cy = content_y + cur_row as f32 * pch + pixel_offset;

                        let (tw, th) = match cursor_shape {
                            CursorShape::Beam => (2.0 * scale, pch),
                            CursorShape::Underline => (pcw, 2.0 * scale),
                            _ => (pcw, pch), // Block and default
                        };

                        let ty = match cursor_shape {
                            CursorShape::Underline => cy + pch - th,
                            _ => cy,
                        };

                        self.cursor_anim.set_target(cx, ty, tw, th);

                        let anim = &self.cursor_anim;
                        ctx.cursor = Some(CursorData {
                            prev_rect: Rect {
                                x: anim.prev_x,
                                y: anim.prev_y,
                                width: anim.prev_w,
                                height: anim.prev_h,
                            },
                            target_rect: Rect {
                                x: anim.target_x,
                                y: anim.target_y,
                                width: anim.target_w,
                                height: anim.target_h,
                            },
                            color: colors.cursor.to_linear_f32(),
                            radius: CURSOR_RADIUS * scale,
                            time_since_move: anim.move_time.elapsed().as_secs_f32(),
                            time_since_input: anim.last_input_time.elapsed().as_secs_f32(),
                            move_speed: anim.move_speed,
                            clip_top: content_y,
                            clip_bottom: content_bottom,
                        });
                        self.cursor_visible = true;
                    }
                }
            } else {
                self.cursor_anim.snap();
                self.cursor_visible = false;
            }

            // Extra bottom row: when content shifts up (scroll down), fill
            // the gap at the bottom by reading one row past the viewport
            // from the grid directly (display_iter doesn't include it).
            if pixel_offset < -0.5 && display_offset > 0 {
                let grid = term.grid();
                let grid_line = rows as i32 - display_offset as i32;
                let y_base = content_y + rows as f32 * pch + pixel_offset;
                let row_data = &grid[Line(grid_line)];

                for col_idx in 0..cols {
                    let cell = &row_data[Column(col_idx)];
                    let flags = cell.flags;

                    let (fg_color, bg_color) = if flags.contains(Flags::INVERSE) {
                        (cell.bg, cell.fg)
                    } else {
                        (cell.fg, cell.bg)
                    };

                    let extra_point = Point::new(Line(grid_line), Column(col_idx));
                    let selected = selection_range.is_some_and(|r| r.contains(extra_point));

                    if selected {
                        let quad = Rect {
                            x: content_x + col_idx as f32 * pcw,
                            y: y_base,
                            width: pcw,
                            height: pch,
                        };
                        if let Some(clipped) = quad.clip_y(content_y, content_bottom) {
                            ctx.flat_quad(clipped, selection_color);
                        }
                    } else if !colors.is_default_bg(bg_color) {
                        let quad = Rect {
                            x: content_x + col_idx as f32 * pcw,
                            y: y_base,
                            width: pcw,
                            height: pch,
                        };
                        if let Some(clipped) = quad.clip_y(content_y, content_bottom) {
                            ctx.flat_quad(clipped, colors.to_rgba(bg_color));
                        }
                    }

                    let is_invisible = flags.intersects(
                        Flags::WIDE_CHAR_SPACER
                            | Flags::LEADING_WIDE_CHAR_SPACER
                            | Flags::HIDDEN,
                    );
                    let c = if is_invisible { ' ' } else { cell.c };
                    if c == ' ' || c == '\0' {
                        continue;
                    }

                    let dim = flags.contains(Flags::DIM);
                    let fg = if dim {
                        let base = colors.to_glyphon_fg(fg_color);
                        GlyphonColor::rgba(
                            base.r() / 2,
                            base.g() / 2,
                            base.b() / 2,
                            base.a(),
                        )
                    } else {
                        colors.to_glyphon_fg(fg_color)
                    };

                    let cx = content_x + col_idx as f32 * pcw;

                    if let Some(rects) = block_char_rects(c, cx, y_base, pcw, pch) {
                        let color = glyphon_to_linear(fg);
                        for r in rects {
                            if let Some(clipped) = r.clip_y(content_y, content_bottom) {
                                ctx.flat_quad(clipped, color);
                            }
                        }
                        continue;
                    }

                    let bold = flags.contains(Flags::BOLD);
                    let italic = flags.contains(Flags::ITALIC);
                    let key = CharKey { ch: c, bold, italic };
                    let buf_idx = get_or_create_buffer(
                        &mut self.char_buffers,
                        &mut self.char_key_map,
                        key,
                        font_system,
                        metrics,
                        cell_metrics,
                    );

                    text_specs.push(TextSpec {
                        left: cx,
                        top: y_base,
                        color: fg,
                        buffer_index: buf_idx,
                        bounds: content_clip,
                    });
                }
            }
        }
    }

    pub fn buffers(&self) -> &[Buffer] {
        &self.char_buffers
    }

    pub fn write_to_pty(&self, data: Vec<u8>) {
        self.backend.send_input(Cow::Owned(data));
    }
}

fn get_or_create_buffer(
    char_buffers: &mut Vec<Buffer>,
    char_key_map: &mut HashMap<CharKey, usize>,
    key: CharKey,
    font_system: &mut FontSystem,
    metrics: glyphon::Metrics,
    cell_metrics: &CellMetrics,
) -> usize {
    if let Some(&idx) = char_key_map.get(&key) {
        return idx;
    }
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_size(
        font_system,
        Some(cell_metrics.width * 2.0),
        Some(cell_metrics.height),
    );
    let mut attrs = font::default_attrs();
    if key.bold {
        attrs = attrs.weight(glyphon::Weight::BOLD);
    }
    if key.italic {
        attrs = attrs.style(glyphon::Style::Italic);
    }
    buf.set_text(font_system, &key.ch.to_string(), attrs, Shaping::Advanced);
    buf.shape_until_scroll(font_system, false);
    let idx = char_buffers.len();
    char_buffers.push(buf);
    char_key_map.insert(key, idx);
    idx
}

/// Convert a GlyphonColor (sRGB u8) to linear f32 RGBA for GPU quads.
fn glyphon_to_linear(c: GlyphonColor) -> [f32; 4] {
    crate::colors::rgba_u8_to_linear(c.r(), c.g(), c.b(), c.a())
}

/// Return sub-rectangles for block-drawing characters (U+2580-U+259F).
fn block_char_rects(c: char, cx: f32, cy: f32, cw: f32, ch: f32) -> Option<Vec<Rect>> {
    let u = c as u32;
    let hw = cw / 2.0;
    let hh = ch / 2.0;
    let r = |x, y, w, h| Rect { x, y, width: w, height: h };

    match u {
        // Lower N/8 blocks: U+2581 (1/8) .. U+2588 (full)
        0x2581..=0x2588 => {
            let n = (u - 0x2580) as f32 / 8.0;
            Some(vec![r(cx, cy + ch * (1.0 - n), cw, ch * n)])
        }
        // Left N/8 blocks: U+2589 (7/8) .. U+258F (1/8)
        0x2589..=0x258F => {
            let n = (0x2590 - u) as f32 / 8.0;
            Some(vec![r(cx, cy, cw * n, ch)])
        }
        // Special halves and eighths
        0x2580 => Some(vec![r(cx, cy, cw, hh)]),             // upper half
        0x2590 => Some(vec![r(cx + hw, cy, hw, ch)]),         // right half
        0x2594 => Some(vec![r(cx, cy, cw, ch / 8.0)]),        // upper 1/8
        0x2595 => Some(vec![r(cx + cw * 7.0 / 8.0, cy, cw / 8.0, ch)]), // right 1/8
        // Quadrant characters: U+2596-U+259F — bitmask: TL=1, TR=2, BL=4, BR=8
        0x2596..=0x259F => {
            const QUAD_BITS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];
            let bits = QUAD_BITS[(u - 0x2596) as usize];
            let mut rects = Vec::with_capacity(4);
            if bits & 1 != 0 { rects.push(r(cx, cy, hw, hh)); }
            if bits & 2 != 0 { rects.push(r(cx + hw, cy, hw, hh)); }
            if bits & 4 != 0 { rects.push(r(cx, cy + hh, hw, hh)); }
            if bits & 8 != 0 { rects.push(r(cx + hw, cy + hh, hw, hh)); }
            Some(rects)
        }
        _ => None,
    }
}
