//! Lifecycle-aware Google Flow blob download coordinator.
//!
//! The remote WebView only talks to Tauri IPC commands. Native dialogs, file
//! state and finalization are owned by Tauri; no WebView2 COM callback is used.
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    panic::AssertUnwindSafe,
};

use base64::Engine;
use tauri::{AppHandle, Runtime, State, Webview};
use tauri_plugin_dialog::DialogExt;

#[cfg(all(windows, feature = "diag"))]
pub(crate) fn diag(event: &str, detail: &str) { crate::webview_diagnostics::command_event(event, detail); }
#[cfg(not(all(windows, feature = "diag")))]
pub(crate) fn diag(_event: &str, _detail: &str) {}

struct Pending {
    owner: String,
    destination: PathBuf,
    partial: PathBuf,
    file: Option<File>,
    bytes: u64,
    failed: bool,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.partial);
    }
}

#[derive(Default)]
pub struct DownloadState {
    pending: Mutex<HashMap<String, Pending>>,
    last_folder: Mutex<Option<PathBuf>>,
}

fn validate_caller<R: Runtime>(webview: &Webview<R>) -> Result<String, String> {
    let label = webview.label().to_string();
    if label.starts_with("google-flow-") || label.starts_with("dola-") || label.starts_with("migoo-") {
        Ok(label)
    } else {
        Err("download command rejected for this WebView".into())
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.len() <= 80
        && !id.is_empty()
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        Ok(())
    } else {
        Err("invalid download id".into())
    }
}

