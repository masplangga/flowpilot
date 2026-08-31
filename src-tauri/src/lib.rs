use std::time::Duration;
use tauri::Manager;

mod account_store;
mod license_store;
mod webview_manager;
mod webview_download_bridge;
mod dialog_thread_experiment;
#[cfg(all(windows, feature = "diag"))]
mod webview_diagnostics;

async fn run_on_ui_thread<F, T>(app: &tauri::AppHandle, operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(operation());
    })
    .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "WebView operation timed out".to_string())?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_google_flow(
    app: tauri::AppHandle,
    account_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let operation_app = app.clone();
    run_on_ui_thread(&app, move || {
        webview_manager::open(&operation_app, account_id, x, y, width, height)
    })
    .await
}

#[tauri::command]
async fn close_google_flow(
    app: tauri::AppHandle,
    account_id: Option<String>,
) -> Result<(), String> {
    let operation_app = app.clone();
    run_on_ui_thread(&app, move || {
        webview_manager::close(&operation_app, account_id)
    })
    .await
}

#[tauri::command]
async fn resize_google_flow(
    app: tauri::AppHandle,
    account_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let operation_app = app.clone();
    run_on_ui_thread(&app, move || {
        webview_manager::resize(&operation_app, account_id, x, y, width, height)
    })
    .await
}

#[tauri::command]
async fn remove_google_flow_account(
    app: tauri::AppHandle,
    account_id: String,
) -> Result<bool, String> {
    let operation_app = app.clone();
    run_on_ui_thread(&app, move || {
        webview_manager::remove(&operation_app, account_id)
    })
    .await
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    if url != "https://tokotelegram.com/toko/flowpilot" {
        return Err("unsupported external URL".to_string());
    }
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn expand_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.set_resizable(true).map_err(|e| e.to_string())?;
    window
        .set_size(tauri::Size::Physical(tauri::PhysicalSize {
            width: 1280,
            height: 800,
        }))
        .map_err(|e| e.to_string())?;
    window
        .set_min_size(Some(tauri::Size::Physical(tauri::PhysicalSize {
            width: 1000,
            height: 700,
        })))
        .map_err(|e| e.to_string())?;
    window.center().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    dialog_thread_experiment::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(webview_manager::WebviewManager::default())
        .manage(webview_download_bridge::DownloadState::default())
        .invoke_handler(tauri::generate_handler![
            expand_main_window,
            open_external_url,
            open_google_flow,
            close_google_flow,
            resize_google_flow,
            remove_google_flow_account,
            webview_download_bridge::begin_blob_download,
            webview_download_bridge::write_blob_download_chunk,
            webview_download_bridge::complete_blob_download,
            webview_download_bridge::cancel_blob_download,
            dialog_thread_experiment::debug_trigger_isolated_dialog,
            #[cfg(all(windows, feature = "diag"))]
            webview_download_bridge::diagnostic_save_file,
            license_store::get_device_id,
            license_store::get_license_state,
            license_store::activate_license,
            license_store::validate_license,
            license_store::clear_license_state,
            account_store::load_accounts,
            account_store::save_accounts
        ])
        .run(tauri::generate_context!())
        .expect("error while running Flowpilot");
}
