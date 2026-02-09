//! Settings GUI — eframe/egui window for configuring Setu.
//!
//! On first run this acts as a guided setup wizard, walking the user through
//! creating a Google Cloud project, enabling the People API, creating OAuth
//! credentials, and signing in.

use setu_lib::config::Config;
use setu_lib::vault::SecureVault;
use eframe::egui;

// ── Theme colours ───────────────────────────────────────────────────
const BLUE_PRIMARY: egui::Color32 = egui::Color32::from_rgb(30, 100, 220);
const BLUE_LIGHT: egui::Color32 = egui::Color32::from_rgb(60, 140, 255);
const BLUE_HOVER: egui::Color32 = egui::Color32::from_rgb(45, 120, 240);
const BLUE_BG: egui::Color32 = egui::Color32::from_rgb(235, 242, 255);
const BLUE_CARD: egui::Color32 = egui::Color32::from_rgb(245, 248, 255);
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(30, 30, 45);
const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(100, 110, 130);
const GREEN_SUCCESS: egui::Color32 = egui::Color32::from_rgb(40, 167, 69);
const RED_ERROR: egui::Color32 = egui::Color32::from_rgb(220, 53, 69);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(250, 251, 253);

/// Launch the settings window (blocking — returns when the window is closed).
pub fn show(vault: SecureVault, db_key: String) -> anyhow::Result<()> {
    let config = Config::load()?;

    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon_32x32.rgba").to_vec(),
        width: 32,
        height: 32,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 660.0])
            .with_min_inner_size([480.0, 500.0])
            .with_title("Setu")
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "Setu",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(config, vault, db_key)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    Ok(())
}

/// Apply the Setu blue theme to egui.
fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    // Rounded corners everywhere
    style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
    style.visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
    style.visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
    style.visuals.widgets.active.rounding = egui::Rounding::same(6.0);

    // Light background
    style.visuals.window_fill = SURFACE;
    style.visuals.panel_fill = SURFACE;

    // Softer widget backgrounds
    style.visuals.widgets.inactive.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 218, 230));
    style.visuals.widgets.hovered.bg_fill = BLUE_BG;
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, BLUE_LIGHT);

    // Text colours
    style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);

    // Nicer selection colour
    style.visuals.selection.bg_fill = BLUE_PRIMARY.linear_multiply(0.25);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, BLUE_PRIMARY);

    // Spacing
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(14.0, 6.0);

    // Separator
    style.visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 230, 240));

    ctx.set_style(style);
}

// ── Login state ──────────────────────────────────────────────────────

enum LoginState {
    NotLoggedIn,
    InProgress,
    LoggedIn(String),
    Error(String),
}

// ── App state ────────────────────────────────────────────────────────

struct SettingsApp {
    client_id: String,
    client_secret: String,
    sync_interval: String,
    server_port: String,
    carddav_password: String,
    use_tls: bool,
    status_msg: String,
    status_is_error: bool,
    login_state: LoginState,
    /// Receives the result of the background OAuth flow.
    login_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    vault: SecureVault,
    db_key: String,
    show_client_secret: bool,
    show_carddav_password: bool,
}

impl SettingsApp {
    fn new(config: Config, vault: SecureVault, db_key: String) -> Self {
        let login_state = match setu_lib::auth::get_logged_in_email(&db_key) {
            Ok(Some(email)) => LoginState::LoggedIn(email),
            _ if setu_lib::auth::ensure_authenticated(&vault) => {
                LoginState::LoggedIn("Authenticated".into())
            }
            _ => LoginState::NotLoggedIn,
        };

        let client_secret = vault
            .get_google_client_secret()
            .ok()
            .flatten()
            .unwrap_or_default();

        let carddav_password = vault
            .get_or_init_carddav_password()
            .unwrap_or_default();

        Self {
            client_id: config.google_client_id,
            client_secret,
            sync_interval: config.sync_interval_secs.to_string(),
            server_port: config.server_port.to_string(),
            carddav_password,
            use_tls: config.use_tls,
            status_msg: String::new(),
            status_is_error: false,
            login_state,
            login_rx: None,
            vault,
            db_key,
            show_client_secret: false,
            show_carddav_password: false,
        }
    }

