//! Secret storage with automatic fallback.
//!
//! Primary: OS keyring (Windows Credential Manager / Linux Secret Service).
//! Fallback: file-based vault at `<data_dir>/setu/vault.json` with `chmod 600`.
//!
//! The fallback activates automatically when the OS keyring is unavailable
//! (e.g. no gnome-keyring / KDE Wallet running). Each method creates a fresh
//! `keyring::Entry` — no stored state on the struct itself.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::OnceLock;

const SERVICE: &str = "setu";

const KEY_DB: &str = "db_key";
const KEY_OAUTH_TOKEN: &str = "oauth_token";
const KEY_CARDDAV_PASSWORD: &str = "carddav_password";
const KEY_GOOGLE_CLIENT_SECRET: &str = "google_client_secret";

// ── Backend detection ────────────────────────────────────────────────

#[derive(Debug)]
enum Backend {
    Keyring,
    File,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

fn backend() -> &'static Backend {
    BACKEND.get_or_init(|| {
        match keyring::Entry::new(SERVICE, "__probe__") {
            Ok(probe) => match probe.get_password() {
                // Keyring is functional (entry exists or is empty).
                Ok(_) | Err(keyring::Error::NoEntry) => {
                    tracing::info!("using OS keyring for secret storage");
                    Backend::Keyring
                }
                Err(e) => {
                    tracing::warn!(
                        "OS keyring unavailable ({e}) — using file-based vault"
                    );
                    Backend::File
                }
            },
            Err(e) => {
                tracing::warn!(
                    "OS keyring unavailable ({e}) — using file-based vault"
                );
                Backend::File
            }
        }
    })
}

// ── Unified get / set / delete ───────────────────────────────────────

fn vault_get(key: &str) -> Result<Option<String>> {
    match backend() {
        Backend::Keyring => match keyring_entry(key)?.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("keyring error reading {key}: {e}")),
        },
        Backend::File => file_get(key),
    }
}

fn vault_set(key: &str, value: &str) -> Result<()> {
    match backend() {
        Backend::Keyring => keyring_entry(key)?
            .set_password(value)
            .with_context(|| format!("storing {key} in keyring")),
        Backend::File => file_set(key, value),
    }
}

fn vault_delete(key: &str) -> Result<()> {
    match backend() {
        Backend::Keyring => match keyring_entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("keyring error deleting {key}: {e}")),
        },
        Backend::File => file_delete(key),
    }
}

// ── SecureVault (public API — unchanged interface) ───────────────────

/// Zero-sized handle to secret storage. All methods are stateless.
#[derive(Copy, Clone, Debug)]
pub struct SecureVault;

impl SecureVault {
    /// Get (or generate + store) the 32-byte hex-encoded DB encryption key.
    pub fn get_or_init_db_key(&self) -> Result<String> {
        if let Some(key) = vault_get(KEY_DB)? {
            return Ok(key);
        }
        let key = generate_hex_key(32);
        vault_set(KEY_DB, &key)?;
        Ok(key)
    }

    /// Store the full OAuth token JSON blob.
    pub fn store_oauth_token(&self, token_json: &str) -> Result<()> {
        vault_set(KEY_OAUTH_TOKEN, token_json)
    }

    /// Retrieve the OAuth token JSON (None if absent).
    pub fn get_oauth_token(&self) -> Result<Option<String>> {
        vault_get(KEY_OAUTH_TOKEN)
    }

    /// Remove the stored OAuth token.
    pub fn clear_oauth_token(&self) -> Result<()> {
        vault_delete(KEY_OAUTH_TOKEN)
    }

