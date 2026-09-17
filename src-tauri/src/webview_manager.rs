use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::path::BaseDirectory;
use tauri::webview::WebviewBuilder;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl};
#[cfg(windows)]
use webview2_com::{DownloadStartingEventHandler, take_pwstr};
#[cfg(windows)]
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2_4, ICoreWebView2DownloadStartingEventArgs};
#[cfg(windows)]
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

const GOOGLE_FLOW_URL: &str = "https://flow.google";
const DOLA_URL: &str = "https://www.dola.com/chat/";
const MIGOO_URL: &str = "https://migoo.ai/";
const GEMINI_URL: &str = "https://gemini.google.com/app";
const WEBVIEW_LABEL_PREFIX: &str = "google-flow";
const MAX_CACHED_WEBVIEWS: usize = 10;

#[cfg(windows)]
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().split(';').next().unwrap_or("") {
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        _ => None,
    }
}

#[cfg(windows)]
fn attach_google_flow_download_handler(webview: &ICoreWebView2) -> Result<(), String> {
    let webview4 = webview.cast::<ICoreWebView2_4>().map_err(|e| e.to_string())?;
    let mut token = 0;
    unsafe {
        webview4.add_DownloadStarting(
            &DownloadStartingEventHandler::create(Box::new(
                move |_, args: Option<ICoreWebView2DownloadStartingEventArgs>| {
                    let Some(args) = args else { return Ok(()); };
                    args.SetHandled(false)?;
                    args.SetCancel(false)?;
                    let mut suggested = PWSTR::null();
                    let mut filename = if args.ResultFilePath(&mut suggested).is_ok() {
                        let path = take_pwstr(suggested);
                        std::path::Path::new(&path).file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "flowpilot-download".into())
                    } else { "flowpilot-download".into() };
                    let mime_type = args.DownloadOperation().ok().and_then(|operation| {
                        let mut mime = PWSTR::null();
                        if operation.MimeType(&mut mime).is_ok() {
                            Some(take_pwstr(mime))
                        } else {
                            None
                        }
                    });
                    let mime_extension = mime_type.as_deref().and_then(extension_for_mime);
                    let filename_extension = std::path::Path::new(&filename)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(|extension| extension.to_ascii_lowercase());
                    let mime_matches_filename = filename_extension.as_deref().zip(mime_extension)
                        .is_some_and(|(filename_extension, mime_extension)| filename_extension == mime_extension);
                    if let Some(extension) = mime_extension.filter(|_| !mime_matches_filename) {
                        filename.push('.');
                        filename.push_str(extension);
                    }
                    let original_extension = std::path::Path::new(&filename)
                        .extension()
                        .map(|extension| extension.to_os_string());
                    let Some(mut destination) = rfd::FileDialog::new().set_file_name(&filename).save_file() else {
                        args.SetCancel(true)?;
                        return Ok(());
                    };
                    if destination.extension().is_none() {
                        if let Some(extension) = original_extension {
                            destination.set_extension(extension);
                        }
                    }
                    let destination = HSTRING::from(destination.as_os_str());
                    args.SetResultFilePath(PCWSTR(destination.as_ptr()))?;
                    args.SetHandled(true)?;
                    Ok(())
                },
            )),
            &mut token,
        ).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
fn attach_gemini_download_handler(webview: &ICoreWebView2) -> Result<(), String> {
    let webview4 = webview.cast::<ICoreWebView2_4>().map_err(|e| e.to_string())?;
    let mut token = 0;
    unsafe {
        webview4.add_DownloadStarting(
            &DownloadStartingEventHandler::create(Box::new(
                move |_, args: Option<ICoreWebView2DownloadStartingEventArgs>| {
                    let Some(args) = args else { return Ok(()); };
                    let mut suggested = PWSTR::null();
                    let filename = if args.ResultFilePath(&mut suggested).is_ok() {
                        let path = take_pwstr(suggested);
                        std::path::Path::new(&path).file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "video.mp4".into())
                    } else { "video.mp4".into() };
                    let mime_type = args.DownloadOperation().ok().and_then(|operation| {
                        let mut mime = PWSTR::null();
                        if operation.MimeType(&mut mime).is_ok() {
                            Some(take_pwstr(mime))
                        } else {
                            None
                        }
                    });
                    let image_mime = mime_type.as_deref()
                        .map(|mime| mime.trim().to_ascii_lowercase().starts_with("image/"))
                        .unwrap_or(false);
                    let image_filename = std::path::Path::new(&filename)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg"))
                        .unwrap_or(false);
                    if image_mime || image_filename {
                        return Ok(());
                    }
                    let Some(mut destination) = rfd::FileDialog::new().set_file_name(&filename).save_file() else {
                        args.SetCancel(true)?;
                        return Ok(());
                    };
                    if destination.extension().is_none() {
                        destination.set_extension("mp4");
                    }
                    let destination = HSTRING::from(destination.as_os_str());
                    args.SetResultFilePath(PCWSTR(destination.as_ptr()))?;
                    args.SetHandled(true)?;
                    Ok(())
                },
            )),
            &mut token,
        ).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub struct WebviewManager {
    active_account_id: Mutex<Option<String>>,
    active_provider: Mutex<Option<String>>,
    visible: Mutex<bool>,
    cached_accounts: Mutex<HashMap<String, u64>>,
    usage_counter: Mutex<u64>,
    operation: Mutex<()>,
}

impl Default for WebviewManager {
    fn default() -> Self {
        Self {
            active_account_id: Mutex::new(None),
            active_provider: Mutex::new(None),
            visible: Mutex::new(false),
            cached_accounts: Mutex::new(HashMap::new()),
            usage_counter: Mutex::new(0),
            operation: Mutex::new(()),
        }
    }
}

fn webview_label(account_id: &str, provider: Option<&str>) -> String {
    format!("{}-{account_id}", match provider { Some("dola") => "dola", Some("migoo") => "migoo", Some("gemini") => "gemini", _ => WEBVIEW_LABEL_PREFIX })
}

fn touch_account(state: &WebviewManager, account_id: &str) -> Result<(), String> {
    let mut counter = state
        .usage_counter
        .lock()
        .map_err(|_| "webview state unavailable")?;
    *counter = counter.wrapping_add(1);
    let mut cached = state
        .cached_accounts
        .lock()
        .map_err(|_| "webview state unavailable")?;
    cached.insert(account_id.to_string(), *counter);
    Ok(())
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty()
        || account_id.len() > 128
        || !account_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err("invalid account id".to_string());
    }
    Ok(())
}

fn profile_path<R: Runtime>(app: &AppHandle<R>, account_id: &str) -> Result<PathBuf, String> {
    let root = app
        .path()
        .resolve("webview-profiles", BaseDirectory::AppLocalData)
        .map_err(|e| e.to_string())?;
    let profile = root.join(account_id);
    if profile.parent() != Some(root.as_path()) {
        return Err("invalid account profile path".to_string());
    }
    Ok(profile)
}

fn validate_bounds(x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    if ![x, y, width, height].iter().all(|value| value.is_finite())
        || x < 0.0
        || y < 0.0
        || width <= 0.0
        || height <= 0.0
    {
        return Err("invalid WebView bounds".to_string());
    }
    Ok(())
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    account_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    provider: String,
) -> Result<(), String> {
    let state = app.state::<WebviewManager>();
    let operation = state
        .operation
        .lock()
        .map_err(|_| "webview state unavailable")?;
    validate_account_id(&account_id)?;
    validate_bounds(x, y, width, height)?;
    let window = app
        .get_window("main")
        .ok_or_else(|| "main window not found".to_string())?;

    let requested_label = format!("{}-{}", match provider.as_str() { "dola" => "dola", "migoo" => "migoo", "gemini" => "gemini", _ => "google-flow" }, account_id);
    let active_account = state
        .active_account_id
        .lock()
        .map_err(|_| "webview state unavailable")?
        .clone();
    if let Some(active_id) = active_account.as_deref() {
        if active_id != account_id {
            let active_provider = state.active_provider.lock().map_err(|_| "webview state unavailable")?.clone();
            if let Some(webview) = app.get_webview(&webview_label(active_id, active_provider.as_deref())) {
                webview.hide().map_err(|e| e.to_string())?;
            }
        }
    }

    if let Some(webview) = app.get_webview(&requested_label) {
        webview
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        webview
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
        webview.show().map_err(|e| e.to_string())?;
        *state
            .active_account_id
            .lock()
            .map_err(|_| "webview state unavailable")? = Some(account_id.clone());
        *state.active_provider.lock().map_err(|_| "webview state unavailable")? = Some(provider.clone());
        *state
            .visible
            .lock()
            .map_err(|_| "webview state unavailable")? = true;
        touch_account(&state, &account_id)?;
        return Ok(());
    }

    {
        let mut cached = state
            .cached_accounts
            .lock()
            .map_err(|_| "webview state unavailable")?;
        if cached.len() >= MAX_CACHED_WEBVIEWS {
            let eviction = cached
                .iter()
                .filter(|(id, _)| Some(id.as_str()) != active_account.as_deref())
                .min_by_key(|(_, last_used)| **last_used)
                .map(|(id, _)| id.clone());
            if let Some(evicted_id) = eviction {
                let evicted_label = webview_label(&evicted_id, None);
                crate::webview_download_bridge::cancel_for_webview(
                    &app.state::<crate::webview_download_bridge::DownloadState>(),
                    &evicted_label,
                );
                if let Some(webview) = app.get_webview(&evicted_label) {
                    webview.close().map_err(|e| e.to_string())?;
                }
                cached.remove(&evicted_id);
            }
        }
    }

    let profile = profile_path(app, &account_id)?;
    let url = WebviewUrl::External(
        match provider.as_str() { "dola" => DOLA_URL, "migoo" => MIGOO_URL, "gemini" => GEMINI_URL, _ => GOOGLE_FLOW_URL }
            .parse()
            .map_err(|_| "invalid Google Flow URL")?,
    );
    let initialization_script = match provider.as_str() {
        "google-flow" => "",
        "gemini" => crate::webview_download_bridge::GEMINI_BLOB_SCRIPT,
        _ => crate::webview_download_bridge::INIT_SCRIPT,
    };

    let builder = WebviewBuilder::new(requested_label.clone(), url)
        .data_directory(profile)
        .initialization_script_for_all_frames(initialization_script)
        .on_navigation(|url| url.scheme() == "https")
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny);
    let webview = window
        .add_child(
            builder,
            tauri::LogicalPosition::new(x, y),
            tauri::LogicalSize::new(width, height),
        )
        .map_err(|e| e.to_string())?;

    #[cfg(windows)]
    if matches!(provider.as_str(), "google-flow" | "gemini") {
        let is_gemini = provider.as_str() == "gemini";
        webview.with_webview(move |platform| {
            if let Ok(native) = unsafe { platform.controller().CoreWebView2() } {
                let result = if is_gemini {
                    attach_gemini_download_handler(&native)
                } else {
                    attach_google_flow_download_handler(&native)
                };
                let _ = result;
            }
        }).map_err(|e| e.to_string())?;
    }


    *state.active_provider.lock().map_err(|_| "webview state unavailable")? = Some(provider);
    *state
        .active_account_id
        .lock()
        .map_err(|_| "webview state unavailable")? = Some(account_id.clone());
    *state
        .visible
        .lock()
        .map_err(|_| "webview state unavailable")? = true;
    touch_account(&state, &account_id)?;
    drop(operation);
    Ok(())
}

