use orchard_core::connection_diagnostics;
use orchard_core::core;
use orchard_core::patterns;

use core::{now_ms, Result, Settings, Snapshot};
use rusqlite::Connection;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

struct Tracker {
    service: orchard_core::service::Service,
}
type AppStatus = orchard_core::service::Status;
impl Tracker {
    fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.service.db()
    }
    fn status(&self) -> Result<AppStatus> {
        self.service.status()
    }
    fn collect(&self, app: &tauri::AppHandle) -> Result<Snapshot> {
        self.collect_with_mode(app, false)
    }
    fn collect_with_mode(&self, app: &tauri::AppHandle, reconnect: bool) -> Result<Snapshot> {
        let outcome = self.service.check(None, None, None, reconnect)?;
        let snapshot = outcome.snapshot.ok_or_else(|| {
            outcome
                .message
                .unwrap_or_else(|| "The next check is deferred.".into())
        })?;
        let current = core::load_settings(&*self.db()?)?;
        if current.notify_available
            && current.url == snapshot.url
            && !outcome.newly_available.is_empty()
        {
            let changes = outcome.newly_available;
            let title = if changes.len() == 1 {
                "Available for pickup".into()
            } else {
                format!("{} pickup options now available", changes.len())
            };
            let mut body = changes
                .iter()
                .take(3)
                .map(|row| format!("{} · {}", row.product, row.store))
                .collect::<Vec<_>>()
                .join("\n");
            if changes.len() > 3 {
                body.push_str(&format!(
                    "\nAnd {} more. Open Orchard for details.",
                    changes.len() - 3
                ));
            }
            if let Err(error) = app
                .notification()
                .builder()
                .title(title)
                .body(body)
                .sound("Ping")
                .show()
            {
                eprintln!("Availability notification: {error}");
                let _ = app.emit(
                    "notification-error",
                    "The availability check was saved, but its notification could not be sent.",
                );
            }
        }
        Ok(snapshot)
    }
}

#[tauri::command]
fn get_status(state: tauri::State<'_, Arc<Tracker>>) -> Result<AppStatus> {
    state.status()
}

#[tauri::command]
fn save_settings(settings: Settings, state: tauri::State<'_, Arc<Tracker>>) -> Result<AppStatus> {
    state.service.save_settings(&settings)
}

#[tauri::command]
async fn check_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<Snapshot> {
    let tracker = state.inner().clone();
    let notification_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || tracker.collect(&notification_app))
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("inventory-updated", ());
    result
}

#[tauri::command]
async fn reconnect_apple_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<Snapshot> {
    let tracker = state.inner().clone();
    let handle = app.clone();
    let _ = app.emit("inventory-checking", ());
    let result =
        tauri::async_runtime::spawn_blocking(move || tracker.collect_with_mode(&handle, true))
            .await
            .map_err(|error| error.to_string())?;
    let _ = app.emit("inventory-updated", ());
    result
}
#[tauri::command]
fn get_connection_diagnostics(
    state: tauri::State<'_, Arc<Tracker>>,
) -> connection_diagnostics::ConnectionReport {
    state.service.session.connection_report()
}
#[tauri::command]
fn export_connection_diagnostics(state: tauri::State<'_, Arc<Tracker>>) -> Result<String> {
    state.service.session.export_diagnostics()
}

