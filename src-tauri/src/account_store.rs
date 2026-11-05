use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};

fn path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .resolve("accounts.json", BaseDirectory::AppLocalData)
        .map_err(|e| e.to_string())
}

#[derive(Debug)]
struct AccountSnapshot {
    bytes: Vec<u8>,
    value: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveStep {
    BackupWrite,
    BackupReplace,
    TemporaryWrite,
    MainReplace,
}

fn valid_account(value: &serde_json::Value) -> bool {
    let Some(account) = value.as_object() else {
        return false;
    };
    let provider_valid = match account.get("provider") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(provider)) => {
            provider.is_empty()
                || matches!(
                    provider.as_str(),
                    "google-flow" | "google_flow" | "dola" | "migoo" | "gemini" | "suno"
                )
        }
        _ => false,
    };
    provider_valid
        && account.get("id").and_then(serde_json::Value::as_str).is_some()
        && account.get("name").and_then(serde_json::Value::as_str).is_some()
        && account
            .get("email")
            .is_some_and(|value| value.is_null() || value.is_string())
        && account
            .get("avatarUrl")
            .is_some_and(|value| value.is_null() || value.is_string())
        && account.get("avatar").and_then(serde_json::Value::as_str).is_some()
        && account.get("favorite").and_then(serde_json::Value::as_bool).is_some()
        && account.get("order").and_then(serde_json::Value::as_number).is_some()
}

fn validate_accounts(value: &serde_json::Value) -> Result<(), String> {
    let Some(accounts) = value.as_array() else {
        return Err("Saved accounts must be a JSON array".to_string());
    };
    if !accounts.iter().all(valid_account) {
        return Err("Saved accounts contain an invalid account record".to_string());
    }
    Ok(())
}

fn read_snapshot(file: &Path) -> Result<Option<AccountSnapshot>, String> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Unable to read saved accounts: {error}")),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Saved accounts file contains invalid JSON: {error}"))?;
    validate_accounts(&value)
        .map_err(|error| format!("Saved accounts file has invalid structure: {error}"))?;
    Ok(Some(AccountSnapshot { bytes, value }))
}

fn read_accounts_file(file: &Path) -> Result<Option<serde_json::Value>, String> {
    Ok(read_snapshot(file)?.map(|snapshot| snapshot.value))
}

fn replace_file(source: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source = HSTRING::from(source.as_os_str());
        let target = HSTRING::from(target.as_os_str());
        unsafe {
            MoveFileExW(
                &source,
                &target,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, target).map_err(|error| error.to_string())
    }
}

fn write_synced(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut output = fs::File::create(file).map_err(|error| error.to_string())?;
    output
        .write_all(bytes)
        .and_then(|()| output.sync_all())
        .map_err(|error| error.to_string())
}

fn write_accounts_file(file: &Path, accounts: &serde_json::Value) -> Result<(), String> {
    write_accounts_file_with_hook(file, accounts, |_| Ok(()))
}