fn safe_filename(value: &str) -> String {
    let mut value: String = value
        .chars()
        .map(|character| {
            if "<>:\"/\\|?*".contains(character) || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect();
    if value.trim().is_empty() {
        value = "Flowpilot_Video.mp4".into();
    }
    if !value.to_ascii_lowercase().ends_with(".mp4") {
        value.push_str(".mp4");
    }
    value
}

fn partial_path(destination: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", destination.display()))
}

const AUTO_SAVE_TO_DOWNLOADS: bool = true;
const ENABLE_NATIVE_SAVE_AS: bool = true;

fn downloads_destination(filename: &str) -> Result<PathBuf, String> {
    let profile = std::env::var_os("USERPROFILE").ok_or("user profile unavailable")?;
    let dir = PathBuf::from(profile).join("Downloads");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let base = dir.join(filename);
    if !base.exists() { return Ok(base); }
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("Flowpilot_Video");
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
    for n in 1..10000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() { return Ok(candidate); }
    }
    Err("could not choose download filename".into())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    unsafe {
        MoveFileExW(
            &HSTRING::from(source.as_os_str()),
            &HSTRING::from(destination.as_os_str()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn begin_blob_download<R: Runtime>(
    app: AppHandle<R>,
    webview: Webview<R>,
    state: State<'_, DownloadState>,
    id: String,
    filename: String,
) -> Result<bool, String> {
    diag("TauriCommand:Entered", &format!("command=begin_blob_download id={id}"));
    let owner = validate_caller(&webview)?;
    diag("begin_blob_download:CallerValidated", "");
    validate_id(&id)?;
    diag("begin_blob_download:IdValidated", "");
    {
        let pending = state.pending.lock().map_err(|_| "download state unavailable")?;
        if pending.contains_key(&id) {
            return Err("duplicate download id".into());
        }
    }

    let suggested_name = safe_filename(&filename);
    diag("begin_blob_download:FilenamePrepared", "");
    if ENABLE_NATIVE_SAVE_AS {
        let last_folder = state.last_folder.lock().map_err(|_| "download folder state unavailable")?.clone();
        diag("begin_blob_download:DialogRequestSent", "dedicated-thread");
        let selected = crate::dialog_thread_experiment::request_dialog(last_folder, suggested_name.clone()).await?;
        diag("begin_blob_download:DialogReturned", if selected.is_some() { "raw=Some(path)" } else { "raw=None" });
        let Some(mut destination) = selected else {
            diag("DownloadCancelledByUser", "");
            return Ok(false);
        };
        if destination.extension().is_none() { destination.set_extension("mp4"); }
        let partial = partial_path(&destination);
        let file = OpenOptions::new().create(true).read(true).write(true).truncate(true).open(&partial).map_err(|e| e.to_string())?;
        if let Some(parent) = destination.parent() { *state.last_folder.lock().map_err(|_| "download folder state unavailable")? = Some(parent.to_path_buf()); }
        state.pending.lock().map_err(|_| "download state unavailable")?.insert(id, Pending { owner, destination, partial, file: Some(file), bytes: 0, failed: false });
        diag("begin_blob_download:DialogStateInserted", "");
        return Ok(true);
    }
    if AUTO_SAVE_TO_DOWNLOADS {
        let destination = downloads_destination(&suggested_name)?;
        let partial = partial_path(&destination);
        let file = OpenOptions::new().create(true).read(true).write(true).truncate(true).open(&partial).map_err(|e| e.to_string())?;
        state.pending.lock().map_err(|_| "download state unavailable")?.insert(id, Pending { owner, destination, partial, file: Some(file), bytes: 0, failed: false });
        diag("begin_blob_download:AutoSaveStateInserted", "folder=Downloads");
        return Ok(true);
    }
    let last_folder = state
        .last_folder
        .lock()
        .map_err(|_| "download folder state unavailable")?
        .clone();

    let (sender, receiver) = std::sync::mpsc::sync_channel::<Result<Option<PathBuf>, String>>(1);
    let dialog_app = app.clone();
    let dialog_thread = std::thread::current();
    diag("begin_blob_download:BeforeDialog", &format!("thread_id={:?} thread_name={}", dialog_thread.id(), dialog_thread.name().unwrap_or("unnamed")));
    let dialog_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        app.run_on_main_thread(move || {
            let dialog_thread = std::thread::current();
            diag("begin_blob_download:DialogThread", &format!("thread_id={:?} thread_name={}", dialog_thread.id(), dialog_thread.name().unwrap_or("unnamed")));
            let mut dialog = dialog_app.dialog().file().set_file_name(suggested_name);
            if let Some(folder) = last_folder {
                dialog = dialog.set_directory(folder);
            }
            diag("BeforeSaveFileCall", "");
            dialog.save_file(move |path| {
                diag("DialogCallback:Entered", "");
                let raw = path.as_ref().map(|p| p.to_string());
                let _ = sender.send(Ok(path.map(|path| PathBuf::from(path.to_string()))));
                diag("begin_blob_download:DialogCallback", if raw.is_some() { "raw=Some(path)" } else { "raw=None" });
            });
            diag("AfterSaveFileCallScheduled", "");
        })
    }));
    if let Err(payload) = dialog_result {
        let message = payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("non-string panic");
        diag("PanicCaught", &format!("message={message}"));
        return Err(format!("dialog panic: {message}"));
    }
    dialog_result.unwrap()
    .map_err(|error| error.to_string())?;
    diag("begin_blob_download:DialogRequested", "");
    let raw_result = tauri::async_runtime::spawn_blocking(move || receiver.recv().ok())
        .await
        .map_err(|error| error.to_string())?;
    let raw_result = raw_result.ok_or_else(|| "dialog callback produced no result".to_string())?;
    if let Err(message) = &raw_result { diag("begin_blob_download:DialogError", message); }
    let selected = raw_result?;
    diag("begin_blob_download:DialogReturned", if selected.is_some() { "raw=Some(path)" } else { "raw=None(user-cancel-or-api-none)" });
    let Some(mut destination) = selected else {
        return Ok(false);
    };
    if destination.extension().is_none() {
        destination.set_extension("mp4");
    }
    let partial = partial_path(&destination);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&partial)
        .map_err(|error| error.to_string())?;
    diag("begin_blob_download:PartialOpened", "");

    if let Some(parent) = destination.parent() {
        *state
            .last_folder
            .lock()
            .map_err(|_| "download folder state unavailable")? = Some(parent.to_path_buf());
    }
    state
        .pending
        .lock()
        .map_err(|_| "download state unavailable")?
        .insert(
            id,
            Pending {
                owner,
                destination,
                partial,
                file: Some(file),
                bytes: 0,
                failed: false,
            },
        );
    diag("begin_blob_download:StateInserted", "");
    Ok(true)
}

#[cfg(all(windows, feature = "diag"))]
#[tauri::command]
pub fn diagnostic_save_file<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    diag("DiagnosticSaveFile:Entered", "");
    app.clone().run_on_main_thread(move || {
        diag("DiagnosticSaveFile:BeforeSaveFileCall", "");
        app.dialog().file().save_file(move |path| {
            diag("DiagnosticSaveFile:DialogCallbackEntered", if path.is_some() { "raw=Some(path)" } else { "raw=None" });
        });
        diag("DiagnosticSaveFile:AfterSaveFileCallScheduled", "");
    }).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn write_blob_download_chunk<R: Runtime>(
    webview: Webview<R>,
    state: State<'_, DownloadState>,
    id: String,
    data: String,
) -> Result<(), String> {
    diag("TauriCommand:Entered", &format!("command=write_blob_download_chunk id={id}"));
    let owner = validate_caller(&webview)?;
    diag("write_blob_download_chunk:CallerValidated", "");
    validate_id(&id)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "invalid download chunk")?;
    diag("write_blob_download_chunk:ChunkDecoded", "");
    let mut pending = state.pending.lock().map_err(|_| "download state unavailable")?;
    let item = pending.get_mut(&id).ok_or("download is no longer active")?;
    if item.owner != owner {
        return Err("download belongs to another WebView".into());
    }
    if item.failed {
        return Err("download has failed".into());
    }
    if let Err(error) = item
        .file
        .as_mut()
        .ok_or("download file is closed")?
        .write_all(&decoded)
    {
        item.failed = true;
        return Err(error.to_string());
    }
    item.bytes += decoded.len() as u64;
    diag("write_blob_download_chunk:ChunkWritten", "");
    Ok(())
}

