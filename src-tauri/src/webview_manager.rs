use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

use tauri::path::BaseDirectory;
use tauri::webview::{PageLoadEvent, WebviewBuilder};
use tauri::{AppHandle, Manager, Runtime, WebviewUrl};
#[cfg(windows)]
use webview2_com::{DownloadStartingEventHandler, take_pwstr};
#[cfg(windows)]
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2_4, ICoreWebView2DownloadStartingEventArgs};
#[cfg(windows)]
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

const GOOGLE_FLOW_URL: &str = "https://flow.google/";
const STORYBOARD_URL: &str = "https://flow.google.com/shared/tool/c66ec783-8729-4035-9934-b7f48907365c";
const DOLA_URL: &str = "https://www.dola.com/chat/";

const STORYBOARD_LOGIN_SCRIPT: &str = r#"(() => {
  if (window.top !== window || window.__flowpilotStoryboardBridgeInstalled) return;
  window.__flowpilotStoryboardBridgeInstalled = true;
  const toolUrl = 'https://flow.google.com/shared/tool/c66ec783-8729-4035-9934-b7f48907365c';
  const publicMarkers = ['unlock your best creative work', 'pricing', 'meet your agent'];
  const authenticatedSelectors = [
    '[aria-label*="account" i]',
    '[data-testid*="project" i]',
    'a[href*="/project"]',
    'button[aria-label*="new" i]'
  ];
  const isAuthenticatedFlow = () => {
    const body = (document.body?.innerText || '').toLowerCase();
    if (publicMarkers.some((marker) => body.includes(marker))) return false;
    return authenticatedSelectors.some((selector) => document.querySelector(selector));
  };
  const check = () => {
    if (!window.__flowpilotStoryboardMode) return;
    if (location.hostname !== 'flow.google.com' || location.href.startsWith(toolUrl)) return;
    if (location.pathname === '/' && isAuthenticatedFlow()) location.assign(toolUrl);
  };
  const timer = window.setInterval(check, 1000);
  window.addEventListener('beforeunload', () => window.clearInterval(timer), { once: true });
  check();
})()"#;
const STORYBOARD_MODE_OFF_SCRIPT: &str = "window.__flowpilotStoryboardMode = false;";
const STORYBOARD_MODE_ON_SCRIPT: &str = "window.__flowpilotStoryboardMode = true;";
const GEMINI_TTS_BRANDING_SCRIPT: &str = r#"(() => {
  if (window.top !== window || window.__flowpilotTtsBrandingCleanupInstalled) return;
  window.__flowpilotTtsBrandingCleanupInstalled = true;

  const hideGeminiBranding = () => {
    const candidates = Array.from(document.querySelectorAll('a, button, [role="button"], div, span'))
      .filter((element) => {
        if ((element.innerText || element.textContent || '').trim() !== 'Gemini') return false;
        const rect = element.getBoundingClientRect();
        return rect.top >= 0 && rect.top < 120 && rect.left >= 0 && rect.left < 280
          && rect.width > 0 && rect.width <= 280 && rect.height > 0 && rect.height <= 90;
      });

    const branding = candidates.reduce((largest, element) => {
      if (!largest) return element;
      const rect = element.getBoundingClientRect();
      const largestRect = largest.getBoundingClientRect();
      return rect.width * rect.height > largestRect.width * largestRect.height ? element : largest;
    }, null);

    if (branding && branding.dataset.flowpilotTtsBrandingHidden !== 'true') {
      branding.dataset.flowpilotTtsBrandingHidden = 'true';
      branding.style.setProperty('display', 'none', 'important');
    }
  };

  const install = () => {
    if (!document.documentElement) {
      requestAnimationFrame(install);
      return;
    }
    const observer = new MutationObserver(hideGeminiBranding);
    observer.observe(document.documentElement, { childList: true, subtree: true, characterData: true });
    hideGeminiBranding();
  };

  install();
})()"#;
const MIGOO_URL: &str = "https://migoo.ai/";
const GEMINI_URL: &str = "https://gemini.google.com/app";
const GEMINI_TTS_URL: &str = "https://share.gemini.google/jLVZLHqihx0e";
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
    format!("{}-{account_id}", match provider { Some("dola") => "dola", Some("migoo") => "migoo", Some("gemini") => "gemini", Some("gemini-tts") => "flowpilot-tts", _ => WEBVIEW_LABEL_PREFIX })
}