pub fn close<R: Runtime>(app: &AppHandle<R>, account_id: Option<String>, provider: Option<String>) -> Result<(), String> {
    let state = app.state::<WebviewManager>();
    let operation = state
        .operation
        .lock()
        .map_err(|_| "webview state unavailable")?;
    let active_account = state
        .active_account_id
        .lock()
        .map_err(|_| "webview state unavailable")?
        .clone();
    if let Some(active_id) = active_account {
        if account_id
            .as_deref()
            .map_or(true, |requested_id| requested_id == active_id)
        {
            let active_provider = provider.or_else(|| state.active_provider.lock().ok().and_then(|p| p.clone()));
            if let Some(webview) = app.get_webview(&webview_label(&active_id, active_provider.as_deref())) {
                webview.hide().map_err(|e| e.to_string())?;
            }
        }
    }
    *state
        .visible
        .lock()
        .map_err(|_| "webview state unavailable")? = false;
    drop(operation);
    Ok(())
}

pub fn resize<R: Runtime>(
    app: &AppHandle<R>,
    account_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    validate_bounds(x, y, width, height)?;
    let state = app.state::<WebviewManager>();
    let active_account = state
        .active_account_id
        .lock()
        .map_err(|_| "webview state unavailable")?
        .clone();
    if active_account.as_deref() != Some(account_id.as_str()) {
        return Ok(());
    }
    let active_provider = state.active_provider.lock().map_err(|_| "webview state unavailable")?.clone();
    if let Some(webview) = app.get_webview(&webview_label(&account_id, active_provider.as_deref())) {
        webview
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        webview
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn remove<R: Runtime>(app: &AppHandle<R>, account_id: String) -> Result<bool, String> {
    validate_account_id(&account_id)?;
    let state = app.state::<WebviewManager>();
    let _operation = state
        .operation
        .lock()
        .map_err(|_| "webview state unavailable")?;
    let label = webview_label(&account_id, None);

    crate::webview_download_bridge::cancel_for_webview(
        &app.state::<crate::webview_download_bridge::DownloadState>(),
        &label,
    );
    if let Some(webview) = app.get_webview(&label) {
        webview.close().map_err(|e| e.to_string())?;
    }
    {
        let mut cached = state
            .cached_accounts
            .lock()
            .map_err(|_| "webview state unavailable")?;
        cached.remove(&account_id);
    }
    {
        let mut active = state
            .active_account_id
            .lock()
            .map_err(|_| "webview state unavailable")?;
        if active.as_deref() == Some(account_id.as_str()) {
            *active = None;
            *state
                .visible
                .lock()
                .map_err(|_| "webview state unavailable")? = false;
        }
    }

    let profile = profile_path(app, &account_id)?;
    if !profile.exists() {
        return Ok(true);
    }
    match std::fs::remove_dir_all(&profile) {
        Ok(()) => Ok(true),
        Err(error) => {
            eprintln!("[flowpilot-webview] profile cleanup pending account={account_id}: {error}");
            Ok(false)
        }
    }
}