#[tauri::command]
pub fn complete_blob_download<R: Runtime>(
    webview: Webview<R>,
    state: State<'_, DownloadState>,
    id: String,
) -> Result<(), String> {
    diag("TauriCommand:Entered", &format!("command=complete_blob_download id={id}"));
    let owner = validate_caller(&webview)?;
    diag("complete_blob_download:CallerValidated", "");
    validate_id(&id)?;
    let mut item = {
        let mut pending = state.pending.lock().map_err(|_| "download state unavailable")?;
        let item = pending.get(&id).ok_or("download is no longer active")?;
        if item.owner != owner {
            return Err("download belongs to another WebView".into());
        }
        pending.remove(&id).ok_or("download is no longer active")?
    };
    if item.failed || item.bytes == 0 {
        diag("complete_blob_download:RejectedEmpty", &format!("bytes={} failed={}", item.bytes, item.failed));
        return Err("download did not produce a valid file".into());
    }
    let file = item.file.as_mut().ok_or("download file is closed")?;
    file.flush().map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    diag("complete_blob_download:FileFlushed", "");
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|_| "download is not a valid MP4 file".to_string())?;
    diag("complete_blob_download:Header", &format!("bytes={} signature={:02x?}", item.bytes, &header[..8]));
    if &header[4..8] != b"ftyp" {
        return Err("download is not a valid MP4 file".into());
    }
    drop(item.file.take());
    diag("complete_blob_download:Mp4Validated", "");
    atomic_replace(&item.partial, &item.destination)?;
    diag("complete_blob_download:Renamed", "");
    Ok(())
}

#[tauri::command]
pub fn cancel_blob_download<R: Runtime>(
    webview: Webview<R>,
    state: State<'_, DownloadState>,
    id: String,
) -> Result<(), String> {
    diag("TauriCommand:Entered", &format!("command=cancel_blob_download id={id}"));
    let owner = validate_caller(&webview)?;
    validate_id(&id)?;
    let mut pending = state.pending.lock().map_err(|_| "download state unavailable")?;
    if let Some(item) = pending.get(&id) {
        if item.owner != owner {
            return Err("download belongs to another WebView".into());
        }
    }
    pending.remove(&id);
    diag("cancel_blob_download:StateRemoved", "");
    Ok(())
}

