//! Read-only CardDAV server — serves Google Contacts from SQLite as vCards.
//!
//! Discovery chain (RFC 6764 / RFC 6352):
//!   GET  /.well-known/carddav          → 301 /
//!   PROPFIND /                          → current-user-principal → /principals/
//!   PROPFIND /principals/               → addressbook-home-set  → /addressbook/
//!   PROPFIND /addressbook/   (Depth:0)  → address book properties
//!   PROPFIND /addressbook/   (Depth:1)  → properties + per-contact entries
//!   REPORT  /addressbook/               → addressbook-multiget or addressbook-query
//!   GET     /addressbook/<id>.vcf       → individual vCard 3.0
//!
//! On-demand search (for OpenBubbles / phone-number lookup):
//!   When an addressbook-query REPORT includes a TEL `prop-filter` and no
//!   local match is found, the server queries Google People API in real-time,
//!   caches the result in SQLite, and returns it immediately.

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::any,
    Router,
};
use std::sync::Arc;

use crate::db;
use crate::google_api::GoogleApi;
use crate::vault::SecureVault;

// ── Shared application state ────────────────────────────────────────────

/// State shared across all axum handlers via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    /// `None` when Google credentials are not configured.
    pub google_api: Option<GoogleApi>,
    /// Hex-encoded SQLCipher encryption key.
    pub db_key: String,
    /// Vault handle — reads CardDAV password from keyring on each request.
    pub vault: SecureVault,
}

// ── Public entry point ───────────────────────────────────────────────────

/// Start the CardDAV server on `127.0.0.1:{port}`.
///
/// When `tls_config` is `Some`, the server accepts HTTPS connections using the
/// provided `rustls::ServerConfig`.  When `None`, it listens on plain HTTP
/// (the default, backward-compatible behaviour).
pub async fn start_carddav_server(
    port: u16,
    google_api: Option<GoogleApi>,
    db_key: String,
    vault: SecureVault,
    tls_config: Option<Arc<rustls::ServerConfig>>,
) -> Result<()> {
    let state = AppState {
        google_api,
        db_key,
        vault,
    };

    let app = Router::new()
        .route("/.well-known/carddav", any(well_known))
        .route("/", any(root_handler))
        .route("/principals/", any(principals_handler))
        .route("/addressbook/", any(addressbook_handler))
        .route("/addressbook/{id}", any(contact_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            basic_auth_middleware,
        ))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    match tls_config {
        Some(tls_cfg) => {
            tracing::info!(%addr, "CardDAV server listening (HTTPS)");
            serve_tls(listener, app, tls_cfg).await
        }
        None => {
            tracing::info!(%addr, "CardDAV server listening (HTTP)");
            axum::serve(listener, app).await?;
            Ok(())
        }
    }
}

/// Manual TLS accept loop using `tokio-rustls` + `hyper-util` auto-builder.
///
/// Based on axum's low-level-rustls example.
async fn serve_tls(
    listener: tokio::net::TcpListener,
    app: Router,
    tls_config: Arc<rustls::ServerConfig>,
) -> Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto;
    use tower_service::Service;

    let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

    loop {
        let (tcp_stream, remote_addr) = listener.accept().await?;

        let tls_acceptor = tls_acceptor.clone();
        let app = app.clone();

        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(%remote_addr, "TLS handshake failed: {e}");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);

            let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let svc = app.clone();
                async move {
                    let mut svc = svc;
                    Service::call(&mut svc, req).await
                }
            });

            if let Err(e) = auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
            {
                tracing::debug!(%remote_addr, "connection error: {e}");
            }
        });
    }
}

// ── Basic Auth middleware ────────────────────────────────────────────────

async fn basic_auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    // OPTIONS requests pass through without auth (DAV discovery).
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }

    // Read the current password from the OS keyring on every request,
    // so changes in Settings take effect without restarting.
    let expected_pw = match state.vault.get_or_init_carddav_password() {
        Ok(pw) => pw,
        Err(e) => {
            tracing::error!("failed to read CardDAV password from keyring: {e:#}");
            return internal_error();
        }
    };

    if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(encoded) = auth_str.strip_prefix("Basic ") {
                if let Ok(decoded) = base64_decode(encoded) {
                    if let Some((_user, password)) = decoded.split_once(':') {
                        if password == expected_pw {
                            return next.run(req).await;
                        }
                    }
                }
            }
        }
    }

    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Basic realm=\"Setu CardDAV\"")
        .body(Body::from("Unauthorized"))
        .unwrap()
}

