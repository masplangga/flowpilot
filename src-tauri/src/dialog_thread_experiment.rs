use std::{fs::OpenOptions, io::Write, path::PathBuf, sync::{mpsc, OnceLock}, time::{SystemTime, UNIX_EPOCH}};

struct Request { folder: Option<PathBuf>, filename: String, reply: mpsc::SyncSender<Option<PathBuf>> }
static CHANNEL: OnceLock<mpsc::SyncSender<Request>> = OnceLock::new();

pub fn init() {
    CHANNEL.get_or_init(|| {
        if let (Ok(dir), Ok(ms)) = (std::env::current_dir(), SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis())) {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(dir.join("flowpilot_diag.log")) {
                let _ = writeln!(file, "{ms} event=DialogThread:Started");
                let _ = file.flush();
            }
        }
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        std::thread::Builder::new().name("flowpilot-dialog-experiment".into()).spawn(move || {
            #[cfg(windows)] unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED); }
            while let Ok(request) = receiver.recv() {
                crate::webview_download_bridge::diag("DialogExperiment:RequestReceived", "");
                let mut dialog = rfd::FileDialog::new().set_file_name(&request.filename);
                if let Some(folder) = request.folder { dialog = dialog.set_directory(folder); }
                let result = dialog.save_file();
                let present = result.is_some();
                let _ = request.reply.send(result);
                crate::webview_download_bridge::diag("DialogExperiment:ReplySent", if present { "raw=Some(path)" } else { "raw=None" });
            }
        }).expect("dialog experiment thread");
        sender
    });
}

pub async fn request_dialog(folder: Option<PathBuf>, filename: String) -> Result<Option<PathBuf>, String> {
    init();
    let (reply, receiver) = mpsc::sync_channel(1);
    CHANNEL.get().unwrap().send(Request { folder, filename, reply }).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv().map_err(|e| e.to_string())).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn debug_trigger_isolated_dialog(filename: Option<String>) -> Result<Option<String>, String> {
    init();
    let (reply, receiver) = mpsc::sync_channel(1);
    CHANNEL.get().unwrap().send(Request { folder: None, filename: filename.unwrap_or_else(|| "Flowpilot_Test.mp4".into()), reply }).map_err(|e| e.to_string())?;
    receiver.recv().map_err(|e| e.to_string()).map(|p| p.map(|v| v.to_string_lossy().into_owned()))
}
