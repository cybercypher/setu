//! OAuth2 authentication flow for Google People API.
//!
//! Provides a two-step login flow:
//! 1. [`login()`] — runs the OAuth2 installed-app flow (browser → localhost redirect → token).
//! 2. [`ensure_authenticated()`] — checks that a valid token exists in SQLite before sync.
//!
//! The loopback listener binds to an OS-assigned port on `127.0.0.1` to receive
//! the Google redirect.

use anyhow::{Context, Result};
use std::future::Future;
use std::pin::Pin;
use yup_oauth2::authenticator_delegate::InstalledFlowDelegate;

use crate::db;
use crate::vault::SecureVault;

/// Google People API read-only scope.
const SCOPES: &[&str] = &["https://www.googleapis.com/auth/contacts.readonly"];

// ── Browser launcher ──────────────────────────────────────────────────

/// Open a URL in the default browser.
///
/// On Windows, uses the `open` crate (ShellExecuteW).
/// On Linux, bypasses shell invocation to avoid `&` in URLs being
/// misinterpreted — tries xdg-open, sensible-browser, then common browsers.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let browsers = [
            "xdg-open",
            "sensible-browser",
            "firefox",
            "chromium",
            "chromium-browser",
            "google-chrome",
        ];
        for browser in &browsers {
            // Pass URL as a direct argument (no shell) so & is not mangled.
            if std::process::Command::new(browser)
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
        anyhow::bail!("no browser found");
    }
    #[cfg(not(target_os = "linux"))]
    {
        open::that(url).context("failed to open default browser")?;
        Ok(())
    }
}

// ── OAuth2 browser delegate ──────────────────────────────────────────

/// Delegate that opens the Google consent screen via [`open_browser`].
#[derive(Copy, Clone)]
pub(crate) struct BrowserDelegate;

impl InstalledFlowDelegate for BrowserDelegate {
    fn present_user_url<'a>(
        &'a self,
        url: &'a str,
        _need_code: bool,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            tracing::info!("opening browser for Google sign-in");
            // Always print the URL so the user can open it manually if the
            // browser launch fails (common in containers / webtop / WSL).
            eprintln!("\n  Open this URL to sign in with Google:\n  {url}\n");
            if let Err(e) = open_browser(url) {
                tracing::warn!("could not open browser: {e:#} — copy the URL from the terminal");
            }
            Ok(String::new())
        })
    }
}

// ── Token file path ──────────────────────────────────────────────────

/// Path to the yup-oauth2 token cache file (`<data_dir>/setu/oauth_token.json`).
///
/// Shared between [`login()`] and [`crate::google_api::GoogleApi::build()`] so
/// both use the same on-disk cache.
pub fn token_file_path() -> Result<std::path::PathBuf> {
    let base = dirs::data_dir().context("cannot resolve data directory")?;
    let dir = base.join("setu");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("oauth_token.json"))
}

// ── Login result ─────────────────────────────────────────────────────

/// Result of a successful OAuth2 login.
pub struct LoginResult {
    /// The user's Google email address.
    pub email: String,
}

// ── OAuth2 flow ──────────────────────────────────────────────────────

/// Concrete hub type used only within this module for the email fetch.
type AuthHub = google_people1::PeopleService<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
>;

/// Run the full OAuth2 installed-app flow.
///
/// 1. Start a TCP listener on `127.0.0.1` (OS-assigned port).
/// 2. Open the Google auth URL in the Windows browser.
/// 3. Capture the `code` from the redirect.
/// 4. Exchange for an access + refresh token.
/// 5. Persist the token to the OS keyring and email to encrypted SQLite.
///
/// Returns a [`LoginResult`] with the user's email on success.
pub async fn login(
    client_id: &str,
    client_secret: &str,
    vault: &SecureVault,
    db_key: &str,
) -> Result<LoginResult> {
    let secret = yup_oauth2::ApplicationSecret {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
        token_uri: "https://oauth2.googleapis.com/token".into(),
        redirect_uris: vec!["http://localhost".into()],
        ..Default::default()
    };

    let token_path = token_file_path()?;

    // Remove any stale token file so yup-oauth2 always starts a fresh
    // browser flow when the user explicitly clicks "Login".
    if token_path.exists() {
        let _ = std::fs::remove_file(&token_path);
    }

    let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
        secret,
        yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
    )
    .persist_tokens_to_disk(&token_path)
    .flow_delegate(Box::new(BrowserDelegate))
    .build()
    .await
    .context("failed to build OAuth2 authenticator")?;

    // Trigger the OAuth flow by requesting a token for the People API scope.
    let _token = auth
        .token(SCOPES)
        .await
        .context("OAuth2 authorization failed")?;

    tracing::info!("OAuth2 login successful, token persisted to {}", token_path.display());

    // Read the token file so we can store it in the keyring.
    let token_json = std::fs::read_to_string(&token_path)
        .context("reading token file after login")?;

    // Store token in OS keyring.
    vault.store_oauth_token(&token_json)?;

    // Store a marker in SQLite (token lives in keyring, email comes later).
    let db_key_owned = db_key.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db::open(Some(&db_key_owned))?;
        db::store_oauth_token(&conn, "<stored-in-keyring>", "")?;
        anyhow::Ok(())
    })
    .await??;

    // Try to fetch the user's email (best-effort, with timeout).
    let email = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fetch_user_email_with_auth(auth),
    )
    .await
    {
        Ok(Ok(addr)) => {
            // Update SQLite with the email.
            let addr_clone = addr.clone();
            let db_key_owned = db_key.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                let conn = db::open(Some(&db_key_owned))?;
                db::store_oauth_token(&conn, "<stored-in-keyring>", &addr_clone)?;
                anyhow::Ok(())
            })
            .await;
            addr
        }
        Ok(Err(e)) => {
            tracing::warn!("could not fetch user email: {e:#}");
            "Authenticated".into()
        }
        Err(_) => {
            tracing::warn!("email fetch timed out (non-fatal)");
            "Authenticated".into()
        }
    };

    Ok(LoginResult { email })
}

/// Build a fresh HTTP client + hub and fetch the user's primary email.
async fn fetch_user_email_with_auth(
    auth: yup_oauth2::authenticator::Authenticator<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    >,
) -> Result<String> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http2()
        .build();
    let client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(connector);
    let hub: AuthHub = google_people1::PeopleService::new(client, auth);

    let fields = google_people1::common::FieldMask::new::<&str>(&["emailAddresses"]);
    let (_resp, person) = hub
        .people()
        .get("people/me")
        .person_fields(fields)
        .doit()
        .await
        .context("People API people.get(me)")?;

    let email = person
        .email_addresses
        .and_then(|addrs| addrs.into_iter().find_map(|a| a.value))
        .unwrap_or_else(|| "Unknown".into());

    Ok(email)
}

// ── Authentication check ─────────────────────────────────────────────

/// Check whether the user has a valid OAuth token in the OS keyring.
///
/// Call this before starting sync or search operations.
pub fn ensure_authenticated(vault: &SecureVault) -> bool {
    vault.has_oauth_token()
}

/// Get the stored Google email (if the user has logged in).
pub fn get_logged_in_email(db_key: &str) -> Result<Option<String>> {
    let conn = db::open(Some(db_key))?;
    db::get_google_email(&conn)
}
