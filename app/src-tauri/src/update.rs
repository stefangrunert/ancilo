//! App updates (M9): the Tauri updater with **signed update archives** – the
//! manifest (`latest.json`, over HTTPS) carries the archive's signature, and
//! the signature must also cover the announced version, so an older release
//! cannot be passed off as a newer one. Never a downgrade.
//!
//! Looking for updates is network traffic: it happens on the user's request
//! ("Check for updates…") or automatically only after they allowed it
//! (`set_update_settings`). Installing always waits for a click.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager, Runtime, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};

/// What the window (the header's update button, System › Updates) and the
/// menu bar icon share: a found update, and whether a check runs.
#[derive(Default)]
pub struct Updates {
    pending: Mutex<Option<Update>>,
    checking: AtomicBool,
    /// The menu bar icon's item ("Check for Updates…" / "Install … and Restart").
    pub item: Mutex<Option<MenuItem<Wry>>>,
}

/// For the window.
#[derive(Debug, serde::Serialize)]
pub struct View {
    /// Updates exist in this build (never in Ancilo Dev).
    pub configured: bool,
    pub current: String,
    pub available: Option<Available>,
    pub checking: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct Available {
    pub version: String,
    /// What is new (from the release's changelog).
    pub notes: Option<String>,
}

pub fn view(app: &AppHandle) -> View {
    let u = app.state::<Updates>();
    View {
        configured: configured(app),
        current: app.package_info().version.to_string(),
        available: u.pending.lock().unwrap().as_ref().map(|p| Available {
            version: p.version.clone(),
            notes: p.body.clone(),
        }),
        checking: u.checking.load(Ordering::SeqCst),
    }
}

fn menu_text(app: &AppHandle, text: &str) {
    if let Some(item) = app.state::<Updates>().item.lock().unwrap().as_ref() {
        let _ = item.set_text(text);
    }
}

/// Looks for an update (`asked`: the user clicked – else the daily check
/// they allowed); a found one waits for their click to install.
pub async fn check_now(app: &AppHandle, asked: bool) -> Result<View, String> {
    let u = app.state::<Updates>();
    if u.checking.swap(true, Ordering::SeqCst) {
        return Ok(view(app));
    }
    if asked {
        menu_text(app, "Checking for updates…");
    }
    let found = check(app, asked).await;
    u.checking.store(false, Ordering::SeqCst);
    match found {
        Ok(Some(update)) => {
            menu_text(
                app,
                &format!("Install Ancilo {} and Restart", update.version),
            );
            *u.pending.lock().unwrap() = Some(update);
        }
        Ok(None) => {
            *u.pending.lock().unwrap() = None;
            if asked {
                menu_text(app, "Ancilo is up to date");
            }
        }
        Err(e) => {
            if asked {
                menu_text(app, &format!("Update check failed: {e}"));
            }
            return Err(e);
        }
    }
    Ok(view(app))
}

/// Installs the update found (checking first if none is known); the
/// caller restarts the app.
pub async fn install_pending(app: &AppHandle) -> Result<(), String> {
    if app.state::<Updates>().pending.lock().unwrap().is_none() {
        check_now(app, true).await?;
    }
    let Some(update) = app.state::<Updates>().pending.lock().unwrap().take() else {
        return Err("Ancilo is up to date".into());
    };
    menu_text(app, "Installing update…");
    let result = install(app, update).await;
    if let Err(e) = &result {
        menu_text(app, &format!("Update failed: {e}"));
    }
    result
}

/// Whether the release has an updater key (the owner creates it for a
/// release). Without one there are no updates.
pub fn configured<R: Runtime>(app: &AppHandle<R>) -> bool {
    // The development app never updates itself to a release.
    !crate::variant::DEV
        && app
            .config()
            .plugins
            .0
            .get("updater")
            .and_then(|u| u["pubkey"].as_str())
            .is_some_and(|k| !k.trim().is_empty())
}

/// A newer, correctly announced release – `None` if this is the latest.
/// `asked`: the user clicked (else Ancilo's daily check, if allowed).
pub async fn check<R: Runtime>(app: &AppHandle<R>, asked: bool) -> Result<Option<Update>, String> {
    if !configured(app) {
        return Err("updates are not set up in this build".into());
    }
    let updater = app.updater().map_err(|e| e.to_string())?;
    let result = updater.check().await.map_err(|e| e.to_string());
    let endpoint = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u["endpoints"][0].as_str())
        .unwrap_or_default()
        .to_string();
    log(
        app,
        "update_check",
        "version",
        &endpoint,
        asked,
        result.as_ref().err().map(String::as_str),
    );
    result
}

