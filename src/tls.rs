//! TLS certificate lifecycle — local CA generation, trust-store installation,
//! and server TLS configuration loading.
//!
//! When the user enables HTTPS in settings, Setu generates a local Certificate
//! Authority and a server certificate signed by that CA.  The CA is installed
//! into the OS trust store so that apps trust the server certificate automatically:
//!   - **Windows**: `certutil.exe -user -addstore Root` (CurrentUser, one-time dialog)
//!   - **Linux**: `pkexec` to copy into the system trust store (password prompt)

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

/// Directory where TLS certificates are stored: `%APPDATA%/setu/`.
fn cert_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("cannot resolve %APPDATA%")?;
    Ok(base.join("setu"))
}

/// Ensure the local CA and server certificate exist on disk.
///
/// Generates them if missing; skips if all four PEM files already exist.
/// Files created:
///   - `ca.crt` / `ca.key`   — local Certificate Authority
///   - `server.crt` / `server.key` — server cert signed by the CA
pub fn ensure_certs() -> Result<()> {
    let dir = cert_dir()?;
    std::fs::create_dir_all(&dir)?;

    let ca_crt_path = dir.join("ca.crt");
    let ca_key_path = dir.join("ca.key");
    let srv_crt_path = dir.join("server.crt");
    let srv_key_path = dir.join("server.key");

    // Idempotent — skip if all files already exist.
    if ca_crt_path.exists()
        && ca_key_path.exists()
        && srv_crt_path.exists()
        && srv_key_path.exists()
    {
        tracing::info!("TLS certificates already exist — skipping generation");
        return Ok(());
    }

    tracing::info!("generating local CA and server certificate");

    // ── Generate CA ──────────────────────────────────────────────
    let ca_key_pair = rcgen::KeyPair::generate()?;

    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Setu Local CA");
    ca_params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Setu");

    let ca_cert = ca_params.self_signed(&ca_key_pair)?;

    std::fs::write(&ca_crt_path, ca_cert.pem())?;
    std::fs::write(&ca_key_path, ca_key_pair.serialize_pem())?;

    // ── Generate server cert signed by CA ────────────────────────
    let server_key_pair = rcgen::KeyPair::generate()?;

    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
    server_params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Setu CardDAV Server");

    let ca_issuer = rcgen::Issuer::from_params(&ca_params, &ca_key_pair);
    let server_cert = server_params.signed_by(&server_key_pair, &ca_issuer)?;

    std::fs::write(&srv_crt_path, server_cert.pem())?;
    std::fs::write(&srv_key_path, server_key_pair.serialize_pem())?;

    tracing::info!(
        ca_crt = %ca_crt_path.display(),
        srv_crt = %srv_crt_path.display(),
        "TLS certificates generated"
    );

    Ok(())
}

/// Install the local CA certificate into the Windows CurrentUser trust store.
///
/// Runs `certutil.exe -user -addstore Root <ca.crt>`, which triggers a
/// one-time Windows security confirmation dialog.  No admin rights needed.
///
/// On non-Windows platforms this is a no-op.
#[cfg(target_os = "windows")]
pub fn install_ca_to_trust_store() -> Result<()> {
    let ca_crt = cert_dir()?.join("ca.crt");
    if !ca_crt.exists() {
        anyhow::bail!("CA certificate not found at {}", ca_crt.display());
    }

    tracing::info!("installing CA into Windows CurrentUser trust store");

    let status = std::process::Command::new("certutil.exe")
        .args(["-user", "-addstore", "Root"])
        .arg(&ca_crt)
        .status()
        .context("failed to run certutil.exe")?;

    if !status.success() {
        anyhow::bail!("certutil.exe exited with status {status}");
    }

    tracing::info!("CA certificate installed successfully");
    Ok(())
}

/// Install the local CA certificate into the Linux system trust store.
///
/// Detects the distro-specific trust store directory and update command:
///   - Debian/Ubuntu:  `/usr/local/share/ca-certificates/` + `update-ca-certificates`
///   - RHEL/Fedora:    `/etc/pki/ca-trust/source/anchors/` + `update-ca-trust`
///   - Arch:           `/etc/ca-certificates/trust-source/anchors/` + `trust extract-compat`
///
/// Uses `pkexec` for privilege elevation (PolicyKit GUI password prompt).
#[cfg(target_os = "linux")]
pub fn install_ca_to_trust_store() -> Result<()> {
    let ca_crt = cert_dir()?.join("ca.crt");
    if !ca_crt.exists() {
        anyhow::bail!("CA certificate not found at {}", ca_crt.display());
    }

    tracing::info!("installing CA into system trust store");

    // Detect trust store layout.
    let (target_dir, update_cmd) =
        if std::path::Path::new("/usr/local/share/ca-certificates").exists() {
            // Debian / Ubuntu / derivatives
            ("/usr/local/share/ca-certificates", "update-ca-certificates")
        } else if std::path::Path::new("/etc/pki/ca-trust/source/anchors").exists() {
            // RHEL / Fedora / SUSE / CentOS
            ("/etc/pki/ca-trust/source/anchors", "update-ca-trust")
        } else if std::path::Path::new("/etc/ca-certificates/trust-source/anchors").exists() {
            // Arch Linux
            (
                "/etc/ca-certificates/trust-source/anchors",
                "trust extract-compat",
            )
        } else {
            anyhow::bail!(
                "Could not detect system CA trust store. \
                 Manually install {} as a trusted CA certificate.",
                ca_crt.display()
            );
        };

    let ca_crt_str = ca_crt
        .to_str()
        .context("CA certificate path is not valid UTF-8")?;
    let script = format!(
        "cp -- '{}' '{}/setu-local-ca.crt' && {}",
        ca_crt_str, target_dir, update_cmd
    );

    let status = std::process::Command::new("pkexec")
        .args(["sh", "-c", &script])
        .status()
        .context("failed to run pkexec — is PolicyKit installed?")?;

    if !status.success() {
        anyhow::bail!("CA trust store update failed (exit status: {status})");
    }

    tracing::info!("CA certificate installed successfully");
    Ok(())
}

/// Load the server TLS configuration from disk.
///
/// Reads `server.crt` and `server.key` and builds a `rustls::ServerConfig`
/// suitable for HTTPS on localhost.
pub fn load_server_tls_config() -> Result<Arc<rustls::ServerConfig>> {
    let dir = cert_dir()?;
    let cert_pem = std::fs::read(&dir.join("server.crt"))
        .context("reading server.crt")?;
    let key_pem = std::fs::read(&dir.join("server.key"))
        .context("reading server.key")?;

    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &cert_pem[..])
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("parsing server.crt PEM")?;

    let key = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("parsing server.key PEM")?
        .context("no private key found in server.key")?;

    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building ServerConfig")?;

    config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(Arc::new(config))
}