    fn save(&mut self) -> bool {
        let interval: u64 = match self.sync_interval.parse() {
            Ok(v) if v >= 10 => v,
            _ => {
                self.status_msg = "Sync interval must be >= 10 seconds".into();
                self.status_is_error = true;
                return false;
            }
        };
        let port: u16 = match self.server_port.parse() {
            Ok(v) if v >= 1024 => v,
            _ => {
                self.status_msg = "Port must be >= 1024".into();
                self.status_is_error = true;
                return false;
            }
        };

        let secret = self.client_secret.trim().to_string();
        if !secret.is_empty() {
            if let Err(e) = self.vault.store_google_client_secret(&secret) {
                self.status_msg = format!("Error storing secret: {e}");
                self.status_is_error = true;
                return false;
            }
        }

        let pw = self.carddav_password.trim().to_string();
        if !pw.is_empty() {
            if let Err(e) = self.vault.store_carddav_password(&pw) {
                self.status_msg = format!("Error storing CardDAV password: {e}");
                self.status_is_error = true;
                return false;
            }
        }

        // If TLS was just enabled, generate certs and install the CA.
        if self.use_tls {
            if let Err(e) = setu_lib::tls::ensure_certs() {
                self.status_msg = format!("Error generating TLS certs: {e}");
                self.status_is_error = true;
                return false;
            }
            if let Err(e) = setu_lib::tls::install_ca_to_trust_store() {
                self.status_msg = format!("Error installing CA: {e}");
                self.status_is_error = true;
                return false;
            }
        }

        let config = Config {
            google_client_id: self.client_id.trim().to_string(),
            google_client_secret: String::new(),
            sync_interval_secs: interval,
            server_port: port,
            use_tls: self.use_tls,
        };

        match config.save() {
            Ok(()) => {
                let scheme = if self.use_tls { "https" } else { "http" };
                self.status_msg = format!(
                    "Settings saved. Server URL: {scheme}://localhost:{port}"
                );
                if self.use_tls {
                    self.status_msg.push_str(" — restart Setu to apply TLS.");
                }
                self.status_is_error = false;
                true
            }
            Err(e) => {
                self.status_msg = format!("Error saving: {e}");
                self.status_is_error = true;
                false
            }
        }
    }

    fn start_login(&mut self, ctx: &egui::Context) {
        if !self.save() {
            return;
        }

        self.login_state = LoginState::InProgress;
        self.status_msg.clear();

        let client_id = self.client_id.trim().to_string();
        let client_secret = self.client_secret.trim().to_string();
        let vault = self.vault;
        let db_key = self.db_key.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.login_rx = Some(rx);
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result =
                rt.block_on(setu_lib::auth::login(&client_id, &client_secret, &vault, &db_key));
            let _ = match result {
                Ok(r) => tx.send(Ok(r.email)),
                Err(e) => tx.send(Err(format!("{e:#}"))),
            };
            ctx.request_repaint();
        });
    }

    fn has_credentials(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

// ── UI helpers ───────────────────────────────────────────────────────

/// Draw a card-like frame with rounded corners and subtle border.
fn card_frame(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(BLUE_CARD)
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(215, 225, 240)))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

/// Blue filled button (primary action).
fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let btn = egui::Button::new(
        egui::RichText::new(text).color(egui::Color32::WHITE).strong(),
    )
    .fill(if enabled { BLUE_PRIMARY } else { egui::Color32::from_rgb(160, 175, 200) })
    .stroke(egui::Stroke::NONE)
    .rounding(egui::Rounding::same(8.0))
    .min_size(egui::vec2(0.0, 34.0));
    let resp = ui.add_enabled(enabled, btn);
    // Hover tint
    if resp.hovered() && enabled {
        ui.painter().rect_filled(
            resp.rect,
            egui::Rounding::same(8.0),
            BLUE_HOVER,
        );
    }
    resp
}

/// Section heading with blue accent.
fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        // Blue accent bar
        let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, egui::Rounding::same(1.5), BLUE_PRIMARY);
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).strong().size(15.0).color(TEXT_PRIMARY));
    });
}

/// Labelled input field with consistent layout.
fn labelled_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(egui::RichText::new(label).size(12.0).color(TEXT_SECONDARY));
    ui.add_space(2.0);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(ui.available_width())
            .margin(egui::Margin::symmetric(8.0, 6.0)),
    );
}

