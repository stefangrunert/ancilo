//! Ancilo desktop app: a menu bar icon and a small window. The UI is served
//! by the daemon (`/app/`); the app finds or starts the daemon and hands the
//! window its access token (decision `2026-09-30-m7-umsetzung`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod daemon;
mod links;
mod monitor;
mod remove;
mod update;
mod variant;

use std::sync::{Arc, Mutex};

use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const WINDOW: &str = "main";

fn show_window(app: &AppHandle, d: &daemon::Daemon) -> tauri::Result<()> {
    show_window_at(app, d, None)
}

/// Shows the window – on `route` (the app's hash route, e.g. `#/system`)
/// when given.
fn show_window_at(app: &AppHandle, d: &daemon::Daemon, route: Option<&str>) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(WINDOW) {
        if let Some(route) = route {
            w.eval(format!(
                "window.location.hash = {}",
                serde_json::to_string(route).unwrap_or_default()
            ))?;
        }
        w.show()?;
        w.set_focus()?;
        return Ok(());
    }
    let url = format!("{}/app/{}", d.url, route.unwrap_or_default())
        .parse()
        .map_err(|e| tauri::Error::Io(anyhow_like(format!("{e}"))))?;
    let token = serde_json::to_string(&d.token).unwrap_or_default();
    let origin = d.url.clone();
    let origin2 = d.url.clone();
    let builder = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(url))
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
        ));
    // The page's header is the title bar: the window buttons sit on its left
    // (centred in its 40 px), setup and system next to them.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(14.0, 20.0));
    builder.build()?;
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

/// One entry of the menu bar (a system item – macOS labels and runs it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    About,
    /// Settings… (⌘,): the window, on System.
    Settings,
    /// The user manual on ancilo.app, in the Mac's language.
    Manual,
    Separator,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Minimize,
    Zoom,
    FullScreen,
    CloseWindow,
}

/// The app's own menu bar – only what Ancilo uses. Tauri's default would
/// add an empty File menu, an empty Help and the system's Services submenu (the
/// services of every other app on the Mac – nothing of Ancilo's). Edit stays:
/// without it, copy and paste (⌘C, ⌘V) do not reach the window. The first
/// menu is the app's own (its title: the app's name).
fn layout() -> [(&'static str, &'static [Entry]); 4] {
    use Entry::*;
    [
        (
            variant::NAME,
            &[
                About, Separator, Settings, Separator, Hide, HideOthers, ShowAll, Separator, Quit,
            ],
        ),
        (
            "Edit",
            &[Undo, Redo, Separator, Cut, Copy, Paste, SelectAll],
        ),
        (
            "Window",
            &[Minimize, Zoom, FullScreen, Separator, CloseWindow],
        ),
        ("Help", &[Manual]),
    ]
}

/// The user manual on ancilo.app – German on a Mac in German, else English.
fn manual_url() -> &'static str {
    let german = alert::german();
    if german {
        "https://ancilo.app/de/handbuch/"
    } else {
        "https://ancilo.app/en/manual/"
    }
}

