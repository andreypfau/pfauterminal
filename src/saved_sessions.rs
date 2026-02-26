use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: SavedAuthType,
    #[serde(default)]
    pub key_path: Option<String>,
    pub last_used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SavedAuthType {
    Password,
    Key,
    Agent,
}

impl SavedSession {
    pub fn display_label(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, self.port)
        }
    }

    pub fn key(&self) -> String {
        format!("{}@{}:{}", self.username, self.host, self.port)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedSessions {
    pub sessions: Vec<SavedSession>,
}

impl SavedSessions {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pfauterminal").join("sessions.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => {
                let mut sessions: Self = serde_json::from_str(&json).unwrap_or_default();
                sessions.sort();
                sessions
            }
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn upsert(&mut self, session: SavedSession) {
        let key = session.key();
        if let Some(existing) = self.sessions.iter_mut().find(|s| s.key() == key) {
            existing.last_used = session.last_used;
            existing.auth_type = session.auth_type;
            existing.key_path = session.key_path;
        } else {
            self.sessions.push(session);
        }
        self.sort();
        self.save();
    }

    pub fn remove_by_key(&mut self, key: &str) {
        self.sessions.retain(|s| s.key() != key);
        self.save();
    }

    pub fn find_by_key(&self, key: &str) -> Option<&SavedSession> {
        self.sessions.iter().find(|s| s.key() == key)
    }

    pub fn touch_by_key(&mut self, key: &str) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.key() == key) {
            session.last_used = now_unix();
        }
        self.sort();
        self.save();
    }

    fn sort(&mut self) {
        self.sessions.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