/// Labelled password field with a show/hide toggle button.
fn password_field(ui: &mut egui::Ui, label: &str, value: &mut String, visible: &mut bool) {
    ui.label(egui::RichText::new(label).size(12.0).color(TEXT_SECONDARY));
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let te = egui::TextEdit::singleline(value)
            .desired_width(ui.available_width() - 52.0)
            .margin(egui::Margin::symmetric(8.0, 6.0))
            .password(!*visible);
        ui.add(te);
        let toggle_text = if *visible { "Hide" } else { "Show" };
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(toggle_text).size(12.0).color(BLUE_PRIMARY),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(210, 218, 230)))
                .rounding(egui::Rounding::same(6.0))
                .min_size(egui::vec2(44.0, 28.0)),
            )
            .clicked()
        {
            *visible = !*visible;
        }
    });
}

/// Small labelled input for numbers (fixed width).
fn small_field(ui: &mut egui::Ui, label: &str, value: &mut String, width: f32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(TEXT_SECONDARY));
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(width)
                .margin(egui::Margin::symmetric(8.0, 6.0)),
        );
    });
}

// ── Main UI ─────────────────────────────────────────────────────────

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for login result.
        if let Some(ref rx) = self.login_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(email) => {
                        self.login_state = LoginState::LoggedIn(email);
                        self.status_msg =
                            "Login successful! Close this window to start Setu.".into();
                        self.status_is_error = false;
                    }
                    Err(msg) => {
                        self.login_state = LoginState::Error(msg.clone());
                        self.status_msg = format!("Login failed: {msg}");
                        self.status_is_error = true;
                    }
                }
                self.login_rx = None;
            }
        }

        let is_logged_in = matches!(self.login_state, LoginState::LoggedIn(_));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(SURFACE).inner_margin(egui::Margin::same(24.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());

                    // ── Header ──────────────────────────────────────
                    ui.horizontal(|ui| {
                        ui.heading(
                            egui::RichText::new("Setu")
                                .size(24.0)
                                .strong()
                                .color(BLUE_PRIMARY),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "v{} ({})",
                                    env!("CARGO_PKG_VERSION"),
                                    env!("SETU_BUILD_ID"),
                                ))
                                .size(11.0)
                                .color(TEXT_SECONDARY),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("CardDAV bridge for Google Contacts")
                            .size(13.0)
                            .color(TEXT_SECONDARY),
                    );
                    ui.add_space(16.0);

                    // ── Google Cloud Setup ───────────────────────────
                    let setup_header = if is_logged_in {
                        "Google Cloud Setup"
                    } else {
                        "Step 1 — Google Cloud Setup"
                    };
                    section_heading(ui, setup_header);
                    ui.add_space(6.0);

                    card_frame(ui, |ui| {
                        if is_logged_in {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Setup complete.")
                                        .color(GREEN_SUCCESS),
                                );
                                ui.label(
                                    egui::RichText::new("Expand for reference.")
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                            });
                            ui.add_space(4.0);
                        }

                        let id = ui.make_persistent_id("setup_steps");
                        egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, !is_logged_in)
                            .show_header(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(if is_logged_in { "Setup steps" } else { "Follow these one-time steps:" })
                                        .size(12.5)
                                        .color(TEXT_SECONDARY),
                                );
                            })
                            .body(|ui| {
                                ui.add_space(4.0);
                                setup_step(ui, "1", "Create a Google Cloud project",
                                    "Open Google Cloud Console",
                                    "https://console.cloud.google.com/projectcreate");
                                setup_step(ui, "2", "Enable the People API",
                                    "Enable People API",
                                    "https://console.cloud.google.com/apis/library/people.googleapis.com");
                                setup_step(ui, "3", "Configure the OAuth consent screen",
                                    "Configure Consent Screen",
                                    "https://console.cloud.google.com/apis/credentials/consent");
                                ui.indent("s3note", |ui| {
                                    ui.label(
                                        egui::RichText::new("Choose \"External\" and add your email as a test user.")
                                            .size(12.0).color(TEXT_SECONDARY).italics(),
                                    );
                                });
                                setup_step(ui, "4", "Create a Desktop OAuth credential",
                                    "Create Credentials",
                                    "https://console.cloud.google.com/apis/credentials");
                                ui.indent("s4note", |ui| {
                                    ui.label(
                                        egui::RichText::new("Copy the Client ID and Secret below.")
                                            .size(12.0).color(TEXT_SECONDARY).italics(),
                                    );
                                });
                            });
                    });

                    ui.add_space(16.0);

                    // ── Credentials & Login ─────────────────────────
                    let cred_header = if is_logged_in {
                        "Google Account"
                    } else {
                        "Step 2 — Credentials & Sign In"
                    };
                    section_heading(ui, cred_header);
                    ui.add_space(6.0);

                    card_frame(ui, |ui| {
                        labelled_field(ui, "Client ID", &mut self.client_id);
                        ui.add_space(6.0);
                        password_field(ui, "Client Secret", &mut self.client_secret, &mut self.show_client_secret);
                        ui.add_space(12.0);

                        ui.horizontal(|ui| {
                            let can_login = self.has_credentials()
                                && !matches!(self.login_state, LoginState::InProgress);
                            if primary_button(ui, "Login with Google", can_login).clicked() && can_login {
                                self.start_login(ctx);
                            }

                            ui.add_space(8.0);

                            match &self.login_state {
                                LoginState::NotLoggedIn => {
                                    ui.label(
                                        egui::RichText::new("Not signed in")
                                            .color(TEXT_SECONDARY).size(13.0),
                                    );
                                }
                                LoginState::InProgress => {
                                    ui.spinner();
                                    ui.label(
                                        egui::RichText::new("Complete sign-in in your browser...")
                                            .color(TEXT_SECONDARY).size(13.0),
                                    );
                                }
                                LoginState::LoggedIn(email) => {
                                    ui.label(
                                        egui::RichText::new(format!("Signed in as {email}"))
                                            .color(GREEN_SUCCESS).size(13.0),
                                    );
                                }
                                LoginState::Error(msg) => {
                                    ui.label(
                                        egui::RichText::new(format!("Error: {msg}"))
                                            .color(RED_ERROR).size(13.0),
                                    );
                                }
                            }
                        });
                    });

                    ui.add_space(16.0);

                    // ── Server Settings ─────────────────────────────
                    section_heading(ui, "Server Settings");
                    ui.add_space(6.0);

                    card_frame(ui, |ui| {
                        small_field(ui, "Sync interval (seconds)", &mut self.sync_interval, 80.0);
                        ui.add_space(4.0);
                        small_field(ui, "CardDAV server port", &mut self.server_port, 80.0);
                        ui.add_space(8.0);
                        password_field(ui, "CardDAV password", &mut self.carddav_password, &mut self.show_carddav_password);
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("Use any username with this password to connect your CardDAV client.")
                                .size(12.0)
                                .color(TEXT_SECONDARY)
                                .italics(),
                        );
                        ui.add_space(8.0);
                        ui.checkbox(&mut self.use_tls, "Enable HTTPS (encrypt localhost traffic)");
                        if self.use_tls {
                            let tls_hint = if cfg!(target_os = "windows") {
                                "A local CA will be generated and added to the Windows trust store on save."
                            } else {
                                "A local CA will be generated and added to the system trust store on save (requires password)."
                            };
                            ui.label(
                                egui::RichText::new(tls_hint)
                                .size(12.0)
                                .color(TEXT_SECONDARY)
                                .italics(),
                            );
                        }
                        ui.add_space(4.0);
                        let scheme = if self.use_tls { "https" } else { "http" };
                        let port_str = self.server_port.trim();
                        let url = format!("{scheme}://localhost:{port_str}");
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Server URL:")
                                    .size(12.0)
                                    .color(TEXT_SECONDARY),
                            );
                            ui.label(
                                egui::RichText::new(&url)
                                    .size(12.0)
                                    .color(BLUE_PRIMARY)
                                    .strong(),
                            );
                        });
                    });

                    ui.add_space(16.0);

                    // ── Save button + status ────────────────────────
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Save Settings", true).clicked() {
                            self.save();
                        }

                        if !self.status_msg.is_empty() {
                            ui.add_space(12.0);
                            let color = if self.status_is_error { RED_ERROR } else { GREEN_SUCCESS };
                            ui.label(
                                egui::RichText::new(&self.status_msg)
                                    .color(color)
                                    .size(13.0),
                            );
                        }
                    });

                    ui.add_space(16.0);
                });
            });
    }
}

/// Render a single setup step (number badge + description + link).
fn setup_step(ui: &mut egui::Ui, num: &str, description: &str, link_text: &str, url: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        // Number badge
        let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().rect_filled(badge_rect, egui::Rounding::same(11.0), BLUE_PRIMARY);
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            num,
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(description).size(13.0));
            ui.hyperlink_to(
                egui::RichText::new(link_text).size(12.0).color(BLUE_LIGHT),
                url,
            );
        });
    });
}
