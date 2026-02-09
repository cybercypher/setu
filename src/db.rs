//! SQLite database layer — contacts cache + sync metadata.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

/// Resolve the database path: `%APPDATA%/setu/setu.db` on Windows.
pub fn db_path() -> Result<PathBuf> {
    let base = dirs::data_dir().context("cannot resolve %APPDATA%")?;
    let dir = base.join("setu");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("setu.db"))
}

/// Open (and auto-migrate) the database.
///
/// When `encryption_key` is `Some`, the hex-encoded key is applied via
/// `PRAGMA key` (SQLCipher) and `PRAGMA secure_delete` is enabled.
pub fn open(encryption_key: Option<&str>) -> Result<Connection> {
    let path = db_path()?;
    let conn = Connection::open(&path)?;

    if let Some(hex_key) = encryption_key {
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";"))?;
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
    }

    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    migrate(&conn)?;
    Ok(conn)
}

/// One-time migration from an unencrypted SQLite database to SQLCipher.
///
/// If the DB is already encrypted (or doesn't exist yet), this is a no-op.
pub fn migrate_to_encrypted(encryption_key: &str) -> Result<()> {
    let path = db_path()?;
    if !path.exists() {
        return Ok(());
    }

    // Try opening with the key — if the master table is readable, it's already encrypted.
    {
        let conn = Connection::open(&path)?;
        conn.execute_batch(&format!("PRAGMA key = \"x'{encryption_key}'\";"))?;
        if conn
            .query_row(
                "SELECT count(*) FROM sqlite_master",
                [],
                |row| row.get::<_, i64>(0),
            )
            .is_ok()
        {
            return Ok(());
        }
    }

    // The DB is unencrypted — export to an encrypted copy.
    let encrypted_path = path.with_extension("db.enc");
    {
        let conn = Connection::open(&path)?;
        // Verify it's readable without a key.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .context("existing DB is neither unencrypted nor encrypted with the given key")?;

        conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS encrypted KEY \"x'{encryption_key}'\";",
            encrypted_path.display()
        ))?;
        conn.execute_batch("SELECT sqlcipher_export('encrypted');")?;
        conn.execute_batch("DETACH DATABASE encrypted;")?;
    }

    // Replace original with encrypted version.
    std::fs::rename(&encrypted_path, &path)
        .context("replacing unencrypted DB with encrypted copy")?;

    tracing::info!("migrated database to SQLCipher encryption");
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS contacts (
            -- Google People API resource name, e.g. 'people/c123456'
            resource_name  TEXT PRIMARY KEY NOT NULL,
            -- Google etag for change detection
            etag           TEXT NOT NULL,
            -- Display name (cached for quick listing)
            display_name   TEXT NOT NULL DEFAULT '',
            -- Pre-rendered vCard 3.0 blob
            vcard          TEXT NOT NULL,
            -- Phone numbers with non-digit chars stripped, space-separated
            searchable_phone TEXT NOT NULL DEFAULT '',
            -- ISO-8601 timestamp of last Google update
            updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS sync_metadata (
            id          INTEGER PRIMARY KEY CHECK (id = 1),
            -- Google People API syncToken for incremental sync
            sync_token  TEXT,
            -- ISO-8601 timestamp of last successful sync
            last_sync   TEXT
        );

        -- Ensure the singleton row exists.
        INSERT OR IGNORE INTO sync_metadata (id) VALUES (1);

        CREATE TABLE IF NOT EXISTS oauth_tokens (
            id            INTEGER PRIMARY KEY CHECK (id = 1),
            -- Serialized yup-oauth2 token as JSON
            token_json    TEXT NOT NULL,
            -- Google email of the authenticated user
            google_email  TEXT NOT NULL DEFAULT ''
        );
        ",
    )?;

    // Migration: add google_email to oauth_tokens for existing databases.
    let has_email_col: bool = conn
        .prepare("SELECT google_email FROM oauth_tokens LIMIT 0")
        .is_ok();
    if !has_email_col {
        conn.execute_batch(
            "ALTER TABLE oauth_tokens ADD COLUMN google_email TEXT NOT NULL DEFAULT '';"
        )?;
    }

    // Migration: add searchable_phone to existing databases that lack it.
    let has_column: bool = conn
        .prepare("SELECT searchable_phone FROM contacts LIMIT 0")
        .is_ok();
    if !has_column {
        conn.execute_batch(
            "ALTER TABLE contacts ADD COLUMN searchable_phone TEXT NOT NULL DEFAULT '';"
        )?;
    }

    // Index for fast phone-number substring searches.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_contacts_searchable_phone
         ON contacts(searchable_phone);"
    )?;

    Ok(())
}

