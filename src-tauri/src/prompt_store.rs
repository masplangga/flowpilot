use std::{fs, path::PathBuf};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub category: String,

    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub order: usize,
    #[serde(default)]
    pub pinned: bool,
    #[serde(rename = "cardColor", default)]
    pub card_color: String,

}

fn path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path().resolve("prompts.json", BaseDirectory::AppLocalData).map_err(|e| e.to_string())
}


fn valid(item: &Prompt) -> bool {
    !item.id.is_empty() && Uuid::parse_str(&item.id).is_ok()
        && !item.title.trim().is_empty() && item.title.chars().count() <= 200
        && !item.prompt.trim().is_empty() && item.prompt.chars().count() <= 100_000
        && item.category.chars().count() <= 100
        && chrono::DateTime::parse_from_rfc3339(&item.created_at).is_ok()
        && chrono::DateTime::parse_from_rfc3339(&item.updated_at).is_ok()
}

fn read<R: Runtime>(app: &AppHandle<R>) -> Vec<Prompt> {
    let Ok(file) = path(app) else { return Vec::new() };
    let Ok(raw) = fs::read_to_string(file) else { return Vec::new() };
    serde_json::from_str::<Vec<Prompt>>(&raw)
        .map(|items| items.into_iter().filter(valid).collect())
        .unwrap_or_default()
}

fn write<R: Runtime>(app: &AppHandle<R>, items: &[Prompt]) -> Result<(), String> {
    let file = path(app)?;
    if let Some(parent) = file.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    let temporary = file.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(items).map_err(|e| e.to_string())?;
    fs::write(&temporary, data).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let source = HSTRING::from(temporary.as_os_str());
        let target = HSTRING::from(file.as_os_str());
        let result = unsafe { MoveFileExW(&source, &target, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
        if result.is_err() { let _ = fs::remove_file(&temporary); return Err(result.err().unwrap().to_string()); }
        return Ok(());
    }
    #[cfg(not(windows))]
    if let Err(error) = fs::rename(&temporary, &file) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    #[cfg(not(windows))]
    { Ok(()) }
}

fn text(value: String, field: &str, max: usize) -> Result<String, String> {
    let value = value.trim().to_string();
    if value.is_empty() || value.chars().count() > max { return Err(format!("invalid {field}")); }
    Ok(value)
}

#[tauri::command]
pub fn list_prompts<R: Runtime>(app: AppHandle<R>) -> Vec<Prompt> { read(&app) }

#[tauri::command]
pub fn create_prompt<R: Runtime>(app: AppHandle<R>, title: String, prompt: String, category: String) -> Result<Prompt, String> {
    let now = Utc::now().to_rfc3339();
    let mut items = read(&app); let order = items.len();
    let item = Prompt { id: Uuid::new_v4().to_string(), title: text(title, "title", 200)?, prompt: text(prompt, "prompt", 100_000)?, category: text(category, "category", 100).unwrap_or_default(), created_at: now.clone(), updated_at: now, order, pinned: false, card_color: "default".into() };
    items.push(item.clone()); write(&app, &items)?; Ok(item)
}

#[tauri::command]
pub fn update_prompt<R: Runtime>(app: AppHandle<R>, id: String, title: String, prompt: String, category: String) -> Result<Prompt, String> {
    Uuid::parse_str(&id).map_err(|_| "invalid prompt id".to_string())?;
    let mut items = read(&app);
    let item = items.iter_mut().find(|item| item.id == id).ok_or("prompt not found")?;
    item.title = text(title, "title", 200)?; item.prompt = text(prompt, "prompt", 100_000)?; item.category = text(category, "category", 100).unwrap_or_default(); item.updated_at = Utc::now().to_rfc3339();
    let result = item.clone(); write(&app, &items)?; Ok(result)
}

#[tauri::command]
pub fn delete_prompt<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    let mut items = read(&app); let before = items.len(); items.retain(|item| item.id != id); if items.len() == before { return Err("prompt not found".into()); } write(&app, &items)?; Ok(())
}


#[tauri::command]
pub fn toggle_prompt_pinned<R: Runtime>(app: AppHandle<R>, id: String) -> Result<Vec<Prompt>, String> {
    let mut items = read(&app);
    let item = items.iter_mut().find(|item| item.id == id).ok_or("prompt not found")?;
    item.pinned = !item.pinned;
    item.updated_at = Utc::now().to_rfc3339();
    write(&app, &items)?;
    Ok(items)
}

#[tauri::command]
pub fn set_prompt_card_color<R: Runtime>(app: AppHandle<R>, id: String, card_color: String) -> Result<Prompt, String> {
    let allowed = ["default", "blue", "purple", "green", "orange", "red"];
    if !allowed.contains(&card_color.as_str()) { return Err("invalid card color".into()); }
    let mut items = read(&app);
    let item = items.iter_mut().find(|item| item.id == id).ok_or("prompt not found")?;
    item.card_color = card_color;
    item.updated_at = Utc::now().to_rfc3339();
    let result = item.clone();
    write(&app, &items)?;
    Ok(result)
}


