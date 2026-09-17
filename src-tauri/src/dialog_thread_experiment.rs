use std::{path::PathBuf, sync::{mpsc, OnceLock}};

struct Request { folder: Option<PathBuf>, filename: String, reply: mpsc::SyncSender<Option<PathBuf>> }
static CHANNEL: OnceLock<mpsc::SyncSender<Request>> = OnceLock::new();

pub fn init() {
    CHANNEL.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        std::thread::Builder::new().name("flowpilot-dialog-experiment".into()).spawn(move || {
            #[cfg(windows)] unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED); }
            while let Ok(request) = receiver.recv() {
                let mut dialog = rfd::FileDialog::new().set_file_name(&request.filename);
                if let Some(folder) = request.folder { dialog = dialog.set_directory(folder); }
                let result = dialog.save_file();
                let _ = request.reply.send(result);
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