// ── Phone normalization ──────────────────────────────────────────────────

/// Strip a phone number to digits only, preserving a leading `+`.
///
/// ```text
/// "+1 (555) 012-3456" → "+15550123456"
/// "555.012.3456"       → "5550123456"
/// ```
pub fn normalize_phone(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (i, ch) in raw.chars().enumerate() {
        if ch.is_ascii_digit() {
            out.push(ch);
        } else if ch == '+' && i == 0 {
            out.push(ch);
        }
    }
    out
}

// ── Query helpers ────────────────────────────────────────────────────────

/// Get the current sync token (None on first run).
pub fn get_sync_token(conn: &Connection) -> Result<Option<String>> {
    let token = conn
        .query_row(
            "SELECT sync_token FROM sync_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(token)
}

/// Persist a new sync token after a successful sync.
pub fn set_sync_token(conn: &Connection, token: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_metadata SET sync_token = ?1, last_sync = datetime('now') WHERE id = 1",
        params![token],
    )?;
    Ok(())
}

/// Upsert a contact row.
pub fn upsert_contact(
    conn: &Connection,
    resource_name: &str,
    etag: &str,
    display_name: &str,
    vcard: &str,
    searchable_phone: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO contacts (resource_name, etag, display_name, vcard, searchable_phone, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(resource_name) DO UPDATE SET
             etag             = excluded.etag,
             display_name     = excluded.display_name,
             vcard            = excluded.vcard,
             searchable_phone = excluded.searchable_phone,
             updated_at       = excluded.updated_at",
        params![resource_name, etag, display_name, vcard, searchable_phone],
    )?;
    Ok(())
}

/// Delete a contact by resource name (used for sync deletions).
pub fn delete_contact(conn: &Connection, resource_name: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM contacts WHERE resource_name = ?1",
        params![resource_name],
    )?;
    Ok(())
}

/// Return all vCards as (resource_name, etag, vcard) tuples.
pub fn all_contacts(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt =
        conn.prepare("SELECT resource_name, etag, vcard FROM contacts ORDER BY display_name")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Look up a single contact's vCard by resource name.
pub fn get_contact(conn: &Connection, resource_name: &str) -> Result<Option<(String, String)>> {
    let result = conn
        .query_row(
            "SELECT etag, vcard FROM contacts WHERE resource_name = ?1",
            params![resource_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(result)
}

/// Search for contacts by normalized phone number.
///
/// Matches any contact that has a phone number ending with the same digits.
/// For example, searching for `4156466123` will match `+14156466123` and
/// `4156466123` but not `41564661234` (extra trailing digit).
///
/// The match strategy: for each space-separated number in `searchable_phone`,
/// check if it ends with the query (or the query ends with it).  This handles
/// country-code differences (e.g. `4156466123` matches `+14156466123`).
///
/// Returns all matching `(resource_name, etag, vcard)` tuples.
pub fn search_by_phone(
    conn: &Connection,
    normalized_number: &str,
) -> Result<Vec<(String, String, String)>> {
    if normalized_number.is_empty() {
        return Ok(Vec::new());
    }
    // Strip leading '+' for suffix comparison.
    let query_digits = normalized_number.trim_start_matches('+');
    if query_digits.is_empty() {
        return Ok(Vec::new());
    }

    // Use LIKE for an initial broad filter, then refine in Rust.
    let pattern = format!("%{query_digits}%");
    let mut stmt = conn.prepare(
        "SELECT resource_name, etag, vcard, searchable_phone FROM contacts
         WHERE searchable_phone LIKE ?1
         ORDER BY display_name",
    )?;
    let candidates = stmt
        .query_map(params![pattern], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, String>(3)?))
        })?
        .collect::<Result<Vec<(String, String, String, String)>, _>>()?;

    // Refine: only keep contacts where at least one phone number is a
    // suffix match (handles country code differences).
    let mut results = Vec::new();
    for (rn, etag, vcard, searchable) in candidates {
        let is_match = searchable.split_whitespace().any(|stored| {
            let stored_digits = stored.trim_start_matches('+');
            stored_digits.ends_with(query_digits) || query_digits.ends_with(stored_digits)
        });
        if is_match {
            results.push((rn, etag, vcard));
        }
    }
    Ok(results)
}