pub fn cancel_for_webview(state: &DownloadState, label: &str) {
    if let Ok(mut pending) = state.pending.lock() {
        pending.retain(|_, item| item.owner != label);
    }
}

pub const INIT_SCRIPT: &str = r#"(() => {
  const ENABLE_NATIVE_SAVE_AS = true;
  const AUTO_SAVE_TO_DOWNLOADS = true;
  if (window.__flowpilotBlobBridgeInstalled) return;
  window.__flowpilotBlobBridgeInstalled = true;
  const invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
  const pending = new Map();
  let lastDownloadHref = null;
  const originalClick = HTMLAnchorElement.prototype.click;

  async function cancel(id) {
    pending.delete(id);
    try { await invoke('cancel_blob_download', { id }); } catch (_) {}
  }

  async function transfer(id, blob) {
    try {
      for (let offset = 0; offset < blob.size; offset += 262144) {
        if (!pending.has(id)) return;
        const chunk = new Uint8Array(
          await blob.slice(offset, Math.min(offset + 262144, blob.size)).arrayBuffer()
        );
        let binary = '';
        for (let index = 0; index < chunk.length; index += 8192) {
          binary += String.fromCharCode(...chunk.subarray(index, index + 8192));
        }
        await invoke('write_blob_download_chunk', { id, data: btoa(binary) });
      }
      await invoke('complete_blob_download', { id });
      pending.delete(id);
    } catch (_) {
      await cancel(id);
    }
  }

  HTMLAnchorElement.prototype.click = function (...args) {
    const href = this.href || '';
    const isVideoDownload = this.download &&
      (href.startsWith('blob:') || href.startsWith('https:'));
    if (this.download) {
      const scheme = href.split(':', 1)[0] || 'other';
      lastDownloadHref = href;
    }
    if ((ENABLE_NATIVE_SAVE_AS || AUTO_SAVE_TO_DOWNLOADS) && isVideoDownload) {
      const id = `blob-${crypto.randomUUID()}`;
      const blobPromise = fetch(href, { credentials: 'include' }).then(response => {
        if (!response.ok) throw new Error('blob fetch failed');
        return response.blob();
      });
      pending.set(id, true);
      void (async () => {
        try {
          const accepted = await invoke('begin_blob_download', {
            id,
            filename: this.download || 'Flowpilot_Video.mp4'
          });
          if (!accepted) {
            pending.delete(id);
            return;
          }
          await transfer(id, await blobPromise);
        } catch (_) {
          await cancel(id);
        }
      })();
      return;
    }
    return originalClick.apply(this, args);
  };

  window.addEventListener('pagehide', () => {
    for (const id of pending.keys()) void cancel(id);
  });
})();"#;

pub const GOOGLE_FLOW_RESET_SCRIPT: &str = "void 0;";

#[cfg(test)]
mod tests {
    use super::{partial_path, safe_filename, validate_id};
    use std::path::Path;

    #[test]
    fn filename_is_sanitized_and_has_mp4_extension() {
        assert_eq!(safe_filename("video:name"), "video_name.mp4");
        assert_eq!(safe_filename("movie.MP4"), "movie.MP4");
        assert_eq!(safe_filename(""), "Flowpilot_Video.mp4");
    }

    #[test]
    fn download_id_is_strictly_validated() {
        assert!(validate_id("blob-1234-abcd").is_ok());
        assert!(validate_id("").is_err());
        assert!(validate_id("../outside").is_err());
    }

    #[test]
    fn partial_file_stays_next_to_destination() {
        assert_eq!(
            partial_path(Path::new(r"C:\Downloads\video.mp4")),
            Path::new(r"C:\Downloads\video.mp4.part")
        );
    }
}