fn base64_decode(input: &str) -> std::result::Result<String, ()> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| ())?;
    String::from_utf8(bytes).map_err(|_| ())
}

// ── Well-known redirect (RFC 6764) ───────────────────────────────────────

async fn well_known() -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, "/")
        .body(Body::empty())
        .unwrap()
}

// ── Root (/) — current-user-principal discovery ──────────────────────────

async fn root_handler(req: Request) -> Response {
    tracing::info!(method = %req.method(), "/ request");
    match *req.method() {
        Method::OPTIONS => options_response(),
        _ if req.method().as_str() == "PROPFIND" => root_propfind(),
        _ => method_not_allowed(),
    }
}

fn root_propfind() -> Response {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:response>
    <D:href>/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype>
          <D:collection/>
        </D:resourcetype>
        <D:current-user-principal>
          <D:href>/principals/</D:href>
        </D:current-user-principal>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

    multistatus_response(xml)
}

// ── Principals (/principals/) — addressbook-home-set ─────────────────────

async fn principals_handler(req: Request) -> Response {
    tracing::info!(method = %req.method(), "/principals/ request");
    match *req.method() {
        Method::OPTIONS => options_response(),
        _ if req.method().as_str() == "PROPFIND" => principals_propfind(),
        _ => method_not_allowed(),
    }
}

fn principals_propfind() -> Response {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:response>
    <D:href>/principals/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype>
          <D:collection/>
        </D:resourcetype>
        <C:addressbook-home-set>
          <D:href>/addressbook/</D:href>
        </C:addressbook-home-set>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

    multistatus_response(xml)
}

// ── Address book (/addressbook/) ─────────────────────────────────────────

async fn addressbook_handler(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let depth = req
        .headers()
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("0")
        .to_string();

    tracing::info!(
        method = %method,
        depth = %depth,
        user_agent = req.headers().get("User-Agent").and_then(|v| v.to_str().ok()).unwrap_or("-"),
        "CardDAV /addressbook/ request"
    );

    match method.as_str() {
        "OPTIONS" => options_response(),
        "PROPFIND" => addressbook_propfind(&depth, &state.db_key),
        "REPORT" => addressbook_report(req, state.google_api, &state.db_key).await,
        _ => method_not_allowed(),
    }
}

/// PROPFIND on the address book collection.
///
/// - **Depth: 0** — return only the collection's own properties.
/// - **Depth: 1** — return the collection *plus* one entry per contact.
fn addressbook_propfind(depth: &str, db_key: &str) -> Response {
    let contacts = match db::open(Some(db_key)).and_then(|conn| db::all_contacts(&conn)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("DB error in PROPFIND: {e:#}");
            return internal_error();
        }
    };

    let ctag = chrono::Utc::now().timestamp().to_string();

    let mut xml = String::with_capacity(4096);
    xml.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav" xmlns:CS="http://calendarserver.org/ns/">
  <D:response>
    <D:href>/addressbook/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype>
          <D:collection/>
          <C:addressbook/>
        </D:resourcetype>
        <D:displayname>Google Contacts</D:displayname>
        <CS:getctag>"#,
    );
    xml.push_str(&xml_escape(&ctag));
    xml.push_str(
        r#"</CS:getctag>
        <D:supported-report-set>
          <D:supported-report>
            <D:report><C:addressbook-multiget/></D:report>
          </D:supported-report>
          <D:supported-report>
            <D:report><C:addressbook-query/></D:report>
          </D:supported-report>
        </D:supported-report-set>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
"#,
    );

    // Depth: 1 — include each contact as a child resource.
    if depth == "1" || depth == "infinity" {
        for (resource_name, etag, _vcard) in &contacts {
            let href = contact_href(resource_name);
            xml.push_str("  <D:response>\n    <D:href>");
            xml.push_str(&xml_escape(&href));
            xml.push_str("</D:href>\n    <D:propstat>\n      <D:prop>\n");
            xml.push_str("        <D:getetag>\"");
            xml.push_str(&xml_escape(etag));
            xml.push_str("\"</D:getetag>\n");
            xml.push_str(
                "        <D:getcontenttype>text/vcard;charset=utf-8</D:getcontenttype>\n",
            );
            xml.push_str("        <D:resourcetype/>\n");
            xml.push_str("      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n");
            xml.push_str("    </D:propstat>\n  </D:response>\n");
        }
    }

    tracing::info!(depth = depth, contact_count = contacts.len(), "PROPFIND /addressbook/ response");

    xml.push_str("</D:multistatus>");
    multistatus_response(&xml)
}