fn touch_webview(state: &WebviewManager, label: &str) -> Result<(), String> {
    let mut counter = state
        .usage_counter
        .lock()
        .map_err(|_| "webview state unavailable")?;
    *counter = counter.wrapping_add(1);
    let mut cached = state
        .cached_accounts
        .lock()
        .map_err(|_| "webview state unavailable")?;
    cached.insert(label.to_string(), *counter);
    Ok(())
}

fn safe_tts_resume_url(raw_url: &str) -> Option<String> {
    let clean_url = raw_url.split(['?', '#']).next()?.trim();
    let route = clean_url.strip_prefix("https://gemini.google.com/")?;
    let (section, remainder) = route.split_once('/')?;
    if !matches!(section, "share" | "gem") {
        return None;
    }
    let id = remainder.split('/').next()?;
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return None;
    }
    Some(format!("https://gemini.google.com/{section}/{id}"))
}

fn is_gemini_tts_login_landing(host: Option<&str>, path: &str) -> bool {
    host == Some("gemini.google.com") && matches!(path, "/" | "/app")
}

fn tts_resume_path<R: Runtime>(app: &AppHandle<R>, account_id: &str) -> Result<PathBuf, String> {
    validate_account_id(account_id)?;
    let root = app
        .path()
        .resolve("flowpilot-tts", BaseDirectory::AppLocalData)
        .map_err(|error| error.to_string())?;
    let path = root.join(format!("{account_id}.txt"));
    if path.parent() != Some(root.as_path()) {
        return Err("invalid TTS resume path".to_string());
    }
    Ok(path)
}

fn load_tts_resume_url<R: Runtime>(app: &AppHandle<R>, account_id: &str) -> Result<Option<String>, String> {
    let path = tts_resume_path(app, account_id)?;
    let Ok(raw_url) = fs::read_to_string(path) else {
        return Ok(None);
    };
    Ok(safe_tts_resume_url(&raw_url))
}

