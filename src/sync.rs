//! Google People API sync engine with incremental `syncToken` support.
//!
//! Flow:
//!   1. First run  → full sync (fetch all contacts, store syncToken).
//!   2. Later runs → incremental sync (fetch only deltas via syncToken).
//!   3. If the token expires (410 Gone) → fall back to a full sync.

use anyhow::{Context, Result};
use google_people1::api::Person;
use google_people1::common::FieldMask;
use tokio::sync::mpsc;

use setu_lib::{auth, db};
use setu_lib::google_api::{GoogleApi, Hub, PERSON_FIELDS};
use setu_lib::vault::SecureVault;
use crate::vcard;

// ── Public entry point ───────────────────────────────────────────────────

/// Run the sync loop forever.
///
/// * `google_api` – shared Google API client (already authenticated).
/// * `interval_secs` – seconds between automatic syncs.
/// * `trigger_rx` – receives `()` when the user clicks "Sync Now".
/// * `vault` – OS keyring handle for auth checks.
/// * `db_key` – hex-encoded SQLCipher encryption key.
pub async fn run_sync_loop(
    google_api: GoogleApi,
    interval_secs: u64,
    mut trigger_rx: mpsc::Receiver<()>,
    vault: SecureVault,
    db_key: String,
) -> Result<()> {
    let hub = google_api.hub();
    let interval = tokio::time::Duration::from_secs(interval_secs);
    tracing::info!(interval_secs, "sync loop started");

    loop {
        if let Err(e) = run_one_sync(hub, &vault, &db_key).await {
            tracing::error!("sync failed: {e:#}");
        }

        tokio::select! {
            _ = tokio::time::sleep(interval) => {},
            _ = trigger_rx.recv() => {
                tracing::info!("immediate sync triggered");
            },
        }
    }
}

// ── Single sync cycle ────────────────────────────────────────────────────

async fn run_one_sync(hub: &Hub, vault: &SecureVault, db_key: &str) -> Result<()> {
    // Verify OAuth token is present before attempting API calls.
    let v = *vault;
    if !auth::ensure_authenticated(&v) {
        anyhow::bail!("not authenticated — skipping sync");
    }

    // Read the sync token on a blocking thread (rusqlite::Connection is !Send).
    let db_key_owned = db_key.to_string();
    let sync_token = tokio::task::spawn_blocking(move || {
        let conn = db::open(Some(&db_key_owned))?;
        db::get_sync_token(&conn)
    })
    .await??;

    match sync_token {
        Some(token) => match incremental_sync(hub, &token, db_key).await {
            Ok(()) => {}
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("410") || msg.contains("Sync token") || msg.contains("expired") {
                    tracing::warn!("sync token expired, falling back to full sync");
                    full_sync(hub, db_key).await?;
                } else {
                    return Err(e);
                }
            }
        },
        None => {
            tracing::info!("no sync token found — performing full sync");
            full_sync(hub, db_key).await?;
        }
    }

    Ok(())
}

// ── Full sync ────────────────────────────────────────────────────────────

async fn full_sync(hub: &Hub, db_key: &str) -> Result<()> {
    let fields = FieldMask::new::<&str>(PERSON_FIELDS);
    let mut page_token: Option<String> = None;
    let mut all_persons: Vec<Person> = Vec::new();
    let mut new_sync_token: Option<String> = None;

    loop {
        let mut req = hub
            .people()
            .connections_list("people/me")
            .person_fields(fields.clone())
            .page_size(1000)
            .request_sync_token(true);

        if let Some(ref pt) = page_token {
            req = req.page_token(pt);
        }

        let (_resp, body) = req.doit().await.context("People API connections_list")?;

        if let Some(connections) = body.connections {
            all_persons.extend(connections);
        }

        if body.next_sync_token.is_some() {
            new_sync_token = body.next_sync_token;
        }

        match body.next_page_token {
            Some(pt) => page_token = Some(pt),
            None => break,
        }
    }

    let total = all_persons.len();

    // Write all contacts to DB on a blocking thread.
    let token = new_sync_token.clone();
    let db_key_owned = db_key.to_string();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = db::open(Some(&db_key_owned))?;
        for person in &all_persons {
            store_person(&conn, person)?;
        }
        if let Some(t) = token {
            db::set_sync_token(&conn, &t)?;
        }
        Ok(())
    })
    .await??;

    tracing::info!(contacts = total, "full sync complete");
    Ok(())
}

// ── Incremental sync ─────────────────────────────────────────────────────

async fn incremental_sync(hub: &Hub, sync_token: &str, db_key: &str) -> Result<()> {
    let fields = FieldMask::new::<&str>(PERSON_FIELDS);
    let mut page_token: Option<String> = None;
    let mut upserts: Vec<Person> = Vec::new();
    let mut deletions: Vec<String> = Vec::new();
    let mut new_sync_token: Option<String> = None;

    loop {
        let mut req = hub
            .people()
            .connections_list("people/me")
            .person_fields(fields.clone())
            .sync_token(sync_token)
            .request_sync_token(true)
            .page_size(1000);

        if let Some(ref pt) = page_token {
            req = req.page_token(pt);
        }

        let (_resp, body) = req
            .doit()
            .await
            .context("People API incremental connections_list")?;

        if let Some(connections) = body.connections {
            for person in connections {
                let is_deleted = person
                    .metadata
                    .as_ref()
                    .and_then(|m| m.deleted)
                    .unwrap_or(false);

                if is_deleted {
                    if let Some(rn) = person.resource_name.clone() {
                        deletions.push(rn);
                    }
                } else {
                    upserts.push(person);
                }
            }
        }

        if body.next_sync_token.is_some() {
            new_sync_token = body.next_sync_token;
        }

        match body.next_page_token {
            Some(pt) => page_token = Some(pt),
            None => break,
        }
    }

    let upserted = upserts.len();
    let deleted = deletions.len();

    // Write changes to DB on a blocking thread.
    let token = new_sync_token.clone();
    let db_key_owned = db_key.to_string();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = db::open(Some(&db_key_owned))?;
        for person in &upserts {
            store_person(&conn, person)?;
        }
        for rn in &deletions {
            db::delete_contact(&conn, rn)?;
        }
        if let Some(t) = token {
            db::set_sync_token(&conn, &t)?;
        }
        Ok(())
    })
    .await??;

    if upserted > 0 || deleted > 0 {
        tracing::info!(upserted, deleted, "incremental sync complete");
    } else {
        tracing::debug!("incremental sync: no changes");
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Normalise all phone numbers on a `Person` into a single
/// space-separated string suitable for substring search.
fn normalize_phones(person: &Person) -> String {
    let phones = match person.phone_numbers.as_ref() {
        Some(p) => p,
        None => return String::new(),
    };

    phones
        .iter()
        .filter_map(|p| p.value.as_deref())
        .map(db::normalize_phone)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn store_person(conn: &rusqlite::Connection, person: &Person) -> Result<()> {
    let resource_name = match person.resource_name.as_deref() {
        Some(rn) => rn,
        None => {
            tracing::warn!("skipping person with no resource_name");
            return Ok(());
        }
    };

    let etag = person.etag.as_deref().unwrap_or("");
    let display = vcard::display_name(person);
    let vcard = vcard::person_to_vcard(person);
    let searchable_phone = normalize_phones(person);

    db::upsert_contact(conn, resource_name, etag, &display, &vcard, &searchable_phone)?;
    Ok(())
}