/// REPORT on the address book — handles `addressbook-multiget`, generic
/// `addressbook-query`, and **on-demand TEL search** with Google fallback.
///
/// On-demand flow (when a TEL `prop-filter` is present):
///   1. Normalise the phone number from the filter.
///   2. Search the local SQLite `searchable_phone` column.
///   3. If no local hit **and** a `GoogleApi` is available, call
///      `search_by_phone` in real-time.
///   4. Upsert the Google result into SQLite with a fresh ETag.
///   5. Return the standard multistatus XML containing the vCard.
async fn addressbook_report(req: Request, google_api: Option<GoogleApi>, db_key: &str) -> Response {
    let body_bytes = match axum::body::to_bytes(req.into_body(), 1024 * 64).await {
        Ok(b) => b,
        Err(_) => return bad_request("request body too large"),
    };
    let body_str = String::from_utf8_lossy(&body_bytes);

    tracing::info!(body = %body_str, "REPORT request body");

    let is_multiget = body_str.contains("addressbook-multiget");

    // ── addressbook-multiget: filter by href list ───────────────────
    if is_multiget {
        let contacts = match db::open(Some(db_key)).and_then(|conn| db::all_contacts(&conn)) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB error in REPORT: {e:#}");
                return internal_error();
            }
        };

        let requested_hrefs = extract_hrefs(&body_str);

        let filtered: Vec<&(String, String, String)> = if requested_hrefs.is_empty() {
            contacts.iter().collect()
        } else {
            contacts
                .iter()
                .filter(|(rn, _, _)| {
                    let href = contact_href(rn);
                    requested_hrefs.iter().any(|rh| rh == &href)
                })
                .collect()
        };

        return build_report_xml(&filtered);
    }

    // ── addressbook-query: check for TEL prop-filter ────────────────
    let tel_filter = extract_tel_filter(&body_str);

    if let Some(ref raw_phone) = tel_filter {
        let normalized = db::normalize_phone(raw_phone);
        tracing::debug!(raw = raw_phone, normalized = %normalized, "TEL prop-filter in REPORT");

        if !normalized.is_empty() {
            // 1. Local DB search
            let local_hits = match db::open(Some(db_key)).and_then(|conn| db::search_by_phone(&conn, &normalized)) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("DB error in phone search: {e:#}");
                    return internal_error();
                }
            };

            if !local_hits.is_empty() {
                return build_report_xml_owned(&local_hits);
            }

            // 2. Google fallback (on-demand)
            if let Some(ref api) = google_api {
                tracing::info!(phone = raw_phone, "no local match — querying Google");
                match api.search_by_phone(raw_phone).await {
                    Ok(Some(person)) => {
                        let contact = match cache_person(&person, db_key) {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::error!("failed to cache Google result: {e:#}");
                                return internal_error();
                            }
                        };
                        return build_report_xml_owned(&[contact]);
                    }
                    Ok(None) => {
                        tracing::debug!(phone = raw_phone, "Google search returned no results");
                    }
                    Err(e) => {
                        tracing::error!("Google search failed: {e:#}");
                    }
                }
            }

            // TEL filter was present but no match found — return empty result.
            return build_report_xml_owned(&[]);
        }
    }

    // ── Generic addressbook-query (no TEL filter, or fallback) ──────
    let contacts = match db::open(Some(db_key)).and_then(|conn| db::all_contacts(&conn)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("DB error in REPORT: {e:#}");
            return internal_error();
        }
    };

    let all_refs: Vec<&(String, String, String)> = contacts.iter().collect();
    build_report_xml(&all_refs)
}

