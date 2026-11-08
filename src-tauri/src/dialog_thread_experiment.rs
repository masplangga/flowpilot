use std::{path::PathBuf, sync::mpsc};

use tauri::{Runtime, Window};
use tauri_plugin_dialog::DialogExt;

/// Opens a parented native Save As dialog and waits without blocking Tauri's UI thread.
pub async fn request_dialog<R: Runtime>(
    window: Window<R>,
    folder: Option<PathBuf>,
    filename: String,
) -> Result<Option<PathBuf>, String> {
    let (reply, receiver) = mpsc::sync_channel(1);
    let mut dialog = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Save downloaded file")
        .set_file_name(filename);
    if let Some(folder) = folder {
        dialog = dialog.set_directory(folder);
    }
    dialog.save_file(move |selected| {
        let path = selected.and_then(|value| value.into_path().ok());
        let _ = reply.send(path);
    });

    tauri::async_runtime::spawn_blocking(move || receiver.recv().map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
