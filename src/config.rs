//! The tool's own settings: language, plus names for account / organization uuids.
//!
//! The desktop app only names its directories by uuid. Names come from two places:
//! ones this tool detected (see [`crate::names`]) and aliases the user typed, which win.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::store::OrgKey;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// `"zh"` for Chinese; anything else is English.
    #[serde(default)]
    pub language: String,
    /// uuid (account or organization) -> alias typed by the user
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// uuid -> detected account e-mail / organization name, remembered across runs
    /// because the desktop app only caches them for the account currently signed in.
    #[serde(default)]
    pub detected: BTreeMap<String, String>,
    #[serde(default)]
    pub show_archived: bool,
    #[serde(skip)]
    path: Option<PathBuf>,
}

/// Where this tool keeps its settings and backups.
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("claude-code-ln"))
}

pub fn short(uuid: &str) -> &str {
    uuid.get(..8).unwrap_or(uuid)
}

/// What the Claude Code CLI knows about the account it is logged in to.
fn cli_login_names() -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    let Some(home) = dirs::home_dir() else { return names };
    let Ok(bytes) = fs::read(home.join(".claude.json")) else { return names };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return names };
    let Some(account) = value.get("oauthAccount") else { return names };
    let field = |k: &str| account.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    for (id, name) in [("organizationUuid", "organizationName"), ("accountUuid", "emailAddress")] {
        if let (Some(uuid), Some(name)) = (field(id), field(name)) {
            names.insert(uuid.to_owned(), name.to_owned());
        }
    }
    names
}

impl Config {
    pub fn load() -> Self {
        let path = data_dir().map(|d| d.join("config.json"));
        let mut config: Config = path
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        config.path = path;
        config.learn(cli_login_names());
        config
    }

    /// Remembers detected names; returns whether anything changed.
    pub fn learn(&mut self, names: BTreeMap<String, String>) -> bool {
        let mut changed = false;
        for (uuid, name) in names {
            if self.detected.get(&uuid) != Some(&name) {
                self.detected.insert(uuid, name);
                changed = true;
            }
        }
        changed
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn alias(&self, uuid: &str) -> Option<&str> {
        self.aliases.get(uuid).map(String::as_str).filter(|a| !a.trim().is_empty())
    }

    pub fn detected(&self, uuid: &str) -> Option<&str> {
        self.detected.get(uuid).map(String::as_str)
    }

    pub fn set_alias(&mut self, uuid: &str, alias: &str) {
        let alias = alias.trim();
        if alias.is_empty() {
            self.aliases.remove(uuid);
        } else {
            self.aliases.insert(uuid.to_owned(), alias.to_owned());
        }
    }

    /// Alias, else detected name, else the first 8 characters of the uuid.
    pub fn name(&self, uuid: &str) -> String {
        self.alias(uuid).or_else(|| self.detected(uuid)).unwrap_or_else(|| short(uuid)).to_owned()
    }

    /// `account / organization` label for an organization directory.
    pub fn org_label(&self, key: &OrgKey) -> String {
        format!("{} / {}", self.name(&key.account), self.name(&key.org))
    }
}