fn save_tts_resume_url<R: Runtime>(app: &AppHandle<R>, account_id: &str, raw_url: &str) -> Result<(), String> {
    let Some(url) = safe_tts_resume_url(raw_url) else {
        return Ok(());
    };
    let path = tts_resume_path(app, account_id)?;
    let parent = path
        .parent()
        .ok_or_else(|| "invalid TTS resume path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(path, url).map_err(|error| error.to_string())
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
    force_navigation: bool,
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

    let requested_label = webview_label(&account_id, Some(&provider));
    let active_account = state
        .active_account_id
        .lock()
        .map_err(|_| "webview state unavailable")?
        .clone();
    let active_provider = state.active_provider.lock().map_err(|_| "webview state unavailable")?.clone();
    let active_label = active_account
        .as_deref()
        .map(|active_id| webview_label(active_id, active_provider.as_deref()));
    if let Some(label) = active_label.as_deref().filter(|label| *label != requested_label) {
        if let Some(webview) = app.get_webview(label) {
            webview.hide().map_err(|e| e.to_string())?;
        }
    }

    if let Some(webview) = app.get_webview(&requested_label) {
        let target_url = match provider.as_str() {
            "dola" => DOLA_URL,
            "migoo" => MIGOO_URL,
            "gemini" => GEMINI_URL,
            "gemini-tts" => GEMINI_TTS_URL,
            "storyboard" => STORYBOARD_URL,
            _ => GOOGLE_FLOW_URL,
        };
        let should_navigate = match provider.as_str() {
            "storyboard" | "gemini-tts" => force_navigation,
            _ => true,
        };
        let mode_script = match provider.as_str() {
            "storyboard" => Some(STORYBOARD_MODE_ON_SCRIPT),
            "google-flow" => Some(STORYBOARD_MODE_OFF_SCRIPT),
            _ => None,
        };
        if let Some(script) = mode_script {
            webview.eval(script).map_err(|e| e.to_string())?;
        }
        if should_navigate {
            webview
                .navigate(target_url.parse().map_err(|_| "invalid navigation URL")?)
                .map_err(|e| e.to_string())?;
        }
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
        touch_webview(&state, &requested_label)?;
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
                .filter(|(label, _)| {
                    Some(label.as_str()) != active_label.as_deref()
                        && label.as_str() != requested_label
                })
                .min_by_key(|(_, last_used)| **last_used)
                .map(|(label, _)| label.clone());
            if let Some(evicted_label) = eviction {
                crate::webview_download_bridge::cancel_for_webview(
                    &app.state::<crate::webview_download_bridge::DownloadState>(),
                    &evicted_label,
                );
                if let Some(webview) = app.get_webview(&evicted_label) {
                    webview.close().map_err(|e| e.to_string())?;
                }
                cached.remove(&evicted_label);
            }
        }
    }

    let profile = profile_path(app, &account_id)?;
    let initial_url = if provider == "gemini-tts" {
        load_tts_resume_url(app, &account_id)?.unwrap_or_else(|| GEMINI_TTS_URL.to_string())
    } else {
        match provider.as_str() {
            "dola" => DOLA_URL,
            "migoo" => MIGOO_URL,
            "gemini" => GEMINI_URL,
            "storyboard" => STORYBOARD_URL,
            _ => GOOGLE_FLOW_URL,
        }
        .to_string()
    };
    let url = WebviewUrl::External(
        initial_url
            .parse()
            .map_err(|_| "invalid WebView URL")?,
    );
    let initialization_script: String = match provider.as_str() {
        "google-flow" => STORYBOARD_LOGIN_SCRIPT.to_string(),
        "storyboard" => format!("window.__flowpilotStoryboardMode = true;\n{}", STORYBOARD_LOGIN_SCRIPT),
        "gemini" => crate::webview_download_bridge::GEMINI_BLOB_SCRIPT.to_string(),
        "gemini-tts" => GEMINI_TTS_BRANDING_SCRIPT.to_string(),
        _ => crate::webview_download_bridge::INIT_SCRIPT.to_string(),
    };

    let persist_tts_url = provider == "gemini-tts";
    let allow_google_popups = persist_tts_url;
    let tts_login_seen = Arc::new(AtomicBool::new(false));
    let tts_redirect_target = initial_url.clone();
    let persistence_app = app.clone();
    let persistence_account_id = account_id.clone();
    let builder = WebviewBuilder::new(requested_label.clone(), url)
        .data_directory(profile)
        .initialization_script_for_all_frames(&initialization_script)
        .on_navigation({
            let tts_login_seen = Arc::clone(&tts_login_seen);
            move |url| {
                if persist_tts_url {
                    if url.host_str() == Some("accounts.google.com") {
                        tts_login_seen.store(true, Ordering::SeqCst);
                    }
                    let _ = save_tts_resume_url(
                        &persistence_app,
                        &persistence_account_id,
                        url.as_str(),
                    );
                }
                url.scheme() == "https"
            }
        })
        .on_page_load({
            let tts_login_seen = Arc::clone(&tts_login_seen);
            move |webview, payload| {
                let loaded_url = payload.url();
                if persist_tts_url
                    && payload.event() == PageLoadEvent::Finished
                    && is_gemini_tts_login_landing(loaded_url.host_str(), loaded_url.path())
                    && tts_login_seen.swap(false, Ordering::SeqCst)
                {
                    if let Ok(target) = tts_redirect_target.parse() {
                        let _ = webview.navigate(target);
                    }
                }
            }
        })
        .on_new_window(move |url, _| {
            let host = url.host_str().unwrap_or_default();
            if allow_google_popups
                && url.scheme() == "https"
                && (host == "google.com"
                    || host.ends_with(".google.com")
                    || host == "google"
                    || host.ends_with(".google"))
            {
                tauri::webview::NewWindowResponse::Allow
            } else {
                tauri::webview::NewWindowResponse::Deny
            }
        });
    let webview = window
        .add_child(
            builder,
            tauri::LogicalPosition::new(x, y),
            tauri::LogicalSize::new(width, height),
        )
        .map_err(|e| e.to_string())?;

    #[cfg(windows)]
    if matches!(provider.as_str(), "google-flow" | "storyboard" | "gemini") {
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
    touch_webview(&state, &requested_label)?;
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
    provider: Option<String>,
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
    let requested_provider = provider.or(active_provider);
    if let Some(webview) = app.get_webview(&webview_label(&account_id, requested_provider.as_deref())) {
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
    let labels = [
        webview_label(&account_id, None),
        webview_label(&account_id, Some("dola")),
        webview_label(&account_id, Some("migoo")),
        webview_label(&account_id, Some("gemini")),
        webview_label(&account_id, Some("gemini-tts")),
    ];
    for label in &labels {
        crate::webview_download_bridge::cancel_for_webview(
            &app.state::<crate::webview_download_bridge::DownloadState>(),
            label,
        );
        if let Some(webview) = app.get_webview(label) {
            webview.close().map_err(|e| e.to_string())?;
        }
    }
    {
        let mut cached = state
            .cached_accounts
            .lock()
            .map_err(|_| "webview state unavailable")?;
        for label in &labels {
            cached.remove(label);
        }
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
            *state
                .active_provider
                .lock()
                .map_err(|_| "webview state unavailable")? = None;
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

#[cfg(test)]
mod tests {
    use super::{is_gemini_tts_login_landing, safe_tts_resume_url, webview_label};

    #[test]
    fn tts_login_landing_matches_only_gemini_home() {
        assert!(is_gemini_tts_login_landing(Some("gemini.google.com"), "/app"));
        assert!(is_gemini_tts_login_landing(Some("gemini.google.com"), "/"));
        assert!(!is_gemini_tts_login_landing(Some("gemini.google.com"), "/share/app-id"));
        assert!(!is_gemini_tts_login_landing(Some("accounts.google.com"), "/"));
    }

    #[test]
    fn tts_resume_url_keeps_only_safe_gemini_content_routes() {
        assert_eq!(
            safe_tts_resume_url(
                "https://gemini.google.com/share/662b1aa6dcbe?social_preview=false#result"
            ),
            Some("https://gemini.google.com/share/662b1aa6dcbe".to_string())
        );
        assert_eq!(
            safe_tts_resume_url("https://gemini.google.com/gem/abc_123-xy?authuser=2"),
            Some("https://gemini.google.com/gem/abc_123-xy".to_string())
        );
    }

    #[test]
    fn tts_resume_url_rejects_auth_and_external_routes() {
        assert_eq!(safe_tts_resume_url("https://gemini.google.com/app"), None);
        assert_eq!(safe_tts_resume_url("https://accounts.google.com/signin"), None);
        assert_eq!(safe_tts_resume_url("http://gemini.google.com/share/abc"), None);
        assert_eq!(safe_tts_resume_url("https://gemini.google.com.evil.test/share/abc"), None);
    }

    #[test]
    fn tts_has_its_own_view_label_for_the_gemini_account() {
        assert_eq!(webview_label("account-id", Some("gemini-tts")), "flowpilot-tts-account-id");
        assert_eq!(webview_label("account-id", Some("gemini")), "gemini-account-id");
    }
}
