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
                if let Err(e) = ssh_session(config, term_clone, event_proxy, rx, cols, rows).await {
                    log::error!("SSH session error: {e}");
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

    let mut session = russh::client::connect(russh_config, &addr, SshHandler)
        .await
        .inspect_err(|_| {
            event_proxy.send_event(Event::Exit);
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
            let key_pair = russh_keys::load_secret_key(&key_path, passphrase.as_deref())?;
            session
                .authenticate_publickey(&config.username, Arc::new(key_pair))
                .await?
        }
        SshAuth::Agent => {
            try_agent_auth(&mut session, &config.username).await
                || try_default_keys(&mut session, &config.username).await
        }
    };

    if !authenticated {
        log::error!("SSH authentication failed for {}@{}", config.username, config.host);
        write_to_term(&term, &format!(
            "\r\nAuthentication failed for {}@{}\r\n",
            config.username, config.host
        ));
        event_proxy.send_event(Event::Exit);
        return Ok(());
    }

    // Open channel, request PTY and shell
    let mut channel = session.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(false).await?;

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
) -> bool {
    #[cfg(unix)]
    {
        use russh_keys::agent::client::AgentClient;
        let sock = match std::env::var("SSH_AUTH_SOCK") {
            Ok(s) if !s.is_empty() => s,
            _ => return false,
        };
        if let Ok(stream) = tokio::net::UnixStream::connect(&sock).await {
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
    let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        return false;
    };

    for name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let path = ssh_dir.join(name);
        if !path.exists() {
            continue;
        }
        let key = match russh_keys::load_secret_key(&path, None) {
            Ok(k) => k,
            Err(e) => {
                log::debug!("failed to load key {}: {e}", path.display());
                continue;
            }
        };
        match session.authenticate_publickey(username, Arc::new(key)).await {
            Ok(true) => return true,
            Ok(false) => continue,
            Err(e) => {
                log::debug!("key auth failed for {}: {e}", path.display());
                continue;
            }
        }
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