/// Upsert a Google `Person` into the local DB and return `(resource_name, etag, vcard)`.
fn cache_person(person: &google_people1::api::Person, db_key: &str) -> Result<(String, String, String)> {
    let conn = db::open(Some(db_key))?;
    cache_person_to_conn(&conn, person)
}

/// Testable core of [`cache_person`]: converts a Google `Person` to a vCard,
/// normalises phone numbers, upserts the row, and returns the tuple needed
/// for the multistatus XML response.
fn cache_person_to_conn(
    conn: &rusqlite::Connection,
    person: &google_people1::api::Person,
) -> Result<(String, String, String)> {
    let resource_name = person
        .resource_name
        .as_deref()
        .unwrap_or("unknown")
        .to_string();

    let etag = person
        .etag
        .as_deref()
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let display_name = crate::vcard::display_name(person);
    let vcard_text = crate::vcard::person_to_vcard(person);

    let searchable_phone = person
        .phone_numbers
        .as_ref()
        .map(|phones| {
            phones
                .iter()
                .filter_map(|p| p.value.as_deref())
                .map(db::normalize_phone)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();

    db::upsert_contact(
        conn,
        &resource_name,
        &etag,
        &display_name,
        &vcard_text,
        &searchable_phone,
    )?;

    tracing::info!(
        resource_name = %resource_name,
        display_name = %display_name,
        "cached on-demand Google contact"
    );

    Ok((resource_name, etag, vcard_text))
}

// ── Individual contact (/addressbook/<id>.vcf) ──────────────────────────

async fn contact_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let method = req.method().clone();
    let depth = req
        .headers()
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("0")
        .to_string();

    match method.as_str() {
        "GET" | "HEAD" => contact_get(&id, &state.db_key),
        "PROPFIND" => contact_propfind(&id, &depth, &state.db_key),
        "OPTIONS" => options_response(),
        _ => method_not_allowed(),
    }
}

fn contact_get(id: &str, db_key: &str) -> Response {
    let resource_name = id_to_resource_name(id);
    tracing::info!(resource_name = %resource_name, "GET /addressbook/{id}");

    let conn = match db::open(Some(db_key)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("DB error: {e:#}");
            return internal_error();
        }
    };

    match db::get_contact(&conn, &resource_name) {
        Ok(Some((etag, vcard))) => {
            tracing::info!(resource_name = %resource_name, etag = %etag, len = vcard.len(), "GET response → 200");
            tracing::debug!(vcard = %vcard, "GET vCard body");
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/vcard;charset=utf-8")
                .header(header::ETAG, format!("\"{etag}\""))
                .body(Body::from(vcard))
                .unwrap()
        }
        Ok(None) => {
            tracing::info!(resource_name = %resource_name, "GET response → 404");
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Not Found"))
                .unwrap()
        }
        Err(e) => {
            tracing::error!("DB error: {e:#}");
            internal_error()
        }
    }
}

fn contact_propfind(id: &str, _depth: &str, db_key: &str) -> Response {
    let resource_name = id_to_resource_name(id);
    let conn = match db::open(Some(db_key)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("DB error: {e:#}");
            return internal_error();
        }
    };

    match db::get_contact(&conn, &resource_name) {
        Ok(Some((etag, vcard))) => {
            let href = format!("/addressbook/{id}");
            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:response>
    <D:href>{href}</D:href>
    <D:propstat>
      <D:prop>
        <D:getetag>"{etag}"</D:getetag>
        <D:getcontenttype>text/vcard;charset=utf-8</D:getcontenttype>
        <D:getcontentlength>{len}</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#,
                href = xml_escape(&href),
                etag = xml_escape(&etag),
                len = vcard.len(),
            );
            multistatus_response(&xml)
        }
        Ok(None) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap(),
        Err(e) => {
            tracing::error!("DB error: {e:#}");
            internal_error()
        }
    }
}

// ── Response builders ────────────────────────────────────────────────────