// ── OAuth token persistence ─────────────────────────────────────────────

/// Store the OAuth2 token JSON and (optionally) the user's Google email.
pub fn store_oauth_token(conn: &Connection, token_json: &str, google_email: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO oauth_tokens (id, token_json, google_email)
         VALUES (1, ?1, ?2)",
        params![token_json, google_email],
    )?;
    Ok(())
}

/// Retrieve the stored OAuth2 token JSON (if any).
pub fn get_oauth_token(conn: &Connection) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT token_json FROM oauth_tokens WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

/// Returns `true` if an OAuth2 token is stored.
pub fn has_oauth_token(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM oauth_tokens WHERE id = 1",
        [],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// Remove the stored OAuth2 token (logout).
pub fn clear_oauth_token(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM oauth_tokens WHERE id = 1", [])?;
    Ok(())
}

/// Get the stored Google email of the authenticated user.
pub fn get_google_email(conn: &Connection) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT google_email FROM oauth_tokens WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    // Treat empty string as None.
    Ok(result.filter(|s| !s.is_empty()))
}

/// Open an in-memory database (for testing).
#[cfg(test)]
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let conn = open_in_memory().unwrap();
        // Running migrate again should not fail.
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
    }

    #[test]
    fn sync_token_lifecycle() {
        let conn = open_in_memory().unwrap();

        // Initially None.
        let token = get_sync_token(&conn).unwrap();
        assert!(token.is_none(), "expected None on first run");

        // Set a token.
        set_sync_token(&conn, "token_v1").unwrap();
        let token = get_sync_token(&conn).unwrap();
        assert_eq!(token.as_deref(), Some("token_v1"));

        // Overwrite with a new token.
        set_sync_token(&conn, "token_v2").unwrap();
        let token = get_sync_token(&conn).unwrap();
        assert_eq!(token.as_deref(), Some("token_v2"));
    }

    #[test]
    fn upsert_and_get_contact() {
        let conn = open_in_memory().unwrap();

        upsert_contact(
            &conn,
            "people/c111",
            "etag1",
            "Alice",
            "BEGIN:VCARD\r\nFN:Alice\r\nEND:VCARD\r\n",
            "+15550100",
        )
        .unwrap();

        let result = get_contact(&conn, "people/c111").unwrap();
        assert!(result.is_some());
        let (etag, vcard) = result.unwrap();
        assert_eq!(etag, "etag1");
        assert!(vcard.contains("FN:Alice"));
    }

    #[test]
    fn upsert_updates_existing() {
        let conn = open_in_memory().unwrap();

        upsert_contact(&conn, "people/c111", "etag1", "Alice v1", "vcard_v1", "5550100").unwrap();
        upsert_contact(&conn, "people/c111", "etag2", "Alice v2", "vcard_v2", "5550200").unwrap();

        let (etag, vcard) = get_contact(&conn, "people/c111").unwrap().unwrap();
        assert_eq!(etag, "etag2");
        assert_eq!(vcard, "vcard_v2");

        // Should still be one row, not two.
        let all = all_contacts(&conn).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn delete_contact_removes_row() {
        let conn = open_in_memory().unwrap();

        upsert_contact(&conn, "people/c111", "e1", "Alice", "vc1", "5550100").unwrap();
        upsert_contact(&conn, "people/c222", "e2", "Bob", "vc2", "5550200").unwrap();
        assert_eq!(all_contacts(&conn).unwrap().len(), 2);

        delete_contact(&conn, "people/c111").unwrap();
        assert_eq!(all_contacts(&conn).unwrap().len(), 1);

        let result = get_contact(&conn, "people/c111").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let conn = open_in_memory().unwrap();
        // Should not error.
        delete_contact(&conn, "people/c_does_not_exist").unwrap();
    }

    #[test]
    fn all_contacts_ordered_by_display_name() {
        let conn = open_in_memory().unwrap();

        upsert_contact(&conn, "people/c3", "e3", "Charlie", "vc3", "").unwrap();
        upsert_contact(&conn, "people/c1", "e1", "Alice", "vc1", "").unwrap();
        upsert_contact(&conn, "people/c2", "e2", "Bob", "vc2", "").unwrap();

        let all = all_contacts(&conn).unwrap();
        let names: Vec<&str> = all.iter().map(|(rn, _, _)| rn.as_str()).collect();
        assert_eq!(names, vec!["people/c1", "people/c2", "people/c3"]);
    }

    #[test]
    fn sync_token_persists_with_last_sync_timestamp() {
        let conn = open_in_memory().unwrap();

        set_sync_token(&conn, "tok123").unwrap();

        let last_sync: Option<String> = conn
            .query_row(
                "SELECT last_sync FROM sync_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(
            last_sync.is_some(),
            "last_sync should be set after set_sync_token"
        );
    }

    /// Simulates an incremental sync cycle: upsert some, delete some,
    /// update the sync token.
    #[test]
    fn full_sync_simulation() {
        let conn = open_in_memory().unwrap();

        // ── Full sync (first run) ────────────────────────────────
        let contacts = vec![
            ("people/c1", "e1", "Alice", "vc_alice", "5550100"),
            ("people/c2", "e2", "Bob", "vc_bob", "5550200"),
            ("people/c3", "e3", "Charlie", "vc_charlie", "5550300"),
        ];
        for (rn, etag, name, vc, phone) in &contacts {
            upsert_contact(&conn, rn, etag, name, vc, phone).unwrap();
        }
        set_sync_token(&conn, "sync_v1").unwrap();

        assert_eq!(all_contacts(&conn).unwrap().len(), 3);
        assert_eq!(
            get_sync_token(&conn).unwrap().as_deref(),
            Some("sync_v1")
        );

        // ── Incremental sync (delta) ─────────────────────────────
        // Bob was updated, Charlie was deleted, Dave was added.
        upsert_contact(&conn, "people/c2", "e2_new", "Bob Updated", "vc_bob_v2", "5550201").unwrap();
        delete_contact(&conn, "people/c3").unwrap();
        upsert_contact(&conn, "people/c4", "e4", "Dave", "vc_dave", "5550400").unwrap();
        set_sync_token(&conn, "sync_v2").unwrap();

        assert_eq!(all_contacts(&conn).unwrap().len(), 3); // Alice, Bob, Dave
        assert_eq!(
            get_sync_token(&conn).unwrap().as_deref(),
            Some("sync_v2")
        );

        // Verify Bob's update.
        let (etag, vcard) = get_contact(&conn, "people/c2").unwrap().unwrap();
        assert_eq!(etag, "e2_new");
        assert_eq!(vcard, "vc_bob_v2");

        // Verify Charlie is gone.
        assert!(get_contact(&conn, "people/c3").unwrap().is_none());
    }

    #[test]
    fn normalize_phone_strips_formatting() {
        assert_eq!(normalize_phone("+1 (555) 012-3456"), "+15550123456");
        assert_eq!(normalize_phone("555.012.3456"), "5550123456");
        assert_eq!(normalize_phone("+44 20 7946 0958"), "+442079460958");
        assert_eq!(normalize_phone(""), "");
        // Leading + only at position 0
        assert_eq!(normalize_phone("1+2"), "12");
    }

    /// Verify that three common phone-number formats all resolve to the
    /// same canonical numeric string, so a DB lookup matches regardless
    /// of how the number was originally formatted in Google Contacts.
    #[test]
    fn normalization_canonical_forms() {
        let parenthetical = normalize_phone("(555) 123-4567");
        let international = normalize_phone("+1-555-123-4567");
        let dotted = normalize_phone("555.123.4567");

        // All three must strip to the same 10-digit core.
        assert_eq!(parenthetical, "5551234567");
        assert_eq!(dotted, "5551234567");
        // International retains the leading + and country code.
        assert_eq!(international, "+15551234567");

        // A LIKE query for the 10-digit core must match ALL three.
        let conn = open_in_memory().unwrap();
        upsert_contact(&conn, "people/c10", "e1", "Parens", "vc1", &parenthetical).unwrap();
        upsert_contact(&conn, "people/c20", "e2", "Intl", "vc2", &international).unwrap();
        upsert_contact(&conn, "people/c30", "e3", "Dots", "vc3", &dotted).unwrap();

        let hits = search_by_phone(&conn, "5551234567").unwrap();
        assert_eq!(hits.len(), 3, "all three formats should match the 10-digit search");
    }

    /// Simulate a Google People API response that carries a `nextSyncToken`
    /// and two updated contacts.  Verify the SQLite database correctly
    /// reflects each change after the "sync" is applied.
    #[test]
    fn mock_sync_token_with_two_updated_contacts() {
        use crate::vcard;
        use google_people1::api::{Name, Person, PhoneNumber};

        let conn = open_in_memory().unwrap();

        // ── Helper: replicate sync.rs store_person logic ────────────
        fn store(conn: &Connection, person: &Person) {
            let rn = person.resource_name.as_deref().unwrap();
            let etag = person.etag.as_deref().unwrap_or("");
            let dn = vcard::display_name(person);
            let vc = vcard::person_to_vcard(person);
            let phones = person
                .phone_numbers
                .as_ref()
                .map(|nums| {
                    nums.iter()
                        .filter_map(|p| p.value.as_deref())
                        .map(normalize_phone)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            upsert_contact(conn, rn, etag, &dn, &vc, &phones).unwrap();
        }

        // ── "Full sync" — seed three contacts + initial token ───────
        let alice = Person {
            resource_name: Some("people/c100".into()),
            etag: Some("eA1".into()),
            names: Some(vec![Name {
                display_name: Some("Alice".into()),
                given_name: Some("Alice".into()),
                ..Default::default()
            }]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("+1-555-000-1111".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let bob = Person {
            resource_name: Some("people/c200".into()),
            etag: Some("eB1".into()),
            names: Some(vec![Name {
                display_name: Some("Bob".into()),
                given_name: Some("Bob".into()),
                ..Default::default()
            }]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("(555) 000-2222".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let charlie = Person {
            resource_name: Some("people/c300".into()),
            etag: Some("eC1".into()),
            names: Some(vec![Name {
                display_name: Some("Charlie".into()),
                given_name: Some("Charlie".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        for p in [&alice, &bob, &charlie] {
            store(&conn, p);
        }
        set_sync_token(&conn, "syncTok_FULL_v1").unwrap();

        assert_eq!(all_contacts(&conn).unwrap().len(), 3);
        assert_eq!(
            get_sync_token(&conn).unwrap().as_deref(),
            Some("syncTok_FULL_v1")
        );

        // ── "Incremental sync" response: two updated contacts ───────
        //  • Alice gets a new phone number and etag.
        //  • Bob gets a new display name and etag.
        //  • A new nextSyncToken is returned.
        let alice_v2 = Person {
            resource_name: Some("people/c100".into()),
            etag: Some("eA2".into()),
            names: Some(vec![Name {
                display_name: Some("Alice".into()),
                given_name: Some("Alice".into()),
                ..Default::default()
            }]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("+1-555-000-9999".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let bob_v2 = Person {
            resource_name: Some("people/c200".into()),
            etag: Some("eB2".into()),
            names: Some(vec![Name {
                display_name: Some("Bob Updated".into()),
                given_name: Some("Bob".into()),
                family_name: Some("Updated".into()),
                ..Default::default()
            }]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("(555) 000-2222".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        for p in [&alice_v2, &bob_v2] {
            store(&conn, p);
        }
        set_sync_token(&conn, "syncTok_INC_v2").unwrap();

        // ── Verify DB state ─────────────────────────────────────────
        // Still 3 contacts (upserts, not inserts).
        assert_eq!(all_contacts(&conn).unwrap().len(), 3);

        // Token updated.
        assert_eq!(
            get_sync_token(&conn).unwrap().as_deref(),
            Some("syncTok_INC_v2")
        );

        // Alice's etag + phone updated.
        let (etag, vcard) = get_contact(&conn, "people/c100").unwrap().unwrap();
        assert_eq!(etag, "eA2");
        assert!(
            vcard.contains("+1-555-000-9999"),
            "Alice's vCard should contain the updated phone"
        );
        let alice_hits = search_by_phone(&conn, "5550009999").unwrap();
        assert_eq!(alice_hits.len(), 1);
        // Old phone should no longer match.
        let old_hits = search_by_phone(&conn, "5550001111").unwrap();
        assert!(old_hits.is_empty(), "old phone should be replaced");

        // Bob's display name updated.
        let (etag, vcard) = get_contact(&conn, "people/c200").unwrap().unwrap();
        assert_eq!(etag, "eB2");
        assert!(
            vcard.contains("Bob Updated"),
            "Bob's vCard should reflect new display name"
        );

        // Charlie unchanged.
        let (etag, _) = get_contact(&conn, "people/c300").unwrap().unwrap();
        assert_eq!(etag, "eC1");
    }

    #[test]
    fn search_by_phone_finds_match() {
        let conn = open_in_memory().unwrap();

        upsert_contact(&conn, "people/c1", "e1", "Alice", "vc1", "+15550100 5550200").unwrap();
        upsert_contact(&conn, "people/c2", "e2", "Bob", "vc2", "5559999").unwrap();
        upsert_contact(&conn, "people/c3", "e3", "Charlie", "vc3", "").unwrap();

        let hits = search_by_phone(&conn, "5550100").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "people/c1");

        // No match
        let hits = search_by_phone(&conn, "0000000").unwrap();
        assert!(hits.is_empty());

        // Empty query returns empty
        let hits = search_by_phone(&conn, "").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn searchable_phone_is_stored_and_queryable() {
        let conn = open_in_memory().unwrap();

        upsert_contact(&conn, "people/c1", "e1", "Alice", "vc1", "+15550100 5550200").unwrap();
        upsert_contact(&conn, "people/c2", "e2", "Bob", "vc2", "5559999").unwrap();

        // Search by substring match on the searchable_phone column.
        let mut stmt = conn
            .prepare("SELECT resource_name FROM contacts WHERE searchable_phone LIKE ?1")
            .unwrap();
        let matches: Vec<String> = stmt
            .query_map(params!["%5550100%"], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(matches, vec!["people/c1"]);

        // Verify no false match.
        let matches: Vec<String> = stmt
            .query_map(params!["%0000000%"], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn oauth_token_lifecycle() {
        let conn = open_in_memory().unwrap();

        // Initially no token.
        assert!(!has_oauth_token(&conn));
        assert!(get_oauth_token(&conn).unwrap().is_none());
        assert!(get_google_email(&conn).unwrap().is_none());

        // Store a token.
        store_oauth_token(&conn, r#"{"refresh_token":"abc123"}"#, "user@gmail.com").unwrap();
        assert!(has_oauth_token(&conn));
        assert_eq!(
            get_oauth_token(&conn).unwrap().as_deref(),
            Some(r#"{"refresh_token":"abc123"}"#)
        );
        assert_eq!(
            get_google_email(&conn).unwrap().as_deref(),
            Some("user@gmail.com")
        );

        // Overwrite with new token.
        store_oauth_token(&conn, r#"{"refresh_token":"xyz789"}"#, "other@gmail.com").unwrap();
        assert_eq!(
            get_oauth_token(&conn).unwrap().as_deref(),
            Some(r#"{"refresh_token":"xyz789"}"#)
        );
        assert_eq!(
            get_google_email(&conn).unwrap().as_deref(),
            Some("other@gmail.com")
        );

        // Clear token.
        clear_oauth_token(&conn).unwrap();
        assert!(!has_oauth_token(&conn));
        assert!(get_oauth_token(&conn).unwrap().is_none());
        assert!(get_google_email(&conn).unwrap().is_none());
    }
}
