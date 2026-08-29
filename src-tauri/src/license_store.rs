use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};
use uuid::Uuid;

const API: &str = "https://flowpilot-license-server.frangga-snow.workers.dev";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LicenseState {
    pub plan: String,
    pub status: String,
    pub expires_at: Option<String>,
    pub lifetime: bool,
    pub last_validated_at: String,
    pub device_id: String,
}
#[derive(Deserialize)]
struct ServerResponse {
    status: String,
    plan: String,
    #[serde(rename = "expiresAt")]
    expires_at: Option<String>,
    lifetime: bool,
    #[serde(default)]
    error: Option<String>,
}
fn path<R: Runtime>(a: &AppHandle<R>) -> Result<PathBuf, String> {
    a.path()
        .resolve("license-state.json", BaseDirectory::AppLocalData)
        .map_err(|e| e.to_string())
}
fn read<R: Runtime>(a: &AppHandle<R>) -> Result<Option<LicenseState>, String> {
    let p = path(a)?;
    if !p.exists() {
        return Ok(None);
    };
    serde_json::from_str(&fs::read_to_string(p).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}
fn write<R: Runtime>(a: &AppHandle<R>, s: &LicenseState) -> Result<(), String> {
    let p = path(a)?;
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?
    };
    fs::write(p, serde_json::to_vec(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn entry() -> Result<Entry, String> {
    Entry::new("Flowpilot", "license-key").map_err(|e| e.to_string())
}
fn device<R: Runtime>(a: &AppHandle<R>) -> Result<String, String> {
    if let Some(s) = read(a)? {
        if !s.device_id.is_empty() {
            return Ok(s.device_id);
        }
    };
    let id = Uuid::new_v4().to_string();
    write(
        a,
        &LicenseState {
            device_id: id.clone(),
            ..Default::default()
        },
    )?;
    Ok(id)
}
#[tauri::command]
pub fn get_device_id<R: Runtime>(a: AppHandle<R>) -> Result<String, String> {
    device(&a)
}
#[tauri::command]
pub fn get_license_state<R: Runtime>(a: AppHandle<R>) -> Result<Option<LicenseState>, String> {
    read(&a)
}
async fn call<R: Runtime>(
    a: &AppHandle<R>,
    endpoint: &str,
    key: &str,
) -> Result<ServerResponse, String> {
    let body = serde_json::json!({"licenseKey":key,"deviceId":device(a)?});
    let r = reqwest::Client::new()
        .post(format!("{API}{endpoint}"))
        .json(&body)
        .send()
        .await
        .map_err(|_| "Server Unavailable".to_string())?;
    let code = r.status();
    let d: ServerResponse = r
        .json()
        .await
        .map_err(|_| "Server Unavailable".to_string())?;
    if !code.is_success() {
        return Err(match d.error.as_deref() {
            Some("LICENSE_REVOKED") => "License Revoked",
            Some("LICENSE_EXPIRED") => "License Expired",
            Some("DEVICE_ALREADY_BOUND") => "Device Mismatch",
            Some("INVALID_LICENSE") => "Invalid License",
            _ if code.as_u16() >= 500 => "Server Unavailable",
            _ => "Unexpected Server Response",
        }
        .to_string());
    }
    Ok(d)
}
fn state<R: Runtime>(a: &AppHandle<R>, d: ServerResponse) -> Result<LicenseState, String> {
    let s = LicenseState {
        plan: d.plan,
        status: d.status,
        expires_at: d.expires_at,
        lifetime: d.lifetime,
        last_validated_at: chrono::Utc::now().to_rfc3339(),
        device_id: device(a)?,
    };
    write(a, &s)?;
    Ok(s)
}
#[tauri::command]
pub async fn activate_license<R: Runtime>(
    a: AppHandle<R>,
    license_key: String,
) -> Result<LicenseState, String> {
    let key = license_key.trim();
    if key.is_empty() {
        return Err("Invalid License".into());
    };
    let d = call(&a, "/license/activate", key).await?;
    entry()?.set_password(key).map_err(|e| e.to_string())?;
    state(&a, d)
}
#[tauri::command]
pub async fn validate_license<R: Runtime>(a: AppHandle<R>) -> Result<LicenseState, String> {
    let key = entry()?
        .get_password()
        .map_err(|_| "Invalid License".to_string())?;
    state(&a, call(&a, "/license/validate", &key).await?)
}
#[tauri::command]
pub fn clear_license_state<R: Runtime>(a: AppHandle<R>) -> Result<(), String> {
    let _ = entry()?.delete_credential();
    let p = path(&a)?;
    if p.exists() {
        fs::remove_file(p).map_err(|e| e.to_string())?
    };
    Ok(())
}
