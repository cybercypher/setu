//! Windows system tray icon and menu.

use anyhow::Result;
use tao::event_loop::EventLoopBuilder;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIconBuilder,
};

/// Pre-rendered 32x32 RGBA icon data compiled into the binary.
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon_32x32.rgba");
const ICON_SIZE: u32 = 32;

/// Tray menu action returned to the main loop.
pub enum TrayAction {
    OpenSettings,
    SyncNow,
    Restart,
    Quit,
}

/// Build and run the Windows system tray icon.
/// This blocks the calling thread (owns the Win32 message pump).
pub fn run_tray(action_tx: std::sync::mpsc::Sender<TrayAction>) -> Result<()> {
    let event_loop = EventLoopBuilder::new().build();

    // ── Menu ──────────────────────────────────────────────────
    let menu = Menu::new();
    let item_settings = MenuItem::new("Settings...", true, None);
    let item_sync = MenuItem::new("Sync Now", true, None);
    let item_restart = MenuItem::new("Restart Setu", true, None);
    let item_quit = MenuItem::new("Quit Setu", true, None);
    menu.append(&item_settings)?;
    menu.append(&item_sync)?;
    menu.append(&item_restart)?;
    menu.append(&item_quit)?;

    // ── Icon (pre-rendered 32x32 RGBA) ────────────────────────
    let icon = tray_icon::Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE)
        .expect("valid icon RGBA data");

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!(
            "Setu v{} (build {})",
            env!("CARGO_PKG_VERSION"),
            env!("SETU_BUILD_ID"),
        ))
        .with_icon(icon)
        .build()?;

    // ── Event loop ────────────────────────────────────────────
    let settings_id = item_settings.id().clone();
    let sync_id = item_sync.id().clone();
    let restart_id = item_restart.id().clone();
    let quit_id = item_quit.id().clone();

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = tao::event_loop::ControlFlow::Wait;

        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id() == &settings_id {
                let _ = action_tx.send(TrayAction::OpenSettings);
            } else if event.id() == &sync_id {
                let _ = action_tx.send(TrayAction::SyncNow);
            } else if event.id() == &restart_id {
                // Don't exit the event loop here — let the action handler
                // spawn the new process and call std::process::exit().
                let _ = action_tx.send(TrayAction::Restart);
            } else if event.id() == &quit_id {
                let _ = action_tx.send(TrayAction::Quit);
                *control_flow = tao::event_loop::ControlFlow::Exit;
            }
        }
    });
}

