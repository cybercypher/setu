//! Setu — CardDAV bridge for Google Contacts.
//!
//! Launch modes:
//!   setu              → start (tray on Windows/GUI builds, headless otherwise)
//!   setu --settings   → open the settings GUI (requires "gui" feature)
//!   setu --headless   → run without tray (CardDAV server + sync only)
//!   setu --install    → install systemd user service (Linux only)
//!   setu --uninstall  → remove systemd user service (Linux only)

// Hide the console window on Windows release builds.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

// Modules shared with the lib crate (for testability).
use setu_lib::{auth, config, db, google_api, server, vault};

// GUI modules (only compiled with the "gui" feature).
#[cfg(feature = "gui")]
mod settings;
#[cfg(feature = "gui")]
mod tray;

// Always-available modules.
mod sync;
mod vcard;

#[cfg(target_os = "linux")]
use anyhow::Context;
use std::sync::Mutex;

fn main() -> anyhow::Result<()> {
    // ── TLS crypto provider (must be first) ──────────────────────
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    // ── Logging to file ──────────────────────────────────────────
    let log_dir = dirs::data_dir()
        .expect("cannot resolve data directory")
        .join("setu");
    std::fs::create_dir_all(&log_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("setu.log"))?;

    tracing_subscriber::fmt()
        .with_writer(Mutex::new(log_file))
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "setu=info".into()),
        )
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        build = env!("SETU_BUILD_ID"),
        "Setu starting"
    );

    // ── Vault (OS keyring) ─────────────────────────────────────────
    let vault = vault::SecureVault;

    // ── One-time migration from wincard → setu ────────────────────
    migrate_data_from_wincard();
    vault::migrate_keyring_from_wincard();

    // ── CLI flags ──────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    #[allow(unused_variables)]
    let headless = args.iter().any(|a| a == "--headless");

    // --install / --uninstall manage the systemd user service (Linux only).
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--install") {
        return install_systemd_service();
    }
    #[cfg(target_os = "linux")]
    if args.iter().any(|a| a == "--uninstall") {
        return uninstall_systemd_service();
    }

    // --show-carddav-password prints the CardDAV Basic Auth password and exits.
    if args.iter().any(|a| a == "--show-carddav-password") {
        let pw = vault.get_or_init_carddav_password()?;
        eprintln!("CardDAV Basic Auth credentials:");
        eprintln!("  Username: setu  (any username works)");
        eprintln!("  Password: {pw}");
        return Ok(());
    }

    // --settings opens the GUI and exits (requires "gui" feature).
    if args.iter().any(|a| a == "--settings") {
        #[cfg(feature = "gui")]
        {
            tracing::info!("launching settings GUI");
            // DB key needed for settings to perform login.
            let db_key = vault.get_or_init_db_key()?;
            return settings::show(vault, db_key);
        }
        #[cfg(not(feature = "gui"))]
        {
            anyhow::bail!("settings GUI requires the \"gui\" feature (rebuild with --features gui)");
        }
    }

    // ── Single-instance guard ─────────────────────────────────────
    // Prevents multiple copies of the main tray/server from running.
    // (--settings and --show-carddav-password already exited above.)
    // When restarting, wait briefly for the old process to release the mutex.
    if args.iter().any(|a| a == "--restart") {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _instance_guard = ensure_single_instance();

    // ── Load configuration (migrates client_secret to keyring) ───
    #[allow(unused_mut)]
    let mut cfg = config::Config::load_and_migrate(&vault)?;
    tracing::info!(
        credentials = cfg.has_credentials(&vault),
        port = cfg.server_port,
        interval = cfg.sync_interval_secs,
        "configuration loaded"
    );

    // ── DB encryption key + one-time migration ───────────────────
    let db_key = vault.get_or_init_db_key()?;
    db::migrate_to_encrypted(&db_key)?;

    // ── CardDAV Basic Auth password (ensure one exists) ────────
    let _carddav_password = vault.get_or_init_carddav_password()?;

    // ── First-run: open settings if setup is incomplete ──────────
    let is_setup_complete = cfg.has_credentials(&vault) && auth::ensure_authenticated(&vault);
    if !is_setup_complete {
        #[cfg(feature = "gui")]
        if !headless {
            tracing::info!("setup incomplete — opening settings for first-run setup");
            settings::show(vault, db_key.clone())?;
            cfg = config::Config::load_and_migrate(&vault)?;
            if !cfg.has_credentials(&vault) || !auth::ensure_authenticated(&vault) {
                tracing::warn!("setup still incomplete after settings — exiting");
                return Ok(());
            }
            tracing::info!("setup complete — continuing startup");

            // On Linux, try to install a systemd user service. If systemd
            // isn't available (Docker, containers), continue in-process.
            #[cfg(target_os = "linux")]
            {
                match install_systemd_service() {
                    Ok(()) => {
                        eprintln!("Setu is now running as a background service.");
                        eprintln!("  Status:  systemctl --user status setu");
                        eprintln!("  Logs:    journalctl --user -u setu -f");
                        eprintln!("  Stop:    systemctl --user stop setu");
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::info!("no systemd — launching daemon: {e:#}");
                        // Spawn a detached headless process so the settings
                        // window can close without killing the server.
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(&exe)
                                .arg("--headless")
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn();
                            eprintln!("Setu is running in the background (headless).");
                            return Ok(());
                        }
                        // If spawn failed, fall through to in-process headless.
                    }
                }
            }
        }

        #[cfg(not(feature = "gui"))]
        if !cfg.has_credentials(&vault) {
            anyhow::bail!(
                "setup incomplete — edit config at {:?} or rebuild with --features gui",
                config::Config::path()?
            );
        }
    }

    // ── Ensure database is ready ─────────────────────────────────
    let _conn = db::open(Some(&db_key))?;
    tracing::info!("database initialised at {:?}", db::db_path()?);

    // ── Tokio runtime (background thread) ────────────────────────
    let rt = tokio::runtime::Runtime::new()?;

    // Channel: tray "Sync Now" → sync engine.
    #[allow(unused_variables)]
    let (sync_tx, sync_rx) = tokio::sync::mpsc::channel::<()>(4);

    // ── Build shared GoogleApi (if credentials are configured) ───
    let google_api: Option<google_api::GoogleApi> = if cfg.has_credentials(&vault) {
        let client_secret = vault
            .get_google_client_secret()?
            .unwrap_or_default();
        match rt.block_on(google_api::GoogleApi::build(&cfg, &client_secret)) {
            Ok(api) => {
                tracing::info!("Google API client initialised");
                Some(api)
            }
            Err(e) => {
                tracing::error!("failed to build Google API client: {e:#}");
                None
            }
        }
    } else {
        tracing::warn!("Google credentials not configured — sync & on-demand search disabled");
        None
    };

    // Fire the search warmup in the background.
    if let Some(ref api) = google_api {
        let warmup_api = api.clone();
        rt.spawn(async move {
            if let Err(e) = warmup_api.warmup_search().await {
                tracing::warn!("search warmup failed (non-fatal): {e:#}");
            }
        });
    }

    // Load TLS config if HTTPS is enabled.
    let tls_config = if cfg.use_tls {
        match setu_lib::tls::load_server_tls_config() {
            Ok(tls) => {
                tracing::info!("TLS enabled — CardDAV server will use HTTPS");
                Some(tls)
            }
            Err(e) => {
                tracing::error!("failed to load TLS config, falling back to HTTP: {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Spawn the CardDAV server (with optional GoogleApi for on-demand search).
    let server_port = cfg.server_port;
    let server_api = google_api.clone();
    let server_db_key = db_key.clone();
    rt.spawn(async move {
        if let Err(e) = server::start_carddav_server(
            server_port,
            server_api,
            server_db_key,
            vault,
            tls_config,
        )
        .await
        {
            tracing::error!("CardDAV server error: {e:#}");
        }
    });

    // Spawn the sync loop (only if we have a GoogleApi).
    if let Some(api) = google_api.clone() {
        let interval = cfg.sync_interval_secs;
        let sync_db_key = db_key.clone();
        rt.spawn(async move {
            if let Err(e) =
                sync::run_sync_loop(api, interval, sync_rx, vault, sync_db_key).await
            {
                tracing::error!("sync loop error: {e:#}");
            }
        });
    }

    // ── Run mode: tray (GUI) or headless ────────────────────────
    #[cfg(feature = "gui")]
    if !headless {
        // Handle tray actions on a background thread.
        let (action_tx, action_rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            for action in action_rx {
                match action {
                    tray::TrayAction::OpenSettings => {
                        tracing::info!("user requested settings");
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(exe).arg("--settings").spawn();
                        }
                    }
                    tray::TrayAction::SyncNow => {
                        tracing::info!("user requested immediate sync");
                        let _ = sync_tx.blocking_send(());
                    }
                    tray::TrayAction::Restart => {
                        tracing::info!("user requested restart");
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(exe)
                                .arg("--restart")
                                .spawn();
                        }
                        std::process::exit(0);
                    }
                    tray::TrayAction::Quit => {
                        tracing::info!("user requested quit — shutting down");
                        std::process::exit(0);
                    }
                }
            }
        });

        // Tray icon on the main thread (blocking).
        tracing::info!("starting system tray");
        match tray::run_tray(action_tx) {
            Ok(()) => return Ok(()), // event loop ran until user quit
            Err(e) => {
                tracing::warn!("tray failed to start: {e:#} — falling back to headless mode");
            }
        }
    }

    // Headless mode: block on Ctrl+C.
    tracing::info!("running headless (Ctrl+C to stop)");
    eprintln!(
        "Setu v{} (build {}) — CardDAV server on port {}",
        env!("CARGO_PKG_VERSION"),
        env!("SETU_BUILD_ID"),
        cfg.server_port,
    );
    eprintln!("Press Ctrl+C to stop.");

    rt.block_on(async {
        tokio::signal::ctrl_c().await.ok();
    });

    tracing::info!("received Ctrl+C — shutting down");
    Ok(())
}

