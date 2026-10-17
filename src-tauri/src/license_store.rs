use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};
use uuid::Uuid;

const API: &str = "https://flowpilot-license-api.pages.dev";
const API_HOST: &str = "flowpilot-license-api.pages.dev";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
static ACTIVATION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct ActivationGuard<'a>(&'a AtomicBool);

impl<'a> ActivationGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> Result<Self, String> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(flag))
            .map_err(|_| "Activation already in progress".to_string())
    }
}

impl Drop for ActivationGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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

fn safe_error_code(code: &str) -> Option<&str> {
    (!code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(code)
}

fn parse_response(status: reqwest::StatusCode, body: &str) -> Result<ServerResponse, String> {
    let response: ServerResponse = serde_json::from_str(body).map_err(|_| {
        if status.is_success() {
            "Invalid Server Response".to_string()
        } else {
            format!(
                "License server returned HTTP {} with invalid JSON",
                status.as_u16()
            )
        }
    })?;
    if !status.is_success() {
        let reason = match response.error.as_deref() {
            Some("LICENSE_REVOKED") => "License Revoked".to_string(),
            Some("LICENSE_EXPIRED") => "License Expired".to_string(),
            Some("DEVICE_ALREADY_BOUND") => "Device Mismatch".to_string(),
            Some("INVALID_LICENSE") => "Invalid License".to_string(),
            Some("INVALID_REQUEST") => "Invalid Request".to_string(),
            Some(code) => match safe_error_code(code) {
                Some(code) => format!("License server error {code}"),
                None => "License server rejected request".to_string(),
            },
            None if status.is_server_error() => "Server Unavailable".to_string(),
            None => "License server rejected request".to_string(),
        };
        return Err(format!("{reason} (HTTP {})", status.as_u16()));
    }
    if response.status.is_empty() || response.plan.is_empty() {
        return Err("Invalid Server Response".to_string());
    }
    Ok(response)
}

fn transport_message(endpoint: &str, category: &str) -> String {
    if endpoint == "/license/activate" && category == "timeout" {
        "Activation timed out. The server may have processed it; contact support with your license-network.log before trying again.".to_string()
    } else {
        format!("Server Unavailable: {category}")
    }
}
fn path<R: Runtime>(a: &AppHandle<R>) -> Result<PathBuf, String> {
    a.path()
        .resolve("license-state.json", BaseDirectory::AppLocalData)
        .map_err(|e| e.to_string())
}
fn diagnostic<R: Runtime>(a: &AppHandle<R>, line: String) {
    eprintln!("{line}");
    let Ok(state_path) = path(a) else { return };
    let Some(directory) = state_path.parent() else {
        return;
    };
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
            transport_message(endpoint, category)
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
    let text = r.text().await.map_err(|error| {
        let category = if error.is_timeout() { "timeout" } else { "response_read_failed" };
        diagnostic(
            a,
            format!(
                "[license] stage=response_read_failure host={} endpoint={} duration_ms={} status={} cf_ray={} category={}",
                API_HOST, endpoint, started.elapsed().as_millis(), code.as_u16(), cf_ray, category
            ),
        );
        if category == "timeout" {
            transport_message(endpoint, category)
        } else {
            format!("License server response read failed (HTTP {})", code.as_u16())
        }
    })?;
    let error_code = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("error")?
                .as_str()
                .and_then(safe_error_code)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "none".to_string());
    diagnostic(
        a,
        format!(
            "[license] stage=response_received host={} endpoint={} duration_ms={} status={} content_type={} body_bytes={} cf_ray={} error_code={}",
            API_HOST,
            endpoint,
            started.elapsed().as_millis(),
            code.as_u16(),
            content_type,
            text.len(),
            cf_ray,
            error_code
        ),
    );
    let parsed = parse_response(code, &text);
    if serde_json::from_str::<ServerResponse>(&text).is_err() {
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
    }
    parsed
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
    let _guard = ActivationGuard::acquire(&ACTIVATION_IN_PROGRESS)?;
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
    use super::{parse_response, transport_message, ActivationGuard, ServerResponse};
    use reqwest::StatusCode;
    use std::sync::atomic::AtomicBool;

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

    #[test]
    fn maps_http_error_fixtures_without_guessing_status_meaning() {
        let fixtures = [
            (
                409,
                r#"{"error":"DEVICE_ALREADY_BOUND"}"#,
                "Device Mismatch (HTTP 409)",
            ),
            (
                409,
                r#"{"error":"CONFLICT"}"#,
                "License server error CONFLICT (HTTP 409)",
            ),
            (
                404,
                r#"{"error":"ROUTE_NOT_FOUND"}"#,
                "License server error ROUTE_NOT_FOUND (HTTP 404)",
            ),
            (404, r#"{}"#, "License server rejected request (HTTP 404)"),
            (500, r#"{}"#, "Server Unavailable (HTTP 500)"),
            (
                502,
                "<html>bad gateway</html>",
                "License server returned HTTP 502 with invalid JSON",
            ),
            (
                400,
                r#"{"error":"<unsafe>"}"#,
                "License server rejected request (HTTP 400)",
            ),
        ];
        for (status, body, expected) in fixtures {
            assert_eq!(
                parse_response(StatusCode::from_u16(status).unwrap(), body)
                    .err()
                    .as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn validates_success_response_and_invalid_json() {
        assert!(parse_response(StatusCode::OK, r#"{"status":"active","plan":"pro"}"#).is_ok());
        assert_eq!(
            parse_response(StatusCode::OK, "not json").err().as_deref(),
            Some("Invalid Server Response")
        );
        assert_eq!(
            parse_response(StatusCode::OK, r#"{}"#).err().as_deref(),
            Some("Invalid Server Response")
        );
    }

    #[test]
    fn activation_timeout_warns_about_uncertain_server_result() {
        assert!(transport_message("/license/activate", "timeout").contains("may have processed"));
        assert_eq!(
            transport_message("/license/validate", "timeout"),
            "Server Unavailable: timeout"
        );
    }

    #[test]
    fn activation_guard_rejects_overlap_and_releases_on_drop() {
        let flag = AtomicBool::new(false);
        let first = ActivationGuard::acquire(&flag).unwrap();
        assert!(ActivationGuard::acquire(&flag).is_err());
        drop(first);
        assert!(ActivationGuard::acquire(&flag).is_ok());
    }
}
