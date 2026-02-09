//! Shared Google People API client — used by both the sync engine and the
//! CardDAV server's on-demand search fallback.
//!
//! # Warmup
//! Google's `people.searchContacts` endpoint requires a "warmup" call
//! (empty query) before real searches will return results.  [`GoogleApi::warmup_search`]
//! should be called once at startup; subsequent [`GoogleApi::search_by_phone`]
//! calls will re-warm automatically if the cache has gone stale (>5 min).

use anyhow::{Context, Result};
use google_people1::api::Person;
use google_people1::common::FieldMask;
use google_people1::PeopleService;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crate::auth;
use crate::config::Config;

// ── Hub type alias ──────────────────────────────────────────────────────

/// Concrete `PeopleService` parameterised over the HTTPS connector we use.
pub type Hub = PeopleService<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
>;

/// Fields to request from the People API (shared by sync + search).
pub const PERSON_FIELDS: &[&str] = &[
    "names",
    "emailAddresses",
    "phoneNumbers",
    "addresses",
    "organizations",
    "birthdays",
    "photos",
    "metadata",
];

// ── GoogleApi ───────────────────────────────────────────────────────────

/// Thread-safe wrapper around a `PeopleService` hub.
///
/// Clone is cheap (interior `Arc`s).
#[derive(Clone)]
pub struct GoogleApi {
    hub: Arc<Hub>,
    /// Timestamp of the last successful warmup call.
    warmup_at: Arc<Mutex<Option<Instant>>>,
}

/// How long a warmup remains valid before we re-warm automatically.
const WARMUP_TTL_SECS: u64 = 300;

/// Pause after a fresh warmup before issuing the real search.
const POST_WARMUP_DELAY_SECS: u64 = 2;

impl GoogleApi {
    /// Build a fully-authenticated `GoogleApi` from the application config.
    ///
    /// The `client_secret` is passed explicitly (loaded from the OS keyring)
    /// rather than read from the config struct.
    ///
    /// This creates the OAuth2 authenticator (with on-disk token cache) and
    /// the HTTPS + HTTP/2 client.  The returned handle is `Clone + Send + Sync`.
    pub async fn build(config: &Config, client_secret: &str) -> Result<Self> {
        let token_path = auth::token_file_path()?;

        let secret = yup_oauth2::ApplicationSecret {
            client_id: config.google_client_id.clone(),
            client_secret: client_secret.to_string(),
            auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
            redirect_uris: vec!["http://localhost".into()],
            ..Default::default()
        };

        let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
            secret,
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
        .persist_tokens_to_disk(&token_path)
        .flow_delegate(Box::new(auth::BrowserDelegate))
        .build()
        .await
        .context("failed to build OAuth2 authenticator")?;

        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http2()
            .build();

        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);

        let hub = PeopleService::new(client, auth);

        Ok(Self {
            hub: Arc::new(hub),
            warmup_at: Arc::new(Mutex::new(None)),
        })
    }

    /// Direct access to the underlying `PeopleService` hub (used by the sync
    /// engine for `connections_list` calls).
    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    // ── Search warmup ───────────────────────────────────────────────

    /// Send an empty `searchContacts` request to prime Google's server-side
    /// cache.  Must be called at least once before real searches return
    /// results.
    pub async fn warmup_search(&self) -> Result<()> {
        let fields = FieldMask::new::<&str>(PERSON_FIELDS);
        let _ = self
            .hub
            .people()
            .search_contacts()
            .query("")
            .read_mask(fields)
            .page_size(1)
            .doit()
            .await
            .context("warmup searchContacts")?;

        let mut state = self.warmup_at.lock().await;
        *state = Some(Instant::now());
        tracing::info!("Google searchContacts warmup complete");
        Ok(())
    }

    /// Returns `true` if a warmup has been performed within [`WARMUP_TTL_SECS`].
    async fn is_warm(&self) -> bool {
        let state = self.warmup_at.lock().await;
        state.map_or(false, |t| {
            t.elapsed() < std::time::Duration::from_secs(WARMUP_TTL_SECS)
        })
    }

    /// Ensure the search cache is warm, performing a fresh warmup + delay
    /// if necessary.
    async fn ensure_warm(&self) -> Result<()> {
        if !self.is_warm().await {
            self.warmup_search().await?;
            tokio::time::sleep(std::time::Duration::from_secs(POST_WARMUP_DELAY_SECS)).await;
        }
        Ok(())
    }

    // ── Live search ─────────────────────────────────────────────────

    /// Search Google Contacts by phone number.
    ///
    /// Automatically warms up the search cache if it has expired.
    /// Returns `Ok(None)` when no match is found.
    pub async fn search_by_phone(&self, number: &str) -> Result<Option<Person>> {
        self.ensure_warm().await?;

        let fields = FieldMask::new::<&str>(PERSON_FIELDS);
        let (_resp, result) = self
            .hub
            .people()
            .search_contacts()
            .query(number)
            .read_mask(fields)
            .page_size(5)
            .doit()
            .await
            .context("searchContacts by phone")?;

        let person = result
            .results
            .and_then(|results| results.into_iter().find_map(|r| r.person));

        Ok(person)
    }
}