// ── One-time data directory migration from "wincard" → "setu" ───────

/// Copy data files from the old `wincard` directory to `setu`.
///
/// Files: `config.json`, `wincard.db` → `setu.db`, `oauth_token.json`.
/// The old directory is left in place (user can delete it manually).
/// Idempotent: skips files that already exist in the new location.
fn migrate_data_from_wincard() {
    let Some(base) = dirs::data_dir() else {
        return;
    };
    let old_dir = base.join("wincard");
    let new_dir = base.join("setu");

    if !old_dir.exists() {
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&new_dir) {
        tracing::warn!("cannot create setu data dir: {e}");
        return;
    }

    // (old_name, new_name) pairs.
    let files: &[(&str, &str)] = &[
        ("config.json", "config.json"),
        ("wincard.db", "setu.db"),
        ("oauth_token.json", "oauth_token.json"),
    ];

    for &(old_name, new_name) in files {
        let old_path = old_dir.join(old_name);
        let new_path = new_dir.join(new_name);

        if old_path.exists() && !new_path.exists() {
            match std::fs::copy(&old_path, &new_path) {
                Ok(_) => tracing::info!(
                    old = %old_path.display(),
                    new = %new_path.display(),
                    "migrated data file from wincard → setu"
                ),
                Err(e) => tracing::warn!(
                    old = %old_path.display(),
                    "failed to migrate data file: {e}"
                ),
            }
        }
    }
}

