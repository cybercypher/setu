//! Application configuration — persisted to `%APPDATA%/setu/config.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::vault::SecureVault;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub google_client_id: String,
    #[serde(default)]
    pub google_client_secret: String,
    #[serde(default = "default_sync_interval")]
    pub sync_interval_secs: u64,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default)]
    pub use_tls: bool,
}

fn default_sync_interval() -> u64 {
    900
}
fn default_server_port() -> u16 {
    5232
}

impl Default for Config {
    fn default() -> Self {
        Self {
            google_client_id: String::new(),
            google_client_secret: String::new(),
            sync_interval_secs: default_sync_interval(),
            server_port: default_server_port(),
            use_tls: false,
        }
    }
}

impl Config {
    /// Path to the config file: `%APPDATA%/setu/config.json`.
    pub fn path() -> Result<PathBuf> {
        let base = dirs::data_dir().context("cannot resolve %APPDATA%")?;
        Ok(base.join("setu").join("config.json"))
    }

    /// Load from disk, or return defaults if the file doesn't exist yet.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            serde_json::from_str(&data).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    /// Persist to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        Ok(())
    }

    /// Load config and migrate the client secret from config.json to the
    /// OS keyring if it is still stored in plaintext.
    pub fn load_and_migrate(vault: &SecureVault) -> Result<Self> {
        let mut cfg = Self::load()?;

        // Migrate: move client_secret from config.json → keyring.
        if !cfg.google_client_secret.is_empty() {
            vault.store_google_client_secret(&cfg.google_client_secret)?;
            cfg.google_client_secret.clear();
            cfg.save()?;
            tracing::info!("migrated google_client_secret from config.json to OS keyring");
        }

        Ok(cfg)
    }

    /// Returns `true` if OAuth credentials are configured.
    ///
    /// Checks client ID in config + client secret in the OS keyring.
    pub fn has_credentials(&self, vault: &SecureVault) -> bool {
        if self.google_client_id.is_empty() {
            return false;
        }
        vault
            .get_google_client_secret()
            .ok()
            .flatten()
            .map_or(false, |s| !s.is_empty())
    }
}
