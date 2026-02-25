use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::tty;
use alacritty_terminal::Term;
use tokio::sync::mpsc;
use winit::event_loop::EventLoopProxy;

use crate::ssh::{SshConfig, SshMsg};
use crate::terminal_panel::PanelId;

/// Custom event sent from the terminal I/O thread to the winit event loop.
#[derive(Debug)]
pub enum TerminalEvent {
    Wakeup,
    Title(PanelId, String),
    Exit(PanelId),
    /// Deferred SSH dialog close — carries an optional result (None = cancelled).
    SshDialogClose(Option<crate::ssh_dialog::SshResult>),
}

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
                if let Ok(guard) = self.backend.lock() {
                    if let Some(backend) = guard.as_ref() {
                        backend.send_input(Cow::Owned(text.into_bytes()));
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

/// Wraps alacritty_terminal's Term with either a local PTY or SSH backend.
pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    backend: Backend,
}

impl Terminal {
    /// Spawn a new local terminal with the given size, event proxy, and optional shell program.
    pub fn new(
        size: TermSize,
        cell_width: u16,
        cell_height: u16,
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
            cell_width,
            cell_height,
        };

        let pty = tty::new(&pty_config, window_size, 0).expect("failed to create PTY");

        let event_loop = EventLoop::new(term.clone(), event_proxy.clone(), pty, false, false)
            .expect("failed to create event loop");

        let channel = event_loop.channel();

        if let Ok(mut guard) = event_proxy.backend.lock() {
            *guard = Some(Backend::Local(channel.clone()));
        }

        event_loop.spawn();

        Self {
            term,
            backend: Backend::Local(channel),
        }
    }

    /// Create a terminal backed by a native SSH connection.
    pub fn new_ssh(size: TermSize, event_proxy: EventProxy, ssh_config: SshConfig) -> Self {
        let (term, sender) = crate::ssh::spawn_ssh_thread(ssh_config, size, event_proxy.clone());

        if let Ok(mut guard) = event_proxy.backend.lock() {
            *guard = Some(Backend::Ssh(sender.clone()));
        }

        Self {
            term,
            backend: Backend::Ssh(sender),
        }
    }

    /// Send raw bytes (keyboard input) to the backend.
    pub fn write(&self, data: impl Into<Cow<'static, [u8]>>) {
        self.backend.send_input(data.into());
    }

    /// Resize the terminal grid and notify the backend.
    pub fn resize(&self, size: TermSize, cell_width: u16, cell_height: u16) {
        self.backend.send_resize(size, cell_width, cell_height);
        self.term.lock().resize(size);
    }

    /// Scroll the viewport by the given number of lines (positive = up).
    pub fn scroll(&self, lines: i32) {
        let mut term = self.term.lock();
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
    }
}
