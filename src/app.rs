use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::colors::ColorScheme;
use crate::renderer::{CellInfo, CursorInfo, Renderer};
use crate::terminal::{EventProxy, TermSize, Terminal, TerminalEvent};

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Option<Terminal>,
    event_proxy: EventProxy,
    colors: ColorScheme,
}

impl App {
    pub fn new(event_proxy: EventProxy) -> Self {
        let colors = ColorScheme::load();
        Self {
            window: None,
            renderer: None,
            terminal: None,
            event_proxy,
            colors,
        }
    }

    fn extract_cells(&self) -> (Vec<Vec<CellInfo>>, Option<CursorInfo>) {
        let terminal = self.terminal.as_ref().unwrap();
        let renderer = self.renderer.as_ref().unwrap();
        let term = terminal.term.lock();
        let content = term.renderable_content();

        let rows = renderer.rows;
        let cols = renderer.cols;
        let default_fg = self.colors.fg_glyphon();

        // Pre-fill grid with spaces
        let mut lines: Vec<Vec<CellInfo>> = (0..rows)
            .map(|row| {
                (0..cols)
                    .map(|col| CellInfo {
                        row,
                        col,
                        c: ' ',
                        fg: default_fg,
                        bg: [0.0; 4],
                        is_default_bg: true,
                        bold: false,
                        italic: false,
                    })
                    .collect()
            })
            .collect();

        // Fill from terminal content
        for indexed in content.display_iter {
            let row = indexed.point.line.0 as usize;
            let col = indexed.point.column.0;
            if row < rows && col < cols {
                let cell = &*indexed;
                let flags = cell.flags;
                let bold = flags.contains(alacritty_terminal::term::cell::Flags::BOLD);
                let italic = flags.contains(alacritty_terminal::term::cell::Flags::ITALIC);

                // Handle INVERSE flag
                let (fg_color, bg_color) =
                    if flags.contains(alacritty_terminal::term::cell::Flags::INVERSE) {
                        (cell.bg, cell.fg)
                    } else {
                        (cell.fg, cell.bg)
                    };

                lines[row][col] = CellInfo {
                    row,
                    col,
                    c: cell.c,
                    fg: self.colors.to_glyphon_fg(fg_color),
                    bg: self.colors.to_rgba(bg_color),
                    is_default_bg: self.colors.is_default_bg(bg_color),
                    bold,
                    italic,
                };
            }
        }

        // Cursor
        let cursor = {
            let cp = content.cursor.point;
            let row = cp.line.0 as usize;
            let col = cp.column.0;
            if row < rows && col < cols {
                Some(CursorInfo { row, col })
            } else {
                None
            }
        };

        (lines, cursor)
    }

    fn redraw(&mut self) {
        if self.terminal.is_none() || self.renderer.is_none() {
            return;
        }

        let (lines, cursor) = self.extract_cells();
        let renderer = self.renderer.as_mut().unwrap();

        match renderer.render(&lines, cursor) {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let config = &renderer.surface_config;
                renderer.resize(config.width, config.height);
            }
            Err(wgpu::SurfaceError::Timeout) => {
                log::warn!("surface timeout");
            }
            Err(e) => {
                log::error!("render error: {e}");
            }
        }
    }
}

impl ApplicationHandler<TerminalEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("pfauterminal")
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let renderer = Renderer::new(window.clone(), self.colors.clone());

        let cols = renderer.cols;
        let rows = renderer.rows;
        let cell_w = (renderer.cell.width * renderer.scale_factor) as u16;
        let cell_h = (renderer.cell.height * renderer.scale_factor) as u16;

        let size = TermSize::new(cols, rows);
        let terminal = Terminal::new(size, cell_w, cell_h, self.event_proxy.clone());

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.terminal = Some(terminal);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: TerminalEvent) {
        match event {
            TerminalEvent::Wakeup => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            TerminalEvent::Title(title) => {
                if let Some(w) = &self.window {
                    w.set_title(&title);
                }
            }
            TerminalEvent::Exit => {
                log::info!("terminal exited");
                std::process::exit(0);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(new_size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(new_size.width, new_size.height);
                    let cols = renderer.cols;
                    let rows = renderer.rows;
                    let cell_w = (renderer.cell.width * renderer.scale_factor) as u16;
                    let cell_h = (renderer.cell.height * renderer.scale_factor) as u16;
                    if let Some(terminal) = &self.terminal {
                        terminal.resize(TermSize::new(cols, rows), cell_w, cell_h);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_scale_factor(scale_factor as f32);
                }
            }

            WindowEvent::RedrawRequested => {
                self.redraw();
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                if let Some(terminal) = &self.terminal {
                    let bytes: Option<Vec<u8>> = match logical_key.as_ref() {
                        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
                        Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
                        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
                        Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
                        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
                        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
                        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
                        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
                        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
                        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
                        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
                        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
                        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
                        Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
                        Key::Character(c) => {
                            // Handle Ctrl+key
                            if c.len() == 1 {
                                let ch = c.chars().next().unwrap();
                                if ch.is_ascii_lowercase() && text.is_none() {
                                    // Ctrl+letter: 0x01..0x1a
                                    Some(vec![ch as u8 - b'a' + 1])
                                } else {
                                    None // Fall through to text handling
                                }
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };

                    if let Some(b) = bytes {
                        terminal.write(b);
                    } else if let Some(t) = text {
                        let s: String = t.to_string();
                        if !s.is_empty() {
                            terminal.write(s.into_bytes());
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(terminal) = &self.terminal {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as i32,
                        MouseScrollDelta::PixelDelta(pos) => {
                            (pos.y / self.renderer.as_ref().unwrap().cell.height as f64) as i32
                        }
                    };
                    if lines != 0 {
                        terminal.scroll(lines);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}