// ── Single-instance enforcement ─────────────────────────────────────

/// On Windows, creates a named mutex. If another instance already holds it,
/// the process exits immediately. The returned guard keeps the mutex alive
/// for the lifetime of the process.
///
/// On non-Windows platforms this is a no-op.
#[cfg(target_os = "windows")]
fn ensure_single_instance() -> Option<()> {
    use std::ffi::c_void;

    extern "system" {
        fn CreateMutexW(
            lp_mutex_attributes: *const c_void,
            b_initial_owner: i32,
            lp_name: *const u16,
        ) -> *mut c_void;
        fn GetLastError() -> u32;
    }

    const ERROR_ALREADY_EXISTS: u32 = 183;

    let name: Vec<u16> = "Global\\SetuSingleInstance\0"
        .encode_utf16()
        .collect();

    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            tracing::warn!("another instance of Setu is already running — exiting");
            std::process::exit(0);
        }
        // Intentionally leak the handle — it must live for the entire process.
        // Windows releases it automatically when the process exits.
    }

    Some(())
}

#[cfg(not(target_os = "windows"))]
fn ensure_single_instance() -> Option<()> {
    None
}

// ── Systemd user service management (Linux only) ─────────────────────

/// Check whether a systemd user session is available.
/// Returns false inside Docker / containers where systemd isn't PID 1.
#[cfg(target_os = "linux")]
fn has_systemd_user() -> bool {
    // Standard check: /run/systemd/system exists only when systemd is the init.
    if !std::path::Path::new("/run/systemd/system").exists() {
        return false;
    }
    // Verify the user instance responds.
    std::process::Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn systemd_service_path() -> anyhow::Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("cannot resolve home directory")?;
    let dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("setu.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd_service() -> anyhow::Result<()> {
    use anyhow::Context;

    if !has_systemd_user() {
        anyhow::bail!("systemd user session not available (container or non-systemd init)");
    }

    let exe = std::env::current_exe().context("cannot resolve current executable")?;
    let exe_path = exe.display();

    let unit = format!(
        "[Unit]\n\
         Description=Setu — CardDAV bridge for Google Contacts\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe_path} --headless\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    );

    let service_path = systemd_service_path()?;
    std::fs::write(&service_path, &unit)
        .with_context(|| format!("writing {}", service_path.display()))?;

    tracing::info!("installed systemd service at {}", service_path.display());

    // Reload systemd, enable, and start the service.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "setu"])
        .status()
        .context("failed to enable setu service")?;

    if status.success() {
        tracing::info!("setu service enabled and started");
        Ok(())
    } else {
        anyhow::bail!("systemctl enable --now failed (exit {})", status);
    }
}

#[cfg(target_os = "linux")]
fn uninstall_systemd_service() -> anyhow::Result<()> {
    use anyhow::Context;

    // Stop and disable.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "setu"])
        .status();

    let service_path = systemd_service_path()?;
    if service_path.exists() {
        std::fs::remove_file(&service_path)
            .with_context(|| format!("removing {}", service_path.display()))?;
        tracing::info!("removed {}", service_path.display());
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    eprintln!("Setu service uninstalled.");
    Ok(())
}