fn write_accounts_file_with_hook<F>(
    file: &Path,
    accounts: &serde_json::Value,
    mut before_step: F,
) -> Result<(), String>
where
    F: FnMut(SaveStep) -> Result<(), String>,
{
    validate_accounts(accounts)?;
    let data = serde_json::to_vec(accounts).map_err(|error| error.to_string())?;
    let parent = file
        .parent()
        .ok_or_else(|| "Invalid saved accounts path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;

    let temporary = file.with_extension("json.tmp");
    let backup = file.with_extension("json.bak");
    let backup_temporary = file.with_extension("json.bak.tmp");
    let result = (|| {
        // Read and validate the original before touching either it or its backup.
        if let Some(original) = read_snapshot(file)? {
            before_step(SaveStep::BackupWrite)?;
            let backup_result = (|| {
                write_synced(&backup_temporary, &original.bytes)?;
                let copied = read_snapshot(&backup_temporary)?
                    .ok_or_else(|| "Temporary accounts backup disappeared".to_string())?;
                if copied.bytes != original.bytes || copied.value != original.value {
                    return Err("Temporary accounts backup did not match the original".to_string());
                }
                before_step(SaveStep::BackupReplace)?;
                replace_file(&backup_temporary, &backup)
            })();
            if backup_result.is_err() {
                let _ = fs::remove_file(&backup_temporary);
            }
            backup_result?;
        }

        before_step(SaveStep::TemporaryWrite)?;
        let temporary_result = (|| {
            write_synced(&temporary, &data)?;
            let written = read_snapshot(&temporary)?
                .ok_or_else(|| "Temporary accounts file disappeared".to_string())?;
            if written.bytes != data || written.value != *accounts {
                return Err("Temporary accounts file did not match the requested data".to_string());
            }
            before_step(SaveStep::MainReplace)?;
            replace_file(&temporary, file)
        })();
        if temporary_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        temporary_result
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&backup_temporary);
    }
    result
}

#[tauri::command]
pub fn load_accounts<R: Runtime>(app: AppHandle<R>) -> Result<Option<serde_json::Value>, String> {
    let file = path(&app)?;
    read_accounts_file(&file)
}

#[tauri::command]
pub fn save_accounts<R: Runtime>(
    app: AppHandle<R>,
    accounts: serde_json::Value,
) -> Result<(), String> {
    let file = path(&app)?;
    write_accounts_file(&file, &accounts)
}

#[cfg(test)]
mod tests {
    use super::{read_accounts_file, read_snapshot, write_accounts_file, write_accounts_file_with_hook, SaveStep};
    use serde_json::{json, Value};
    use std::{fs, path::PathBuf};
    use uuid::Uuid;

    fn test_file() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("flowpilot-account-store-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        directory.join("accounts.json")
    }

    fn account(id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "provider": "google-flow",
            "email": null,
            "avatarUrl": null,
            "avatar": "AC",
            "favorite": false,
            "order": 0
        })
    }

    fn clean(file: &PathBuf) {
        if let Some(parent) = file.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn install(file: &PathBuf, value: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(value).unwrap();
        fs::write(file, &bytes).unwrap();
        bytes
    }

    fn assert_bytes(file: &PathBuf, expected: &[u8]) {
        assert_eq!(fs::read(file).unwrap(), expected);
    }

    #[test]
    fn missing_accounts_file_is_a_new_empty_store() {
        let file = test_file();
        assert_eq!(read_accounts_file(&file).unwrap(), None);
        clean(&file);
    }

    #[test]
    fn valid_accounts_file_preserves_all_accounts() {
        let file = test_file();
        let accounts = json!([account("a", "A"), account("b", "B")]);
        install(&file, &accounts);
        assert_eq!(read_accounts_file(&file).unwrap(), Some(accounts));
        clean(&file);
    }

    #[test]
    fn empty_corrupt_or_wrong_shape_files_are_errors() {
        let file = test_file();
        fs::write(&file, b"").unwrap();
        assert!(read_accounts_file(&file).is_err());
        fs::write(&file, b"[{\"id\":").unwrap();
        assert!(read_accounts_file(&file).is_err());
        fs::write(&file, b"null").unwrap();
        assert!(read_accounts_file(&file).is_err());
        clean(&file);
    }

    #[test]
    fn unreadable_accounts_path_is_an_error_not_missing() {
        let file = test_file();
        fs::create_dir(&file).unwrap();
        assert!(read_accounts_file(&file).unwrap_err().contains("Unable to read"));
        clean(&file);
    }

    #[test]
    fn save_rejects_invalid_new_data_before_touching_existing_files() {
        let file = test_file();
        let original = json!([account("old", "Old")]);
        let backup = json!([account("backup", "Backup")]);
        let original_bytes = install(&file, &original);
        let backup_path = file.with_extension("json.bak");
        let backup_bytes = install(&backup_path, &backup);
        assert!(write_accounts_file(&file, &json!([{"id":"bad"}])).is_err());
        assert_bytes(&file, &original_bytes);
        assert_bytes(&backup_path, &backup_bytes);
        clean(&file);
    }

    #[test]
    fn backup_creation_failure_keeps_main_and_previous_backup_intact() {
        let file = test_file();
        let original_bytes = install(&file, &json!([account("main", "Main")]));
        let backup = file.with_extension("json.bak");
        let backup_bytes = install(&backup, &json!([account("backup", "Backup")]));
        let backup_temporary = file.with_extension("json.bak.tmp");
        fs::create_dir(&backup_temporary).unwrap();

        assert!(write_accounts_file(&file, &json!([account("new", "New")])).is_err());
        assert_bytes(&file, &original_bytes);
        assert_bytes(&backup, &backup_bytes);
        clean(&file);
    }

    #[test]
    fn interruption_before_backup_replacement_keeps_previous_backup_intact() {
        let file = test_file();
        let original = json!([account("main", "Main")]);
        let original_bytes = install(&file, &original);
        let backup = file.with_extension("json.bak");
        let previous_backup = json!([account("previous", "Previous")]);
        let previous_backup_bytes = install(&backup, &previous_backup);

        let result = write_accounts_file_with_hook(&file, &json!([account("new", "New")]), |step| {
            if step == SaveStep::BackupReplace { Err("simulated process stop".into()) } else { Ok(()) }
        });
        assert!(result.is_err());
        assert_bytes(&file, &original_bytes);
        assert_bytes(&backup, &previous_backup_bytes);
        clean(&file);
    }

    #[test]
    fn temporary_write_failure_keeps_main_intact_and_backup_valid() {
        let file = test_file();
        let original = json!([account("main", "Main")]);
        let original_bytes = install(&file, &original);
        let backup = file.with_extension("json.bak");
        install(&backup, &json!([account("previous", "Previous")]));
        fs::create_dir(file.with_extension("json.tmp")).unwrap();

        assert!(write_accounts_file(&file, &json!([account("new", "New")])).is_err());
        assert_bytes(&file, &original_bytes);
        assert_eq!(read_snapshot(&backup).unwrap().unwrap().value, original);
        clean(&file);
    }

    #[test]
    fn main_replacement_failure_keeps_original_and_valid_backup() {
        let file = test_file();
        let original = json!([account("main", "Main")]);
        let original_bytes = install(&file, &original);
        let backup = file.with_extension("json.bak");
        install(&backup, &json!([account("previous", "Previous")]));

        let result = write_accounts_file_with_hook(&file, &json!([account("new", "New")]), |step| {
            if step == SaveStep::MainReplace { Err("simulated replacement failure".into()) } else { Ok(()) }
        });
        assert!(result.is_err());
        assert_bytes(&file, &original_bytes);
        assert_eq!(read_snapshot(&backup).unwrap().unwrap().value, original);
        clean(&file);
    }

    #[test]
    fn interruption_before_temp_and_before_replace_leaves_main_recoverable() {
        let original = json!([account("main", "Main")]);
        let proposed = json!([account("new", "New")]);
        for fail_at in [SaveStep::TemporaryWrite, SaveStep::MainReplace] {
            let file = test_file();
            let original_bytes = install(&file, &original);
            let result = write_accounts_file_with_hook(&file, &proposed, |step| {
                if step == fail_at { Err("simulated process stop".into()) } else { Ok(()) }
            });
            assert!(result.is_err());
            assert_bytes(&file, &original_bytes);
            assert_eq!(read_snapshot(&file.with_extension("json.bak")).unwrap().unwrap().value, original);
            clean(&file);
        }
    }

    #[test]
    fn leftover_partial_temporary_files_after_process_stop_are_ignored() {
        let file = test_file();
        let original = json!([account("main", "Main")]);
        let original_bytes = install(&file, &original);
        let backup = file.with_extension("json.bak");
        let backup_bytes = install(&backup, &original);
        fs::write(file.with_extension("json.tmp"), b"[{partial new data").unwrap();
        fs::write(file.with_extension("json.bak.tmp"), b"[{partial backup data").unwrap();

        assert_eq!(read_accounts_file(&file).unwrap(), Some(original));
        assert_bytes(&file, &original_bytes);
        assert_bytes(&backup, &backup_bytes);
        clean(&file);
    }

    #[test]
    fn corrupt_main_never_replaces_a_valid_backup() {
        let file = test_file();
        let corrupt = b"[{broken";
        fs::write(&file, corrupt).unwrap();
        let backup = file.with_extension("json.bak");
        let backup_value = json!([account("safe", "Safe")]);
        let backup_bytes = install(&backup, &backup_value);

        assert!(write_accounts_file(&file, &json!([account("new", "New")])).is_err());
        assert_bytes(&file, corrupt);
        assert_bytes(&backup, &backup_bytes);
        clean(&file);
    }

    #[test]
    fn main_and_backup_both_valid_keep_format_and_previous_data() {
        let file = test_file();
        let main_value = json!([account("main", "Main")]);
        let old_backup_value = json!([account("older", "Older")]);
        let main_bytes = install(&file, &main_value);
        let backup = file.with_extension("json.bak");
        install(&backup, &old_backup_value);

        let next = json!([account("next", "Next")]);
        write_accounts_file(&file, &next).unwrap();
        assert_eq!(read_accounts_file(&file).unwrap(), Some(next));
        assert_bytes(&backup, &main_bytes);
        clean(&file);
    }
}
