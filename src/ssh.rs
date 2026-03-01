use std::borrow::Cow;
use std::sync::Arc;

use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi;
use alacritty_terminal::Term;
use tokio::sync::mpsc;

use crate::terminal_panel::{EventProxy, TermSize};

/// SSH connection configuration.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
}

/// SSH authentication method.
#[derive(Debug, Clone)]
pub enum SshAuth {
    Password(String),
    Key { path: String, passphrase: Option<String> },
    Agent,
}

/// Messages sent from the UI thread to the SSH I/O thread.
pub enum SshMsg {
    Input(Cow<'static, [u8]>),
    Resize { cols: u16, rows: u16 },
}

struct SshHandler;

#[async_trait::async_trait]
impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true) // TOFU — accept all host keys
    }
}

/// Spawn an OS thread running a tokio runtime for the SSH session.
pub fn spawn_ssh_thread(
    config: SshConfig,
    size: TermSize,
    event_proxy: EventProxy,
) -> (Arc<FairMutex<Term<EventProxy>>>, mpsc::UnboundedSender<SshMsg>) {
    let term = Term::new(Config::default(), &size, event_proxy.clone());
    let term = Arc::new(FairMutex::new(term));
    let (tx, rx) = mpsc::unbounded_channel();

    let term_clone = term.clone();
    let cols = size.columns as u16;
    let rows = size.screen_lines as u16;

    std::thread::Builder::new()
        .name("ssh-io".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                if let Err(e) = ssh_session(config, term_clone.clone(), event_proxy.clone(), rx, cols, rows).await {
                    use alacritty_terminal::event::EventListener;
                    log::error!("SSH session error: {e}");
                    write_to_term(&term_clone, &format!(
                        "\x1b[?25l\r\n\x1b[31mSSH error: {e}\x1b[0m\r\n"
                    ));
                    event_proxy.send_event(alacritty_terminal::event::Event::Wakeup);
                }
            });
        })
        .expect("spawn ssh thread");

    (term, tx)
}

/// Write a message to the terminal emulator (for displaying errors to the user).
fn write_to_term(term: &FairMutex<Term<EventProxy>>, msg: &str) {
    let mut parser = ansi::Processor::<ansi::StdSyncHandler>::new();
    let mut t = term.lock();
    parser.advance(&mut *t, msg.as_bytes());
}

async fn ssh_session(
    config: SshConfig,
    term: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
    mut rx: mpsc::UnboundedReceiver<SshMsg>,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use alacritty_terminal::event::Event;
    use alacritty_terminal::event::EventListener;

    let russh_config = Arc::new(russh::client::Config::default());
    let addr = format!("{}:{}", config.host, config.port);

    // Hide cursor during connection — it will be re-enabled by the remote shell
    write_to_term(&term, &format!("\x1b[?25lConnecting to {addr}...\r\n"));
    event_proxy.send_event(Event::Wakeup);

    let mut session = russh::client::connect(russh_config, &addr, SshHandler)
        .await?;

    // Authenticate
    write_to_term(&term, &format!("Authenticating as {}...\r\n", config.username));
    event_proxy.send_event(Event::Wakeup);

    let auth_result = match &config.auth {
        SshAuth::Password(password) => {
            match session.authenticate_password(&config.username, password).await {
                Ok(true) => Ok(()),
                Ok(false) => Err("server rejected password".to_string()),
                Err(e) => Err(format!("password auth error: {e}")),
            }
        }
        SshAuth::Key { path, passphrase } => {
            let key_path = shellexpand_path(path);
            match russh_keys::load_secret_key(&key_path, passphrase.as_deref()) {
                Ok(key_pair) => {
                    match session.authenticate_publickey(&config.username, Arc::new(key_pair)).await {
                        Ok(true) => Ok(()),
                        Ok(false) => Err(format!("server rejected key {}", key_path.display())),
                        Err(e) => Err(format!("public key auth error: {e}")),
                    }
                }
                Err(e) => Err(format!("failed to load key {}: {e}", key_path.display())),
            }
        }
        SshAuth::Agent => {
            let mut reasons = Vec::new();
            let ok = try_agent_auth(&mut session, &config.username, &mut reasons).await
                || try_default_keys(&mut session, &config.username, &mut reasons).await;
            if ok {
                Ok(())
            } else {
                if reasons.is_empty() {
                    reasons.push("no keys available and agent not reachable".to_string());
                }
                Err(reasons.join("\r\n  "))
            }
        }
    };

    if let Err(reason) = auth_result {
        log::error!("SSH authentication failed for {}@{}: {reason}", config.username, config.host);
        // Hide cursor + show error
        write_to_term(&term, &format!(
            "\x1b[?25l\r\n\x1b[31mAuthentication failed for {}@{}\r\n  {reason}\x1b[0m\r\n",
            config.username, config.host
        ));
        event_proxy.send_event(Event::Wakeup);
        return Ok(());
    }

    // Open channel, request PTY and shell
    let mut channel = session.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(false).await?;

    // Re-enable cursor now that the remote shell is ready
    write_to_term(&term, "\x1b[?25h");

    event_proxy.send_event(Event::Title(format!(
        "{}@{}",
        config.username, config.host
    )));

    // Main I/O loop
    let mut parser = ansi::Processor::<ansi::StdSyncHandler>::new();

    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(russh::ChannelMsg::Data { data }
                        | russh::ChannelMsg::ExtendedData { data, .. }) => {
                        {
                            let mut t = term.lock();
                            parser.advance(&mut *t, &data);
                        }
                        event_proxy.send_event(Event::Wakeup);
                    }
                    Some(russh::ChannelMsg::ExitStatus { .. })
                    | Some(russh::ChannelMsg::ExitSignal { .. })
                    | Some(russh::ChannelMsg::Eof)
                    | None => break,
                    _ => {}
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(SshMsg::Input(data)) => {
                        channel.data(&*data).await?;
                    }
                    Some(SshMsg::Resize { cols, rows }) => {
                        channel.window_change(cols as u32, rows as u32, 0, 0).await?;
                    }
                    None => break,
                }
            }
        }
    }

    let _ = channel.eof().await;
    let _ = channel.close().await;
    event_proxy.send_event(Event::Exit);
    Ok(())
}