/// Build a standard REPORT multistatus response from a slice of borrowed tuples.
fn build_report_xml(contacts: &[&(String, String, String)]) -> Response {
    let names: Vec<&str> = contacts.iter().map(|(rn, _, _)| rn.as_str()).collect();
    tracing::info!(count = contacts.len(), contacts = ?names, "REPORT response");

    let mut xml = String::with_capacity(contacts.len() * 2048);
    xml.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
"#,
    );

    for (resource_name, etag, vcard) in contacts {
        append_contact_response(&mut xml, resource_name, etag, vcard);
    }

    xml.push_str("</D:multistatus>");

    tracing::debug!(body = %xml, "REPORT response body");
    multistatus_response(&xml)
}

/// Build a standard REPORT multistatus response from a slice of owned tuples.
fn build_report_xml_owned(contacts: &[(String, String, String)]) -> Response {
    let names: Vec<&str> = contacts.iter().map(|(rn, _, _)| rn.as_str()).collect();
    tracing::info!(count = contacts.len(), contacts = ?names, "REPORT response");

    let mut xml = String::with_capacity(contacts.len() * 2048);
    xml.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
"#,
    );

    for (resource_name, etag, vcard) in contacts {
        append_contact_response(&mut xml, resource_name, etag, vcard);
    }

    xml.push_str("</D:multistatus>");

    tracing::debug!(body = %xml, "REPORT response body");
    multistatus_response(&xml)
}

/// Append a single `<D:response>` element for a contact to the XML buffer.
fn append_contact_response(xml: &mut String, resource_name: &str, etag: &str, vcard: &str) {
    let href = contact_href(resource_name);
    xml.push_str("  <D:response>\n    <D:href>");
    xml.push_str(&xml_escape(&href));
    xml.push_str("</D:href>\n    <D:propstat>\n      <D:prop>\n");
    xml.push_str("        <D:getetag>\"");
    xml.push_str(&xml_escape(etag));
    xml.push_str("\"</D:getetag>\n");
    xml.push_str("        <C:address-data>");
    xml.push_str(&xml_escape(vcard));
    xml.push_str("</C:address-data>\n");
    xml.push_str("      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n");
    xml.push_str("    </D:propstat>\n  </D:response>\n");
}

fn multistatus_response(xml: &str) -> Response {
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml;charset=utf-8")
        .header("DAV", "1, 3, addressbook")
        .body(Body::from(xml.to_string()))
        .unwrap()
}

fn options_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Allow", "OPTIONS, GET, HEAD, PROPFIND, REPORT")
        .header("DAV", "1, 3, addressbook")
        .body(Body::empty())
        .unwrap()
}

fn method_not_allowed() -> Response {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .body(Body::from("Method Not Allowed"))
        .unwrap()
}

fn internal_error() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from("Internal Server Error"))
        .unwrap()
}

fn bad_request(msg: &str) -> Response {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(msg.to_string()))
        .unwrap()
}

// ── URL / resource-name helpers ──────────────────────────────────────────

/// Convert a Google resource name (`people/c123`) to a CardDAV href.
fn contact_href(resource_name: &str) -> String {
    let safe = resource_name.replace('/', "_");
    format!("/addressbook/{safe}.vcf")
}

/// Reverse of `contact_href`: `people_c123.vcf` → `people/c123`.
fn id_to_resource_name(id: &str) -> String {
    id.trim_end_matches(".vcf").replace('_', "/")
}

/// Minimal XML escaping for attribute/text values.
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Extract `<D:href>` values from a REPORT request body.
fn extract_hrefs(xml: &str) -> Vec<String> {
    let mut hrefs = Vec::new();
    // Match both <D:href> and <href> (some clients omit the namespace prefix).
    for tag_open in &["<D:href>", "<href>"] {
        let tag_close = tag_open.replace('<', "</");
        let mut pos = 0;
        while let Some(start) = xml[pos..].find(tag_open) {
            let abs_start = pos + start + tag_open.len();
            if let Some(end) = xml[abs_start..].find(&tag_close) {
                let href = xml[abs_start..abs_start + end].trim().to_string();
                if !href.is_empty() {
                    hrefs.push(href);
                }
                pos = abs_start + end + tag_close.len();
            } else {
                break;
            }
        }
    }
    hrefs
}