#[tauri::command]
async fn open_apple_session(state: tauri::State<'_, Arc<Tracker>>) -> Result<()> {
    let tracker = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || tracker.service.open_session())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn hide_apple_session(state: tauri::State<'_, Arc<Tracker>>) -> Result<()> {
    state.service.session.hide()
}
#[tauri::command]
fn get_catalog(state: tauri::State<'_, Arc<Tracker>>) -> orchard_core::catalog::Catalog {
    state.service.catalog()
}
#[tauri::command]
async fn refresh_catalog(
    category: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<orchard_core::catalog::Catalog> {
    let tracker = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || tracker.service.catalog.refresh(&category))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn add_catalog_page(
    url: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<orchard_core::catalog::Catalog> {
    let tracker = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || tracker.service.catalog.add_page(&url))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_history(
    start_ms: i64,
    end_ms: i64,
    store_id: String,
    product: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<Vec<core::HistoryPoint>> {
    let db = state.db()?;
    let settings = core::load_settings(&db)?;
    core::history(&db, &settings.url, start_ms, end_ms, &store_id, &product)
}

#[tauri::command]
fn get_history_view(
    history_key: Option<String>,
    start_ms: i64,
    end_ms: i64,
    store_id: String,
    product: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<orchard_core::history_view::HistoryView> {
    let catalog = state.service.catalog();
    let db = state.db()?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let view = orchard_core::history_view::read(
        &tx,
        &catalog,
        history_key.as_deref(),
        start_ms,
        end_ms,
        &store_id,
        &product,
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(view)
}

#[tauri::command]
async fn get_patterns(
    history_key: Option<String>,
    start_ms: i64,
    end_ms: i64,
    store_id: String,
    product: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<patterns::PatternInsights> {
    let tracker = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (settings, samples) = {
            let db = tracker.db()?;
            let mut settings = core::load_settings(&db)?;
            if let Some(key) = history_key.filter(|key| !key.is_empty() && key != &settings.url) {
                let exists = db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE url=?1)",
                        [&key],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(|e| e.to_string())?;
                if !exists {
                    return Err("This device has no saved history.".into());
                }
                settings.url = key;
            }
            let samples =
                patterns::read_samples(&db, &settings, start_ms, end_ms, &store_id, &product)?;
            (settings, samples)
        }; // Release the collector's database lock before doing the analysis.
        Ok(patterns::analyze(&samples, &settings, start_ms, end_ms))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_last_available(
    store_id: String,
    product: String,
    state: tauri::State<'_, Arc<Tracker>>,
) -> Result<Vec<core::LastAvailable>> {
    let db = state.db()?;
    let settings = core::load_settings(&db)?;
    core::last_available(&db, &settings.url, &store_id, &product)
}

#[tauri::command]
fn get_latest(state: tauri::State<'_, Arc<Tracker>>) -> Result<Option<Snapshot>> {
    let db = state.db()?;
    let settings = core::load_settings(&db)?;
    core::latest_snapshot(&db, &settings.url)
}

#[tauri::command]
fn get_snapshot(id: i64, state: tauri::State<'_, Arc<Tracker>>) -> Result<Snapshot> {
    core::snapshot_by_id(&*state.db()?, id)
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn test_notification(app: tauri::AppHandle) -> Result<()> {
    app.notification()
        .builder()
        .title("Orchard test notification")
        .body("Availability alerts are ready. Orchard will notify you when a pickup option becomes available.")
        .sound("Ping")
        .show()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn save_export(app: tauri::AppHandle, name: String, content: String) -> Result<String> {
    use std::io::Write;
    if name.len() > 100
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || !(name.ends_with(".csv") || name.ends_with(".json"))
    {
        return Err("Invalid export filename.".into());
    }
    let folder = app
        .path()
        .download_dir()
        .map_err(|e| e.to_string())?
        .join("Orchard");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let (stem, extension) = name.rsplit_once('.').ok_or("Invalid export filename.")?;
    let path = folder.join(format!("{stem}-{}.{extension}", now_ms()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("inventory.sqlite3");
            let service =
                orchard_core::service::Service::new(&path).map_err(std::io::Error::other)?;
            let tracker = Arc::new(Tracker { service });
            app.manage(tracker.clone());
            let termination_handle = app.handle().clone();
            ctrlc::set_handler(move || termination_handle.exit(0))?;

            let notification_handle = app.handle().clone();
            app.notification()
                .on_action(move |_| show_window(&notification_handle))?;
            let open =
                tauri::menu::MenuItem::with_id(app, "open", "Open Orchard", true, None::<&str>)?;
            let quit =
                tauri::menu::MenuItem::with_id(app, "quit", "Quit Orchard", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&open, &quit])?;
            tauri::tray::TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Orchard · scheduled Apple pickup checks")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                if let Ok(status) = tracker.status() {
                    if status.settings.enabled
                        && !status.busy
                        && status.next_due.is_some_and(|at| at <= now_ms())
                    {
                        let _ = handle.emit("inventory-checking", ());
                        if let Err(error) = tracker.collect(&handle) {
                            tracker.service.defer_error();
                            let _ = handle.emit("inventory-error", error);
                        }
                        let _ = handle.emit("inventory-updated", ());
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1000));
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_catalog,
            refresh_catalog,
            add_catalog_page,
            save_settings,
            check_now,
            open_apple_session,
            reconnect_apple_session,
            get_connection_diagnostics,
            export_connection_diagnostics,
            hide_apple_session,
            get_history,
            get_history_view,
            get_patterns,
            get_last_available,
            get_latest,
            get_snapshot,
            test_notification,
            save_export
        ])
        .build(tauri::generate_context!())
        .expect("Unable to start Orchard")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<Arc<Tracker>>().service.session.shutdown();
            }
        });
}
