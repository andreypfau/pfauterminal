use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::tty;
use alacritty_terminal::Term;
use winit::event_loop::EventLoopProxy;

use crate::panel::PanelId;

/// Custom event sent from the terminal I/O thread to the winit event loop.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Wakeup,
    Title(PanelId, String),
    Exit(PanelId),
}

/// Bridges alacritty's EventListener to winit's EventLoopProxy.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<TerminalEvent>,
    panel_id: PanelId,
    pty_writer: Arc<Mutex<Option<EventLoopSender>>>,
}

impl EventProxy {
    pub fn new(
        proxy: EventLoopProxy<TerminalEvent>,
        panel_id: PanelId,
        pty_writer: Arc<Mutex<Option<EventLoopSender>>>,
    ) -> Self {
        Self {
            proxy,
            panel_id,
            pty_writer,
        }
    }

    #[allow(dead_code)]
    pub fn raw_proxy(&self) -> &EventLoopProxy<TerminalEvent> {
        &self.proxy
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
                if let Ok(guard) = self.pty_writer.lock() {
                    if let Some(sender) = guard.as_ref() {
                        let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        };
    }
}

/// Terminal dimensions.
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

/// Wraps alacritty_terminal's PTY, parser, event loop, and Term.
pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    channel: EventLoopSender,
}

impl Terminal {
    /// Spawn a new terminal with the given size, event proxy, and optional shell program.
    /// If `shell` is None, the system default shell is used.
    pub fn new(
        size: TermSize,
        cell_width: u16,
        cell_height: u16,
        event_proxy: EventProxy,
        shell: Option<String>,
    ) -> Self {
        let config = Config::default();
        let term = Term::new(config, &size, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_config = tty::Options {
            shell: shell.map(|program| tty::Shell::new(program, Vec::new())),
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
            cell_width,
            cell_height,
        };

        let pty = tty::new(&pty_config, window_size, 0).expect("failed to create PTY");

        let event_loop = EventLoop::new(term.clone(), event_proxy.clone(), pty, false, false)
            .expect("failed to create event loop");

        let channel = event_loop.channel();

        // Wire up the PTY writer so EventProxy can send responses back to the PTY.
        if let Ok(mut guard) = event_proxy.pty_writer.lock() {
            *guard = Some(channel.clone());
        }

        event_loop.spawn();

        Self { term, channel }
    }

    /// Send raw bytes (keyboard input) to the PTY.
    pub fn write(&self, data: impl Into<Cow<'static, [u8]>>) {
        let _ = self.channel.send(Msg::Input(data.into()));
    }

    /// Resize the terminal grid and PTY.
    pub fn resize(&self, size: TermSize, cell_width: u16, cell_height: u16) {
        let window_size = WindowSize {
            num_lines: size.screen_lines as u16,
            num_cols: size.columns as u16,
            cell_width,
            cell_height,
        };
        let _ = self.channel.send(Msg::Resize(window_size));
        self.term.lock().resize(size);
    }

    /// Scroll the viewport by the given number of lines (positive = up).
    pub fn scroll(&self, lines: i32) {
        let mut term = self.term.lock();
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
    }
}