/// Downloads, verifies the signature, installs; the app restarts afterwards
/// (the daemon of the old version is replaced on the next start).
pub async fn install<R: Runtime>(app: &AppHandle<R>, update: Update) -> Result<(), String> {
    let (url, version) = (update.download_url.to_string(), update.version.clone());
    let result = update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string());
    log(
        app,
        "update_download",
        &format!("Ancilo {version}"),
        &url,
        true,
        result.as_ref().err().map(String::as_str),
    );
    result
}

/// The updater runs here, in the app: what it sent goes into the user's log
/// of what left this computer (System › What left this Mac).
fn log<R: Runtime>(
    app: &AppHandle<R>,
    purpose: &str,
    subject: &str,
    url: &str,
    asked: bool,
    error: Option<&str>,
) {
    if let Some(d) = app.try_state::<crate::daemon::Daemon>() {
        let _ = crate::daemon::op_with(
            &d,
            "record_departure",
            &serde_json::json!({
                "purpose": purpose,
                "subject": subject,
                "by": if asked { "you" } else { "ancilo" },
                "url": url,
                "error": error,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use base64::Engine;
    use serde_json::json;
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use tauri_plugin_updater::UpdaterExt;

    type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

    /// A tiny update server on 127.0.0.1.
    fn serve() -> (String, Files) {
        let files: Files = Arc::default();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let f = files.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = f.lock().unwrap().get(&path).cloned();
                let _ = match body {
                    Some(b) => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        );
                        s.write_all(&b)
                    }
                    None => write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                };
            }
        });
        (base, files)
    }

    fn b64(s: String) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    fn sign(kp: &minisign::KeyPair, data: &[u8], version: &str) -> String {
        let comment = format!("timestamp:1700000000\tfile:Ancilo.app.tar.gz\tversion:{version}");
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(data),
            Some(&comment),
            None,
        )
        .unwrap();
        b64(sig.into_string())
    }

    fn platform() -> String {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            o => o,
        };
        format!("{os}-{}", std::env::consts::ARCH)
    }

    /// The shipped configuration carries the project's updater key (a minisign
    /// public key) and asks for version-bound signatures.
    #[test]
    fn the_release_configuration_has_the_updater_key() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let u = &conf["plugins"]["updater"];
        let key = base64::engine::general_purpose::STANDARD
            .decode(u["pubkey"].as_str().unwrap())
            .unwrap();
        assert!(
            String::from_utf8_lossy(&key).starts_with("untrusted comment: minisign public key")
        );
        assert_eq!(u["requireSignedVersion"], true);
        assert!(u["endpoints"][0].as_str().unwrap().starts_with("https://"));
    }

    // covers: M9-AC-03
    #[test]
    fn only_correctly_signed_newer_updates_are_accepted() {
        let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let (base, files) = serve();
        let mut ctx = mock_context(noop_assets());
        ctx.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey": b64(key.pk.to_box().unwrap().into_string()),
                "endpoints": [format!("{base}/latest.json")],
                "requireSignedVersion": true
            }),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .unwrap();
        assert!(super::configured(app.handle()));
        let archive = b"the new Ancilo.app, packed".to_vec();
        let publish = |version: &str, signature: &str, bytes: &[u8]| {
            let mut f = files.lock().unwrap();
            f.insert("/Ancilo.app.tar.gz".into(), bytes.to_vec());
            let manifest = json!({
                "version": version,
                "platforms": { platform(): { "url": format!("{base}/Ancilo.app.tar.gz"), "signature": signature } }
            });
            f.insert(
                "/latest.json".into(),
                serde_json::to_vec(&manifest).unwrap(),
            );
        };
        let download = || {
            tauri::async_runtime::block_on(async {
                let update = app
                    .handle()
                    .updater()
                    .unwrap()
                    .check()
                    .await
                    .map_err(|e| e.to_string())?;
                match update {
                    None => Ok(None),
                    Some(u) => u
                        .download(|_, _| {}, || {})
                        .await
                        .map(Some)
                        .map_err(|e| e.to_string()),
                }
            })
        };

        // A correctly signed, newer release is accepted.
        publish("9.9.9", &sign(&key, &archive, "9.9.9"), &archive);
        assert_eq!(download().unwrap(), Some(archive.clone()));
        // Changed bytes with the original signature: rejected.
        publish(
            "9.9.9",
            &sign(&key, &archive, "9.9.9"),
            b"the new Ancilo.app, changed",
        );
        assert!(download().is_err());
        // Signed with another key: rejected.
        publish("9.9.9", &sign(&other, &archive, "9.9.9"), &archive);
        assert!(download().is_err());
        // No signature: rejected.
        publish("9.9.9", "", &archive);
        assert!(download().is_err());
        // A signature for another version (an old release announced as new): rejected.
        publish("9.9.9", &sign(&key, &archive, "0.0.9"), &archive);
        assert!(download().is_err());
        // Older than this app (0.1.0): never offered.
        publish("0.0.9", &sign(&key, &archive, "0.0.9"), &archive);
        assert_eq!(download().unwrap(), None);
    }
}