async fn try_agent_auth(
    session: &mut russh::client::Handle<SshHandler>,
    username: &str,
    reasons: &mut Vec<String>,
) -> bool {
    #[cfg(unix)]
    {
        use russh_keys::agent::client::AgentClient;
        let sock = match std::env::var("SSH_AUTH_SOCK") {
            Ok(s) if !s.is_empty() => s,
            _ => {
                reasons.push("SSH_AUTH_SOCK not set — agent not available".to_string());
                return false;
            }
        };
        match tokio::net::UnixStream::connect(&sock).await {
            Ok(stream) => {
                let mut agent = AgentClient::connect(stream);
                match agent.request_identities().await {
                    Ok(identities) => {
                        if identities.is_empty() {
                            reasons.push("agent has no identities".to_string());
                        }
                        for identity in identities {
                            match session
                                .authenticate_publickey_with(username, identity, &mut agent)
                                .await
                            {
                                Ok(true) => return true,
                                Ok(false) => {
                                    reasons.push("agent key rejected by server".to_string());
                                }
                                Err(e) => {
                                    reasons.push(format!("agent key auth error: {e}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        reasons.push(format!("agent request_identities failed: {e}"));
                    }
                }
            }
            Err(e) => {
                reasons.push(format!("cannot connect to SSH agent at {sock}: {e}"));
            }
        }
    }
    #[cfg(windows)]
    {
        use russh_keys::agent::client::AgentClient;
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe_path = r"\\.\pipe\openssh-ssh-agent";
        match ClientOptions::new().open(pipe_path) {
            Ok(pipe) => {
                let mut agent = AgentClient::connect(pipe);
                match agent.request_identities().await {
                    Ok(identities) => {
                        if identities.is_empty() {
                            reasons.push("agent has no identities".to_string());
                        }
                        for identity in identities {
                            match session
                                .authenticate_publickey_with(username, identity, &mut agent)
                                .await
                            {
                                Ok(true) => return true,
                                Ok(false) => {
                                    reasons.push("agent key rejected by server".to_string());
                                }
                                Err(e) => {
                                    reasons.push(format!("agent key auth error: {e}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        reasons.push(format!("agent request_identities failed: {e}"));
                    }
                }
            }
            Err(e) => {
                reasons.push(format!(
                    "cannot connect to SSH agent at {pipe_path}: {e} (is the OpenSSH Authentication Agent service running?)"
                ));
            }
        }
    }
    false
}

async fn try_default_keys(
    session: &mut russh::client::Handle<SshHandler>,
    username: &str,
    reasons: &mut Vec<String>,
) -> bool {
    let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        reasons.push("cannot determine home directory for default keys".to_string());
        return false;
    };

    let mut found_any = false;
    for name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let path = ssh_dir.join(name);
        if !path.exists() {
            continue;
        }
        found_any = true;
        let key = match russh_keys::load_secret_key(&path, None) {
            Ok(k) => k,
            Err(e) => {
                reasons.push(format!("failed to load {}: {e}", path.display()));
                continue;
            }
        };
        match session.authenticate_publickey(username, Arc::new(key)).await {
            Ok(true) => return true,
            Ok(false) => {
                reasons.push(format!("{} rejected by server", path.display()));
            }
            Err(e) => {
                reasons.push(format!("{} auth error: {e}", path.display()));
            }
        }
    }
    if !found_any {
        reasons.push(format!("no default keys found in {}", ssh_dir.display()));
    }
    false
}

fn shellexpand_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    std::path::PathBuf::from(path)
}
