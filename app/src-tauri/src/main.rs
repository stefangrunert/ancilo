//! Ancilo desktop app: a menu bar icon and a small window. The UI is served
//! by the daemon (`/app/`); the app finds or starts the daemon and hands the
//! window its access token (decision `2026-09-30-m7-umsetzung`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod links;
mod monitor;
mod update;
mod variant;

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const WINDOW: &str = "main";

fn show_window(app: &AppHandle, d: &daemon::Daemon) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(WINDOW) {
        w.show()?;
        w.set_focus()?;
        return Ok(());
    }
    let url = format!("{}/app/", d.url)
        .parse()
        .map_err(|e| tauri::Error::Io(anyhow_like(format!("{e}"))))?;
    let token = serde_json::to_string(&d.token).unwrap_or_default();
    let origin = d.url.clone();
    let origin2 = d.url.clone();
    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(url))
        // Links to the web (sources, serper.dev) open in the user's browser;
        // the window itself never leaves Ancilo.
        .on_new_window(move |url, _| {
            if links::outside(&url, &origin) {
                links::open(&url);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_navigation(move |url| {
            if links::outside(url, &origin2) {
                links::open(url);
                return false;
            }
            true
        })
        .title(variant::NAME)
        // Room for the sidebar, a chat column and the changes panel.
        .inner_size(1280.0, 840.0)
        .min_inner_size(720.0, 520.0)
        .initialization_script(format!(
            "window.__ANCILO__ = {{ token: {token}, app: true }};"
        ))
        .build()?;
    Ok(())
}

/// Looks for an update; a found one is offered in the menu ("Install … and
/// restart") – installing waits for the user's click.
async fn offer(
    app: &AppHandle,
    pending: &Mutex<Option<tauri_plugin_updater::Update>>,
    item: &MenuItem<tauri::Wry>,
    asked: bool,
) {
    match update::check(app).await {
        Ok(Some(u)) => {
            let _ = item.set_text(format!("Install Ancilo {} and Restart", u.version));
            *pending.lock().unwrap() = Some(u);
        }
        Ok(None) if asked => {
            let _ = item.set_text("Ancilo is up to date");
        }
        Err(e) if asked => {
            let _ = item.set_text(format!("Update check failed: {e}"));
        }
        _ => {}
    }
}

async fn tokio_sleep(d: std::time::Duration) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    let _ = tauri::async_runtime::spawn_blocking(move || rx.recv()).await;
}

fn anyhow_like(msg: String) -> std::io::Error {
    std::io::Error::other(msg)
}

fn main() {
    let paths = variant::paths();
    let smoke = std::env::var_os("ANCILO_SMOKE").is_some();
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            let d = daemon::ensure(&paths)
                .map_err(|e| Box::new(anyhow_like(e)) as Box<dyn std::error::Error>)?;
            #[cfg(target_os = "macos")]
            if std::env::var_os("ANCILO_NO_LAUNCH_AGENT").is_none()
                && !smoke
                && let Err(e) = daemon::install_launch_agent(&paths)
            {
                eprintln!("LaunchAgent not installed: {e}");
            }
            let open = MenuItem::with_id(
                app,
                "open",
                format!("Open {}", variant::NAME),
                true,
                None::<&str>,
            )?;
            let updates = MenuItem::with_id(
                app,
                "update",
                "Check for Updates…",
                update::configured(app.handle()),
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &updates, &quit])?;
            let d2 = d.clone();
            let pending: Arc<Mutex<Option<tauri_plugin_updater::Update>>> = Arc::default();
            let (p2, u2) = (pending.clone(), updates.clone());
            TrayIconBuilder::with_id("ancilo")
                .icon(app.default_window_icon().cloned().expect("icon"))
                .tooltip(variant::NAME)
                .menu(&menu)
                .on_menu_event(move |app, e| match e.id().as_ref() {
                    "open" => {
                        let _ = show_window(app, &d2);
                    }
                    // Checking and installing only on the user's click.
                    "update" => {
                        let (app, pending, item) = (app.clone(), p2.clone(), u2.clone());
                        tauri::async_runtime::spawn(async move {
                            let ready = pending.lock().unwrap().take();
                            match ready {
                                Some(u) => {
                                    let _ = item.set_text("Installing update…");
                                    match update::install(u).await {
                                        Ok(()) => app.restart(),
                                        Err(e) => {
                                            let _ = item.set_text(format!("Update failed: {e}"));
                                        }
                                    }
                                }
                                None => {
                                    let _ = item.set_text("Checking for updates…");
                                    offer(&app, &pending, &item, true).await;
                                }
                            }
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            // Automatically only if the user allowed it (Settings in the app).
            if update::configured(app.handle()) && !smoke && daemon::auto_update_checks(&d) {
                let (app, pending, item) = (app.handle().clone(), pending.clone(), updates.clone());
                tauri::async_runtime::spawn(async move {
                    loop {
                        offer(&app, &pending, &item, false).await;
                        tokio_sleep(std::time::Duration::from_secs(24 * 3600)).await;
                    }
                });
            }
            show_window(app.handle(), &d)?;
            // The system monitor reaches the user also when the window is away.
            if !smoke {
                monitor::spawn(app.handle().clone(), d.clone(), WINDOW);
            }
            if smoke {
                // Smoke test (M7-AC-08): report what started, then exercise the
                // menu's "open" path once more and quit.
                let handle = app.handle().clone();
                let url = d.url.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let reopened = show_window(
                        &handle,
                        &daemon::Daemon {
                            url: url.clone(),
                            token: String::new(),
                        },
                    )
                    .is_ok();
                    let visible = handle
                        .get_webview_window(WINDOW)
                        .and_then(|w| w.is_visible().ok())
                        .unwrap_or(false);
                    println!(
                        "{}",
                        serde_json::json!({"daemon": url, "window": visible, "menu_open": reopened})
                    );
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Ancilo app");
}
