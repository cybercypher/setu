//! OS-level secret storage via the platform keyring.
//!
//! On Windows: Windows Credential Manager.
//! On Linux: Secret Service (GNOME Keyring / KDE Wallet).
//!
//! Each method creates a fresh `keyring::Entry` — no stored state.

use anyhow::{Context, Result};

const SERVICE: &str = "setu";

const KEY_DB: &str = "db_key";
const KEY_OAUTH_TOKEN: &str = "oauth_token";
const KEY_CARDDAV_PASSWORD: &str = "carddav_password";
const KEY_GOOGLE_CLIENT_SECRET: &str = "google_client_secret";

/// Zero-sized handle to the OS keyring. All methods are stateless.
#[derive(Copy, Clone, Debug)]
pub struct SecureVault;

impl SecureVault {
    /// Get (or generate + store) the 32-byte hex-encoded DB encryption key.
    pub fn get_or_init_db_key(&self) -> Result<String> {
        let entry = entry(KEY_DB)?;
        match entry.get_password() {
            Ok(key) => Ok(key),
            Err(keyring::Error::NoEntry) => {
                let key = generate_hex_key(32);
                entry
                    .set_password(&key)
                    .context("storing DB key in keyring")?;
                Ok(key)
            }
            Err(e) => Err(anyhow::anyhow!("keyring error reading DB key: {e}")),
        }
    }

    /// Store the full OAuth token JSON blob in the keyring.
    pub fn store_oauth_token(&self, token_json: &str) -> Result<()> {
        entry(KEY_OAUTH_TOKEN)?
            .set_password(token_json)
            .context("storing OAuth token in keyring")
    }

    /// Retrieve the OAuth token JSON from the keyring (None if absent).
    pub fn get_oauth_token(&self) -> Result<Option<String>> {
        match entry(KEY_OAUTH_TOKEN)?.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("keyring error reading OAuth token: {e}")),
        }
    }

    /// Remove the stored OAuth token.
    pub fn clear_oauth_token(&self) -> Result<()> {
        match entry(KEY_OAUTH_TOKEN)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("keyring error clearing OAuth token: {e}")),
        }
    }

    /// Returns `true` if an OAuth token exists in the keyring.
    pub fn has_oauth_token(&self) -> bool {
        entry(KEY_OAUTH_TOKEN)
            .and_then(|e| match e.get_password() {
                Ok(_) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            })
            .unwrap_or(false)
    }

    /// Get (or generate + store) the CardDAV Basic Auth password.
    pub fn get_or_init_carddav_password(&self) -> Result<String> {
        let entry = entry(KEY_CARDDAV_PASSWORD)?;
        match entry.get_password() {
            Ok(pw) => Ok(pw),
            Err(keyring::Error::NoEntry) => {
                let pw = generate_alphanumeric(24);
                entry
                    .set_password(&pw)
                    .context("storing CardDAV password in keyring")?;
                Ok(pw)
            }
            Err(e) => Err(anyhow::anyhow!(
                "keyring error reading CardDAV password: {e}"
            )),
        }
    }

    /// Explicitly set the CardDAV Basic Auth password (overwriting any auto-generated one).
    pub fn store_carddav_password(&self, password: &str) -> Result<()> {
        entry(KEY_CARDDAV_PASSWORD)?
            .set_password(password)
            .context("storing CardDAV password in keyring")
    }

    /// Store the Google client secret in the keyring.
    pub fn store_google_client_secret(&self, secret: &str) -> Result<()> {
        entry(KEY_GOOGLE_CLIENT_SECRET)?
            .set_password(secret)
            .context("storing Google client secret in keyring")
    }

    /// Retrieve the Google client secret from the keyring.
    pub fn get_google_client_secret(&self) -> Result<Option<String>> {
        match entry(KEY_GOOGLE_CLIENT_SECRET)?.get_password() {
            Ok(val) => Ok(Some(val)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(
                "keyring error reading Google client secret: {e}"
            )),
        }
    }
}

fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).context("creating keyring entry")
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