/// Extract the phone number from a `<C:prop-filter name="TEL">` element
/// inside an `addressbook-query` REPORT body.
///
/// Handles both namespaced (`<C:prop-filter>`, `<C:text-match>`) and
/// non-namespaced variants.
///
/// Example XML:
/// ```xml
/// <C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
///   <D:prop><D:getetag/><C:address-data/></D:prop>
///   <C:filter>
///     <C:prop-filter name="TEL">
///       <C:text-match collation="i;unicode-casemap" match-type="contains">
///         5551234567
///       </C:text-match>
///     </C:prop-filter>
///   </C:filter>
/// </C:addressbook-query>
/// ```
fn extract_tel_filter(xml: &str) -> Option<String> {
    // Look for prop-filter with name="TEL" (with or without C: prefix).
    let tel_marker_patterns = [
        "prop-filter name=\"TEL\"",
        "prop-filter name='TEL'",
    ];

    let tel_pos = tel_marker_patterns
        .iter()
        .filter_map(|pat| xml.find(pat))
        .min()?;

    // Now find the text-match content after this position.
    let after_tel = &xml[tel_pos..];

    // Try both namespaced and non-namespaced text-match tags.
    for tag_open in &["<C:text-match", "<text-match"] {
        let tag_close = if tag_open.starts_with("<C:") {
            "</C:text-match>"
        } else {
            "</text-match>"
        };

        if let Some(open_start) = after_tel.find(tag_open) {
            let after_open = &after_tel[open_start..];
            // Skip past the opening tag (find the closing >)
            if let Some(gt_pos) = after_open.find('>') {
                let content_start = gt_pos + 1;
                if let Some(close_pos) = after_open[content_start..].find(tag_close) {
                    let value = after_open[content_start..content_start + close_pos]
                        .trim()
                        .to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
        }
    }

    None
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use google_people1::api::{Name, Person, PhoneNumber};

    #[test]
    fn test_contact_href_roundtrip() {
        let rn = "people/c1234567890";
        let href = contact_href(rn);
        assert_eq!(href, "/addressbook/people_c1234567890.vcf");

        let recovered = id_to_resource_name("people_c1234567890.vcf");
        assert_eq!(recovered, rn);
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("a<b>c&d\"e"), "a&lt;b&gt;c&amp;d&quot;e");
        assert_eq!(xml_escape("plain text"), "plain text");
    }

    #[test]
    fn test_extract_hrefs_namespaced() {
        let xml = r#"<?xml version="1.0"?>
<C:addressbook-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><D:getetag/><C:address-data/></D:prop>
  <D:href>/addressbook/people_c111.vcf</D:href>
  <D:href>/addressbook/people_c222.vcf</D:href>
</C:addressbook-multiget>"#;
        let hrefs = extract_hrefs(xml);
        assert_eq!(
            hrefs,
            vec![
                "/addressbook/people_c111.vcf",
                "/addressbook/people_c222.vcf"
            ]
        );
    }

    #[test]
    fn test_extract_hrefs_no_namespace() {
        let xml = r#"<addressbook-multiget>
  <href>/addressbook/people_c999.vcf</href>
</addressbook-multiget>"#;
        let hrefs = extract_hrefs(xml);
        assert_eq!(hrefs, vec!["/addressbook/people_c999.vcf"]);
    }

    #[test]
    fn test_extract_tel_filter_namespaced() {
        let xml = r#"<?xml version="1.0"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><D:getetag/><C:address-data/></D:prop>
  <C:filter>
    <C:prop-filter name="TEL">
      <C:text-match collation="i;unicode-casemap" match-type="contains">5551234567</C:text-match>
    </C:prop-filter>
  </C:filter>
</C:addressbook-query>"#;
        assert_eq!(extract_tel_filter(xml), Some("5551234567".into()));
    }

    #[test]
    fn test_extract_tel_filter_no_namespace() {
        let xml = r#"<addressbook-query>
  <filter>
    <prop-filter name="TEL">
      <text-match match-type="contains">+1-555-999-0000</text-match>
    </prop-filter>
  </filter>
</addressbook-query>"#;
        assert_eq!(extract_tel_filter(xml), Some("+1-555-999-0000".into()));
    }

    #[test]
    fn test_extract_tel_filter_missing() {
        let xml = r#"<C:addressbook-query xmlns:C="urn:ietf:params:xml:ns:carddav">
  <C:filter>
    <C:prop-filter name="FN">
      <C:text-match>John</C:text-match>
    </C:prop-filter>
  </C:filter>
</C:addressbook-query>"#;
        assert_eq!(extract_tel_filter(xml), None);
    }

    #[test]
    fn test_extract_tel_filter_empty_body() {
        assert_eq!(extract_tel_filter(""), None);
        assert_eq!(extract_tel_filter("<empty/>"), None);
    }

    // ── Test 3: XML Generation ──────────────────────────────────────

    /// Verify that PROPFIND responses carry the mandatory CardDAV headers
    /// (`DAV: 1, 3, addressbook`) and 207 Multi-Status code.
    #[test]
    fn test_propfind_response_headers() {
        let resp = root_propfind();
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        assert_eq!(
            resp.headers().get("DAV").unwrap().to_str().unwrap(),
            "1, 3, addressbook"
        );
        assert!(resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/xml"));
    }

    /// Verify the root PROPFIND XML contains the discovery chain elements
    /// that clients rely on: `current-user-principal` → `/principals/`.
    #[tokio::test]
    async fn test_propfind_root_xml_structure() {
        let resp = root_propfind();
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let xml = std::str::from_utf8(&body).unwrap();

        // Must be well-formed (starts with XML declaration, has matching root tag).
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("</D:multistatus>"));

        // Discovery chain.
        assert!(xml.contains("<D:current-user-principal>"));
        assert!(xml.contains("<D:href>/principals/</D:href>"));
    }

    /// Verify the principals PROPFIND points to the addressbook-home-set.
    #[tokio::test]
    async fn test_propfind_principals_xml_structure() {
        let resp = principals_propfind();
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let xml = std::str::from_utf8(&body).unwrap();

        assert!(xml.contains("<C:addressbook-home-set>"));
        assert!(xml.contains("<D:href>/addressbook/</D:href>"));
    }

    /// Verify that `build_report_xml_owned` produces valid multistatus XML
    /// containing the vCard data and correct hrefs / etags for each contact.
    #[tokio::test]
    async fn test_report_xml_generation() {
        let contacts = vec![
            (
                "people/c111".to_string(),
                "etag_aaa".to_string(),
                "BEGIN:VCARD\r\nFN:Alice\r\nEND:VCARD\r\n".to_string(),
            ),
            (
                "people/c222".to_string(),
                "etag_bbb".to_string(),
                "BEGIN:VCARD\r\nFN:Bob\r\nEND:VCARD\r\n".to_string(),
            ),
        ];

        let resp = build_report_xml_owned(&contacts);

        // Mandatory headers.
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        assert_eq!(
            resp.headers().get("DAV").unwrap().to_str().unwrap(),
            "1, 3, addressbook"
        );

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let xml = std::str::from_utf8(&body).unwrap();

        // Well-formed root element.
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("</D:multistatus>"));

        // Two <D:response> entries.
        assert_eq!(
            xml.matches("<D:response>").count(),
            2,
            "expected exactly two <D:response> entries"
        );

        // Contact hrefs.
        assert!(xml.contains("/addressbook/people_c111.vcf"));
        assert!(xml.contains("/addressbook/people_c222.vcf"));

        // ETags (XML-escaped with surrounding quotes).
        assert!(xml.contains("\"etag_aaa\""));
        assert!(xml.contains("\"etag_bbb\""));

        // vCard data (XML-escaped, so BEGIN:VCARD becomes BEGIN:VCARD since
        // it has no special XML chars).
        assert!(xml.contains("<C:address-data>"));
        assert!(xml.contains("FN:Alice"));
        assert!(xml.contains("FN:Bob"));
    }

    // ── Test 4: Reactive Search Mock ────────────────────────────────

    /// End-to-end simulation of the on-demand search flow:
    ///
    ///   1. An `addressbook-query` REPORT arrives with a TEL `prop-filter`.
    ///   2. The phone is parsed and normalised.
    ///   3. Local DB search → miss (the number is not cached yet).
    ///   4. A mock Google `Person` is fed to `cache_person_to_conn`.
    ///   5. The DB is now populated — `search_by_phone` returns the contact.
    ///   6. The resulting multistatus XML includes the new vCard.
    #[tokio::test]
    async fn test_reactive_search_full_flow() {
        // ── 1. Parse the TEL filter from a realistic REPORT body ────
        let report_body = r#"<?xml version="1.0"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><D:getetag/><C:address-data/></D:prop>
  <C:filter>
    <C:prop-filter name="TEL">
      <C:text-match collation="i;unicode-casemap"
                    match-type="contains">+1 (555) 987-6543</C:text-match>
    </C:prop-filter>
  </C:filter>
</C:addressbook-query>"#;

        let raw_phone = extract_tel_filter(report_body)
            .expect("TEL filter should be parsed");
        assert_eq!(raw_phone, "+1 (555) 987-6543");

        // ── 2. Normalise ────────────────────────────────────────────
        let normalized = db::normalize_phone(&raw_phone);
        assert_eq!(normalized, "+15559876543");

        // ── 3. Local DB search → miss ───────────────────────────────
        let conn = db::open_in_memory().unwrap();
        let hits = db::search_by_phone(&conn, "5559876543").unwrap();
        assert!(hits.is_empty(), "DB should be empty before caching");

        // ── 4. Simulate Google returning a Person, cache it ─────────
        let google_person = Person {
            resource_name: Some("people/c98765".into()),
            etag: Some("google_etag_xyz".into()),
            names: Some(vec![Name {
                display_name: Some("Eve Searcher".into()),
                given_name: Some("Eve".into()),
                family_name: Some("Searcher".into()),
                ..Default::default()
            }]),
            phone_numbers: Some(vec![PhoneNumber {
                value: Some("+1 (555) 987-6543".into()),
                type_: Some("mobile".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let (rn, etag, vcard) = cache_person_to_conn(&conn, &google_person)
            .expect("cache_person_to_conn should succeed");

        assert_eq!(rn, "people/c98765");
        assert_eq!(etag, "google_etag_xyz");
        assert!(vcard.contains("FN:Eve Searcher"));
        assert!(vcard.contains("TEL;"));

        // ── 5. Local DB search → hit ────────────────────────────────
        let hits = db::search_by_phone(&conn, "5559876543").unwrap();
        assert_eq!(hits.len(), 1, "contact should now be cached");
        assert_eq!(hits[0].0, "people/c98765");
        assert!(hits[0].2.contains("Eve Searcher"));

        // Also verify via get_contact.
        let (db_etag, db_vcard) = db::get_contact(&conn, "people/c98765")
            .unwrap()
            .expect("contact should exist in DB");
        assert_eq!(db_etag, "google_etag_xyz");
        assert!(db_vcard.contains("BEGIN:VCARD"));
        assert!(db_vcard.contains("END:VCARD"));

        // ── 6. Build the multistatus XML and verify ─────────────────
        let resp = build_report_xml_owned(&hits);

        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        assert_eq!(
            resp.headers().get("DAV").unwrap().to_str().unwrap(),
            "1, 3, addressbook"
        );

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let xml = std::str::from_utf8(&body).unwrap();

        // The response should contain exactly one contact entry.
        assert_eq!(xml.matches("<D:response>").count(), 1);
        assert!(xml.contains("/addressbook/people_c98765.vcf"));
        assert!(xml.contains("google_etag_xyz"));
        assert!(xml.contains("<C:address-data>"));
        assert!(xml.contains("Eve Searcher"));
    }

    // ── Basic Auth tests ────────────────────────────────────────────

    #[test]
    fn test_base64_decode_valid() {
        use base64::Engine;
        // "user:mypassword" → base64
        let encoded = base64::engine::general_purpose::STANDARD
            .encode("user:mypassword");
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, "user:mypassword");
    }

    #[test]
    fn test_base64_decode_invalid() {
        assert!(base64_decode("!!!not-valid!!!").is_err());
    }
}