    /// Returns `true` if an OAuth token exists.
    pub fn has_oauth_token(&self) -> bool {
        vault_get(KEY_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some()
    }

    /// Get (or generate + store) the CardDAV Basic Auth password.
    pub fn get_or_init_carddav_password(&self) -> Result<String> {
        if let Some(pw) = vault_get(KEY_CARDDAV_PASSWORD)? {
            return Ok(pw);
        }
        let pw = generate_alphanumeric(24);
        vault_set(KEY_CARDDAV_PASSWORD, &pw)?;
        Ok(pw)
    }

    /// Explicitly set the CardDAV Basic Auth password.
    pub fn store_carddav_password(&self, password: &str) -> Result<()> {
        vault_set(KEY_CARDDAV_PASSWORD, password)
    }

    /// Store the Google client secret.
    pub fn store_google_client_secret(&self, secret: &str) -> Result<()> {
        vault_set(KEY_GOOGLE_CLIENT_SECRET, secret)
    }

    /// Retrieve the Google client secret.
    pub fn get_google_client_secret(&self) -> Result<Option<String>> {
        vault_get(KEY_GOOGLE_CLIENT_SECRET)
    }
}

// ── Keyring backend ──────────────────────────────────────────────────

fn keyring_entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).context("creating keyring entry")
}

// ── File backend ─────────────────────────────────────────────────────

fn vault_file_path() -> Result<std::path::PathBuf> {
    let base = dirs::data_dir().context("cannot resolve data directory")?;
    let dir = base.join("setu");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("vault.json"))
}

fn file_read_map() -> Result<HashMap<String, String>> {
    let path = vault_file_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&data).context("parsing vault.json")
}

fn file_write_map(map: &HashMap<String, String>) -> Result<()> {
    let path = vault_file_path()?;
    let data = serde_json::to_string_pretty(map)?;
    std::fs::write(&path, data.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;

    // Restrict to owner-only on Unix (equivalent of chmod 600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

fn file_get(key: &str) -> Result<Option<String>> {
    let map = file_read_map()?;
    Ok(map.get(key).cloned())
}

fn file_set(key: &str, value: &str) -> Result<()> {
    let mut map = file_read_map()?;
    map.insert(key.to_string(), value.to_string());
    file_write_map(&map)
}

fn file_delete(key: &str) -> Result<()> {
    let mut map = file_read_map()?;
    map.remove(key);
    file_write_map(&map)
}

// ── One-time migration from "wincard" → "setu" keyring service ──────

const OLD_SERVICE: &str = "wincard";

/// Keys to migrate from the old "wincard" keyring service.
const MIGRATE_KEYS: &[&str] = &[
    KEY_DB,
    KEY_OAUTH_TOKEN,
    KEY_CARDDAV_PASSWORD,
    KEY_GOOGLE_CLIENT_SECRET,
];

/// Migrate keyring entries from the old "wincard" service to "setu".
///
/// For each key: if the new entry is missing but the old one exists,
/// copy the value over. Old entries are left in place (harmless).
/// This is idempotent — safe to call on every startup.
pub fn migrate_keyring_from_wincard() {
    // Only attempt migration if we're using the keyring backend.
    if !matches!(backend(), Backend::Keyring) {
        return;
    }

    for &key in MIGRATE_KEYS {
        let new_entry = match keyring::Entry::new(SERVICE, key) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip if new entry already has a value.
        match new_entry.get_password() {
            Ok(_) => continue,
            Err(keyring::Error::NoEntry) => {} // needs migration
            Err(_) => continue,
        }

        // Try to read from the old service.
        let old_entry = match keyring::Entry::new(OLD_SERVICE, key) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if let Ok(value) = old_entry.get_password() {
            if new_entry.set_password(&value).is_ok() {
                tracing::info!(key, "migrated keyring entry from wincard → setu");
            }
        }
    }
}

// ── Utilities ────────────────────────────────────────────────────────

/// Generate `n` random bytes and return as a hex string (2 * n chars).
fn generate_hex_key(n: usize) -> String {
    use rand::Rng;
    let bytes: Vec<u8> = (0..n).map(|_| rand::thread_rng().gen()).collect();
    let mut hex = String::with_capacity(n * 2);
    for b in bytes {
        hex.push(HEX_CHARS[(b >> 4) as usize]);
        hex.push(HEX_CHARS[(b & 0x0f) as usize]);
    }
    hex
}

const HEX_CHARS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Generate a random alphanumeric string of length `n`.
fn generate_alphanumeric(n: usize) -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}
