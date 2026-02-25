use std::borrow::Cow;
use std::sync::Arc;

use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::Term;
use russh::keys::ssh_key;
use tokio::sync::mpsc;

use crate::terminal::{EventProxy, TermSize};

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
    /// Password authentication.
    Password(String),
    /// Public key authentication with an optional passphrase.
    Key { path: String, passphrase: Option<String> },
    /// Try keys from the SSH agent, then fall back to default key files.
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
        _server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Phase 1: TOFU — accept all host keys.
        Ok(true)
    }
}

/// Spawn an OS thread that runs a single-threaded tokio runtime for the SSH session.
///
/// Returns `(term, sender)` — the `term` is shared with the rendering pipeline,
/// and `sender` is used by `Terminal` to forward input/resize/shutdown.
pub fn spawn_ssh_thread(
    config: SshConfig,
    size: TermSize,
    event_proxy: EventProxy,
) -> (Arc<FairMutex<Term<EventProxy>>>, mpsc::UnboundedSender<SshMsg>) {
    let term_config = Config::default();
    let term = Term::new(term_config, &size, event_proxy.clone());
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
                if let Err(e) = ssh_session(config, term_clone, event_proxy, rx, cols, rows).await {
                    log::error!("SSH session error: {e}");
                }
            });
        })
        .expect("spawn ssh thread");

    (term, tx)
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

    let mut session = russh::client::connect(russh_config, &addr, SshHandler)
        .await
        .map_err(|e| {
            event_proxy.send_event(Event::Exit);
            e
        })?;

    // Authenticate
    let authenticated = match &config.auth {
        SshAuth::Password(password) => {
            session
                .authenticate_password(&config.username, password)
                .await?
        }
        SshAuth::Key { path, passphrase } => {
            let key_path = shellexpand_path(path);
            let key_pair = if let Some(pp) = passphrase {
                russh_keys::load_secret_key(&key_path, Some(pp))?
            } else {
                russh_keys::load_secret_key(&key_path, None)?
            };
            session
                .authenticate_publickey(&config.username, Arc::new(key_pair))
                .await?
        }
        SshAuth::Agent => {
            let authenticated = try_agent_auth(&mut session, &config.username).await;
            if authenticated {
                true
            } else {
                // Fall back to default key files
                try_default_keys(&mut session, &config.username).await
            }
        }
    };

    if !authenticated {
        log::error!("SSH authentication failed for {}@{}", config.username, config.host);
        // Write error to terminal so user sees it
        let msg = format!("\r\nAuthentication failed for {}@{}\r\n", config.username, config.host);
        {
            let mut parser = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
            let mut t = term.lock();
            parser.advance(&mut *t, msg.as_bytes());
        }
        event_proxy.send_event(Event::Exit);
        return Ok(());
    }

    // Open a session channel
    let mut channel = session.channel_open_session().await?;

    // Request a PTY
    channel
        .request_pty(
            false,
            "xterm-256color",
            cols as u32,
            rows as u32,
            0,
            0,
            &[],
        )
        .await?;

    // Request shell
    channel.request_shell(false).await?;

    // Set title
    event_proxy.send_event(Event::Title(format!(
        "{}@{}",
        config.username, config.host
    )));

    // Main I/O loop
    let mut parser = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();

    loop {
        tokio::select! {
            // Data from SSH server → terminal
            msg = channel.wait() => {
                match msg {
                    Some(russh::ChannelMsg::Data { data }) => {
                        {
                            let mut t = term.lock();
                            parser.advance(&mut *t, &data);
                        }
                        event_proxy.send_event(Event::Wakeup);
                    }
                    Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                        {
                            let mut t = term.lock();
                            parser.advance(&mut *t, &data);
                        }
                        event_proxy.send_event(Event::Wakeup);
                    }
                    Some(russh::ChannelMsg::ExitStatus { .. })
                    | Some(russh::ChannelMsg::ExitSignal { .. })
                    | Some(russh::ChannelMsg::Eof) => {
                        // Channel closed
                        break;
                    }
                    None => {
                        // Channel stream ended
                        break;
                    }
                    _ => {}
                }
            }
            // Input from UI → SSH channel
            msg = rx.recv() => {
                match msg {
                    Some(SshMsg::Input(data)) => {
                        channel.data(&*data).await?;
                    }
                    Some(SshMsg::Resize { cols, rows }) => {
                        channel.window_change(cols as u32, rows as u32, 0, 0).await?;
                    }
                    None => {
                        break;
                    }
                }
            }
        }
    }

    // Clean shutdown
    let _ = channel.eof().await;
    let _ = channel.close().await;
    event_proxy.send_event(Event::Exit);
    Ok(())
}

async fn try_agent_auth(
    session: &mut russh::client::Handle<SshHandler>,
    username: &str,
) -> bool {
    #[cfg(unix)]
    {
        use russh_keys::agent::client::AgentClient;
        if let Ok(stream) = tokio::net::UnixStream::connect(
            std::env::var("SSH_AUTH_SOCK").unwrap_or_default(),
        ).await {
            let mut agent = AgentClient::connect(stream);
            if let Ok(identities) = agent.request_identities().await {
                for identity in identities {
                    if let Ok(true) = session
                        .authenticate_publickey_with(username, identity, &mut agent)
                        .await
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

async fn try_default_keys(
    session: &mut russh::client::Handle<SshHandler>,
    username: &str,
) -> bool {
    let ssh_dir = dirs::home_dir()
        .map(|h| h.join(".ssh"))
        .unwrap_or_default();

    let key_names = ["id_ed25519", "id_rsa", "id_ecdsa"];

    for name in &key_names {
        let path = ssh_dir.join(name);
        if !path.exists() {
            continue;
        }
        match russh_keys::load_secret_key(&path, None) {
            Ok(key) => {
                match session
                    .authenticate_publickey(username, Arc::new(key))
                    .await
                {
                    Ok(true) => return true,
                    Ok(false) => continue,
                    Err(e) => {
                        log::debug!("key auth failed for {}: {e}", path.display());
                        continue;
                    }
                }
            }
            Err(e) => {
                log::debug!("failed to load key {}: {e}", path.display());
                continue;
            }
        }
    }
    false
}

/// Expand `~` in paths.
fn shellexpand_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(path)
}
