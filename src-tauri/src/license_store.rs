use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::Duration,
};
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};
use uuid::Uuid;

const API: &str = "https://flowpilot-license-server.frangga-snow.workers.dev";
const API_HOST: &str = "flowpilot-license-server.frangga-snow.workers.dev";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

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
    #[serde(default)]
    status: String,
    #[serde(default)]
    plan: String,
    #[serde(rename = "expiresAt")]
    expires_at: Option<String>,
    #[serde(default)]
    lifetime: bool,
    #[serde(default)]
    error: Option<String>,
}
fn path<R: Runtime>(a: &AppHandle<R>) -> Result<PathBuf, String> {
    a.path()
        .resolve("license-state.json", BaseDirectory::AppLocalData)
        .map_err(|e| e.to_string())
}
fn diagnostic<R: Runtime>(a: &AppHandle<R>, line: String) {
    eprintln!("{line}");
    let Ok(state_path) = path(a) else { return };
    let Some(directory) = state_path.parent() else { return };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let log_path = directory.join("license-network.log");
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = writeln!(file, "{line}");
    }
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
    let started = std::time::Instant::now();
    diagnostic(
        a,
        format!(
            "[license] stage=request_start host={} endpoint={}",
            API_HOST, endpoint
        ),
    );
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| "Server Unavailable: client setup failed".to_string())?;
    let r = client
        .post(format!("{API}{endpoint}"))
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            let category = if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connect_or_tls"
            } else {
                "transport"
            };
            diagnostic(
                a,
                format!(
                    "[license] stage=transport_failure host={} endpoint={} duration_ms={} category={}",
                    API_HOST,
                    endpoint,
                    started.elapsed().as_millis(),
                    category
                ),
            );
            format!("Server Unavailable: {category}")
        })?;
    let code = r.status();
    let content_type = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let cf_ray = r
        .headers()
        .get("cf-ray")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("none")
        .to_string();
    let text = r
        .text()
        .await
        .map_err(|_| "Server Unavailable: response_read_failed".to_string())?;
    diagnostic(
        a,
        format!(
            "[license] stage=response_received host={} endpoint={} duration_ms={} status={} content_type={} body_bytes={} cf_ray={}",
            API_HOST,
            endpoint,
            started.elapsed().as_millis(),
            code.as_u16(),
            content_type,
            text.len(),
            cf_ray
        ),
    );
    let d: ServerResponse = serde_json::from_str(&text).map_err(|_| {
        diagnostic(
            a,
            format!(
                "[license] stage=response_parse_failure host={} endpoint={} duration_ms={} status={} content_type={} body_bytes={}",
                API_HOST,
                endpoint,
                started.elapsed().as_millis(),
                code.as_u16(),
                content_type,
                text.len()
            ),
        );
        if code.is_success() {
            "Invalid Server Response".to_string()
        } else if code.as_u16() >= 500 {
            "Server Unavailable".to_string()
        } else {
            "Unexpected Server Response".to_string()
        }
    })?;
    if !code.is_success() {
        return Err(match d.error.as_deref() {
            Some("LICENSE_REVOKED") => "License Revoked",
            Some("LICENSE_EXPIRED") => "License Expired",
            Some("DEVICE_ALREADY_BOUND") => "Device Mismatch",
            Some("INVALID_LICENSE") => "Invalid License",
            Some("INVALID_REQUEST") => "Invalid Request",
            _ if code.as_u16() >= 500 => "Server Unavailable",
            _ => "Unexpected Server Response",
        }
        .to_string());
    }
    if d.status.is_empty() || d.plan.is_empty() {
        return Err("Invalid Server Response".to_string());
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

#[cfg(test)]
mod tests {
    use super::ServerResponse;

    #[test]
    fn parses_error_envelope_without_success_fields() {
        let response: ServerResponse =
            serde_json::from_str(r#"{"error":"INVALID_REQUEST"}"#).unwrap();
        assert_eq!(response.error.as_deref(), Some("INVALID_REQUEST"));
        assert!(response.status.is_empty());
        assert!(response.plan.is_empty());
    }

    #[test]
    fn parses_success_envelope() {
        let response: ServerResponse = serde_json::from_str(
            r#"{"status":"active","plan":"lifetime","expiresAt":null,"lifetime":true}"#,
        )
        .unwrap();
        assert_eq!(response.status, "active");
        assert_eq!(response.plan, "lifetime");
        assert!(response.lifetime);
    }
}
