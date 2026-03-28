use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SshHostEntry {
    pub alias: String,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}

impl SshHostEntry {
    pub fn effective_host(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }

    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(22)
    }

    pub fn display_label(&self) -> String {
        if let Some(user) = &self.user {
            let host = self.effective_host();
            let port = self.effective_port();
            if self.alias != host {
                if port != 22 {
                    format!("{} ({}@{}:{})", self.alias, user, host, port)
                } else {
                    format!("{} ({}@{})", self.alias, user, host)
                }
            } else if port != 22 {
                format!("{}@{}:{}", user, host, port)
            } else {
                format!("{}@{}", user, host)
            }
        } else if self.alias != self.effective_host() {
            format!("{} ({})", self.alias, self.effective_host())
        } else {
            self.alias.clone()
        }
    }
}

pub fn load_ssh_config() -> Vec<SshHostEntry> {
    let Some(path) = ssh_config_path() else {
        return Vec::new();
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_ssh_config(&content)
}

fn ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

fn parse_ssh_config(content: &str) -> Vec<SshHostEntry> {
    let mut entries = Vec::new();
    let mut current: Option<SshHostEntry> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = match line.split_once(char::is_whitespace) {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };

        match key.to_lowercase().as_str() {
            "host" => {
                if let Some(entry) = current.take() {
                    if should_include(&entry) {
                        entries.push(entry);
                    }
                }
                current = Some(SshHostEntry {
                    alias: value.to_string(),
                    hostname: None,
                    port: None,
                    user: None,
                    identity_file: None,
                });
            }
            "hostname" => {
                if let Some(ref mut entry) = current {
                    entry.hostname = Some(value.to_string());
                }
            }
            "port" => {
                if let Some(ref mut entry) = current {
                    entry.port = value.parse().ok();
                }
            }
            "user" => {
                if let Some(ref mut entry) = current {
                    entry.user = Some(value.to_string());
                }
            }
            "identityfile" => {
                if let Some(ref mut entry) = current {
                    entry.identity_file = Some(value.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(entry) = current {
        if should_include(&entry) {
            entries.push(entry);
        }
    }

    entries
}

fn should_include(entry: &SshHostEntry) -> bool {
    let alias = &entry.alias;
    alias != "*" && !alias.contains('*') && !alias.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_config() {
        let config = r#"
Host myserver
    HostName 192.168.1.100
    User admin
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host *
    ServerAliveInterval 60

Host dev
    HostName dev.example.com
    User deploy
"#;
        let entries = parse_ssh_config(config);
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].alias, "myserver");
        assert_eq!(entries[0].hostname.as_deref(), Some("192.168.1.100"));
        assert_eq!(entries[0].user.as_deref(), Some("admin"));
        assert_eq!(entries[0].port, Some(2222));

        assert_eq!(entries[1].alias, "dev");
        assert_eq!(entries[1].hostname.as_deref(), Some("dev.example.com"));
        assert_eq!(entries[1].user.as_deref(), Some("deploy"));
        assert_eq!(entries[1].port, None);
    }

    #[test]
    fn display_label_formats() {
        let entry = SshHostEntry {
            alias: "myserver".to_string(),
            hostname: Some("192.168.1.100".to_string()),
            user: Some("admin".to_string()),
            port: Some(2222),
            identity_file: None,
        };
        assert_eq!(entry.display_label(), "myserver (admin@192.168.1.100:2222)");

        let entry2 = SshHostEntry {
            alias: "example.com".to_string(),
            hostname: Some("example.com".to_string()),
            user: Some("root".to_string()),
            port: None,
            identity_file: None,
        };
        assert_eq!(entry2.display_label(), "root@example.com");
    }
}
