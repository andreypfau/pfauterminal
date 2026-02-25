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

/// Generalizes the write-back path used by `EventProxy`:
/// either a local PTY sender or an SSH channel sender.
enum PtyWriter {
    Local(EventLoopSender),
    Ssh(mpsc::UnboundedSender<SshMsg>),
}

/// Bridges alacritty's EventListener to winit's EventLoopProxy.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<TerminalEvent>,
    panel_id: PanelId,
    pty_writer: Arc<Mutex<Option<PtyWriter>>>,
}

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<TerminalEvent>, panel_id: PanelId) -> Self {
        Self {
            proxy,
            panel_id,
            pty_writer: Arc::new(Mutex::new(None)),
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
                if let Ok(guard) = self.pty_writer.lock() {
                    match guard.as_ref() {
                        Some(PtyWriter::Local(sender)) => {
                            let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
                        }
                        Some(PtyWriter::Ssh(sender)) => {
                            let _ = sender.send(SshMsg::Input(Cow::Owned(text.into_bytes())));
                        }
                        None => {}
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

/// I/O backend: either a local PTY or an SSH channel.
enum TerminalBackend {
    Local {
        channel: EventLoopSender,
    },
    Ssh {
        sender: mpsc::UnboundedSender<SshMsg>,
    },
}

/// Wraps alacritty_terminal's Term with either a local PTY or SSH backend.
pub struct Terminal {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    backend: TerminalBackend,
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

        // Wire up the PTY writer so EventProxy can send responses back.
        if let Ok(mut guard) = event_proxy.pty_writer.lock() {
            *guard = Some(PtyWriter::Local(channel.clone()));
        }

        event_loop.spawn();

        Self {
            term,
            backend: TerminalBackend::Local { channel },
        }
    }

    /// Create a terminal backed by a native SSH connection.
    pub fn new_ssh(
        size: TermSize,
        cell_width: u16,
        cell_height: u16,
        event_proxy: EventProxy,
        ssh_config: SshConfig,
    ) -> Self {
        let _ = (cell_width, cell_height); // not needed for SSH PTY creation

        let (term, sender) = crate::ssh::spawn_ssh_thread(ssh_config, size, event_proxy.clone());

        // Wire up the PTY writer so EventProxy can route PtyWrite events to SSH.
        if let Ok(mut guard) = event_proxy.pty_writer.lock() {
            *guard = Some(PtyWriter::Ssh(sender.clone()));
        }

        Self {
            term,
            backend: TerminalBackend::Ssh { sender },
        }
    }

    /// Send raw bytes (keyboard input) to the backend.
    pub fn write(&self, data: impl Into<Cow<'static, [u8]>>) {
        match &self.backend {
            TerminalBackend::Local { channel } => {
                let _ = channel.send(Msg::Input(data.into()));
            }
            TerminalBackend::Ssh { sender } => {
                let _ = sender.send(SshMsg::Input(data.into()));
            }
        }
    }

    /// Resize the terminal grid and notify the backend.
    pub fn resize(&self, size: TermSize, cell_width: u16, cell_height: u16) {
        match &self.backend {
            TerminalBackend::Local { channel } => {
                let window_size = WindowSize {
                    num_lines: size.screen_lines as u16,
                    num_cols: size.columns as u16,
                    cell_width,
                    cell_height,
                };
                let _ = channel.send(Msg::Resize(window_size));
            }
            TerminalBackend::Ssh { sender } => {
                let _ = sender.send(SshMsg::Resize {
                    cols: size.columns as u16,
                    rows: size.screen_lines as u16,
                });
            }
        }
        self.term.lock().resize(size);
    }

    /// Scroll the viewport by the given number of lines (positive = up).
    pub fn scroll(&self, lines: i32) {
        let mut term = self.term.lock();
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
    }
}
