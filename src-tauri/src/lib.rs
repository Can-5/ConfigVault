mod vault;

use std::sync::Mutex;
use tauri::{Manager, State};
use vault::{BackupMeta, Manifest, RestoreReport};

pub struct VaultState {
    pub dir: std::path::PathBuf,
    /// Serialize create/restore/import/delete to avoid interleaved writes.
    pub lock: Mutex<()>,
}

#[tauri::command]
fn list_backups(state: State<'_, VaultState>) -> Vec<BackupMeta> {
    vault::list_backups(&state.dir)
}

#[tauri::command]
fn backup_details(state: State<'_, VaultState>, id: String) -> Result<Manifest, String> {
    vault::backup_entry_count(&state.dir, &id).ok_or_else(|| "backup not found".to_string())
}

#[tauri::command]
fn create_backup(state: State<'_, VaultState>, name: String, sources: Vec<String>, excludes: Vec<String>) -> Result<BackupMeta, String> {
    let _guard = state.lock.lock().map_err(|e| e.to_string())?;
    let clean_excludes: Vec<String> = excludes.into_iter().map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect();
    vault::create_backup(&state.dir, &name, &sources, &clean_excludes)
}

#[tauri::command]
fn delete_backup(state: State<'_, VaultState>, id: String) -> Result<(), String> {
    let _guard = state.lock.lock().map_err(|e| e.to_string())?;
    vault::delete_backup(&state.dir, &id)
}

#[tauri::command]
fn restore_preview(state: State<'_, VaultState>, id: String) -> Result<RestoreReport, String> {
    vault::restore_backup(&state.dir, &id, true)
}

#[tauri::command]
fn restore_backup(state: State<'_, VaultState>, id: String) -> Result<RestoreReport, String> {
    let _guard = state.lock.lock().map_err(|e| e.to_string())?;
    vault::restore_backup(&state.dir, &id, false)
}

#[tauri::command]
fn export_backup(state: State<'_, VaultState>, id: String, dest: String) -> Result<(), String> {
    let _guard = state.lock.lock().map_err(|e| e.to_string())?;
    vault::export_backup(&state.dir, &id, &dest)
}

#[tauri::command]
fn import_backup(state: State<'_, VaultState>, src: String) -> Result<BackupMeta, String> {
    let _guard = state.lock.lock().map_err(|e| e.to_string())?;
    vault::import_backup(&state.dir, &src)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(dir.join("backups")).map_err(|e| e.to_string())?;
            app.manage(VaultState { dir, lock: Mutex::new(()) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_backups,
            backup_details,
            create_backup,
            delete_backup,
            restore_preview,
            restore_backup,
            export_backup,
            import_backup
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