fn app_menu<R: tauri::Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some(variant::NAME.into()),
        version: Some(app.package_info().version.to_string()),
        website: Some("https://ancilo.app".into()),
        website_label: Some("ancilo.app".into()),
        ..Default::default()
    };
    let menu = Menu::new(app)?;
    for (title, entries) in layout() {
        let sub = Submenu::new(app, title, true)?;
        for e in entries {
            match e {
                Entry::Settings => {
                    sub.append(&MenuItem::with_id(
                        app,
                        "settings",
                        "Settings…",
                        true,
                        Some("CmdOrCtrl+,"),
                    )?)?;
                    continue;
                }
                Entry::Manual => {
                    sub.append(&MenuItem::with_id(
                        app,
                        "manual",
                        format!("{} Manual", variant::NAME),
                        true,
                        None::<&str>,
                    )?)?;
                    continue;
                }
                _ => {}
            }
            let item = match e {
                Entry::About => PredefinedMenuItem::about(app, None, Some(about.clone()))?,
                Entry::Separator => PredefinedMenuItem::separator(app)?,
                Entry::Hide => PredefinedMenuItem::hide(app, None)?,
                Entry::HideOthers => PredefinedMenuItem::hide_others(app, None)?,
                Entry::ShowAll => PredefinedMenuItem::show_all(app, None)?,
                Entry::Quit => PredefinedMenuItem::quit(app, None)?,
                Entry::Undo => PredefinedMenuItem::undo(app, None)?,
                Entry::Redo => PredefinedMenuItem::redo(app, None)?,
                Entry::Cut => PredefinedMenuItem::cut(app, None)?,
                Entry::Copy => PredefinedMenuItem::copy(app, None)?,
                Entry::Paste => PredefinedMenuItem::paste(app, None)?,
                Entry::SelectAll => PredefinedMenuItem::select_all(app, None)?,
                Entry::Minimize => PredefinedMenuItem::minimize(app, None)?,
                Entry::Zoom => PredefinedMenuItem::maximize(app, None)?,
                Entry::FullScreen => PredefinedMenuItem::fullscreen(app, None)?,
                Entry::CloseWindow => PredefinedMenuItem::close_window(app, None)?,
                Entry::Settings | Entry::Manual => unreachable!("menu items of Ancilo's own"),
            };
            sub.append(&item)?;
        }
        menu.append(&sub)?;
    }
    Ok(menu)
}

/// System › Remove Ancilo, after the user confirmed in the page.
#[tauri::command]
async fn remove_ancilo(app: AppHandle, keep_data: bool) -> Result<remove::Outcome, String> {
    let id = app.config().identifier.clone();
    tauri::async_runtime::spawn_blocking(move || remove::remove(&variant::paths(), &id, keep_data))
        .await
        .map_err(|e| e.to_string())?
}

/// Quits the app (after Remove Ancilo: nothing left to run).
#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

fn anyhow_like(msg: String) -> std::io::Error {
    std::io::Error::other(msg)
}

fn main() {
    let paths = variant::paths();
    let smoke = std::env::var_os("ANCILO_SMOKE").is_some();
    tauri::Builder::default()
        .menu(app_menu)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "settings" => {
                if let Some(d) = app.try_state::<daemon::Daemon>() {
                    let _ = show_window_at(app, &d, Some("#/system"));
                }
            }
            "manual" => {
                if let Ok(url) = tauri::Url::parse(manual_url()) {
                    links::open(&url);
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![remove_ancilo, quit])
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            // From the disk image: the login item would point at a place that
            // is gone after ejecting – install first.
            if let Some(bundle) = std::env::current_exe()
                .ok()
                .and_then(|e| remove::bundle_of(&e))
                && !remove::installed(&bundle)
            {
                alert::show(alert::not_in_applications());
                std::process::exit(1);
            }
            // No background service: say why instead of vanishing.
            let d = match daemon::ensure(&paths) {
                Ok(d) => d,
                Err(e) => {
                    alert::show(alert::no_daemon(&e, &paths.logs_dir().join("daemon.log")));
                    std::process::exit(1);
                }
            };
            // For the menu bar (Settings…).
            app.manage(d.clone());
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

#[cfg(test)]
mod tests {
    use super::Entry::*;

    /// The menu bar holds only Ancilo's own menus – no Services, no empty
    /// File or Help – and Edit, so copy and paste reach the window.
    #[test]
    fn the_menu_bar_has_only_what_ancilo_uses() {
        let layout = super::layout();
        let titles: Vec<&str> = layout.iter().map(|(t, _)| *t).collect();
        assert_eq!(titles, [super::variant::NAME, "Edit", "Window", "Help"]);
        let all: Vec<_> = layout.iter().flat_map(|(_, e)| e.iter()).collect();
        for needed in [Settings, Manual, Quit, Copy, Paste, SelectAll, CloseWindow] {
            assert!(all.contains(&&needed), "{needed:?} missing");
        }
    }
}
