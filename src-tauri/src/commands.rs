use crate::crypto;
use crate::crypto::MasterPasswordState;
use crate::session_store::{
    CreatePortForwardInput, CreateSessionInput, Group, PortForward, Session, SessionStore,
    UpdatePortForwardInput, UpdateSessionInput,
};
use crate::ssh_config;
use font_kit::source::SystemSource;
use serde::Serialize;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::process::Command;
use tauri_plugin_opener::OpenerExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppBootstrapState {
    pub font_family: Option<String>,
    pub font_size: Option<String>,
    pub color_scheme: Option<String>,
    pub code_editor_command: Option<String>,
    pub scrollback: Option<String>,
    pub has_master_password: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub fn list_sessions(store: tauri::State<'_, SessionStore>) -> Result<Vec<Session>, String> {
    store.list_sessions()
}

#[tauri::command]
pub fn get_session(store: tauri::State<'_, SessionStore>, id: i64) -> Result<Session, String> {
    store.get_session(id)
}

#[tauri::command]
pub fn create_session(
    store: tauri::State<'_, SessionStore>,
    input: CreateSessionInput,
) -> Result<Session, String> {
    store.create_session(input)
}

#[tauri::command]
pub fn update_session(
    store: tauri::State<'_, SessionStore>,
    id: i64,
    input: UpdateSessionInput,
) -> Result<Session, String> {
    store.update_session(id, input)
}

#[tauri::command]
pub fn delete_session(store: tauri::State<'_, SessionStore>, id: i64) -> Result<(), String> {
    store.delete_session(id)
}

#[tauri::command]
pub fn get_master_password_status(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
) -> Result<serde_json::Value, String> {
    let has = crypto::has_master_password(&store)?;
    Ok(serde_json::json!({
        "hasMasterPassword": has,
        "unlocked": state.is_unlocked(),
    }))
}

#[tauri::command]
pub fn get_app_bootstrap_state(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
) -> Result<AppBootstrapState, String> {
    Ok(AppBootstrapState {
        font_family: store.get_setting("font_family")?,
        font_size: store.get_setting("font_size")?,
        color_scheme: store.get_setting("color_scheme")?,
        code_editor_command: store.get_setting("code_editor_command")?,
        scrollback: store.get_setting("scrollback")?,
        has_master_password: crypto::has_master_password(&store)?,
        unlocked: state.is_unlocked(),
    })
}

#[tauri::command]
pub fn set_master_password(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    password: String,
) -> Result<(), String> {
    crypto::set_master_password(&store, &state, &password)
}

#[tauri::command]
pub fn unlock_master_password(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    password: String,
) -> Result<(), String> {
    crypto::unlock_master_password(&store, &state, &password)
}

#[tauri::command]
pub fn lock_master_password(state: tauri::State<'_, MasterPasswordState>) -> Result<(), String> {
    state.lock()
}

#[tauri::command]
pub fn change_master_password(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    old_password: String,
    new_password: String,
) -> Result<(), String> {
    crypto::change_master_password(&store, &state, &old_password, &new_password)
}

#[tauri::command]
pub fn store_encrypted_password(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    session_id: i64,
    password: String,
) -> Result<(), String> {
    crypto::encrypt_and_store_password(&store, &state, session_id, &password)
}

#[tauri::command]
pub fn get_encrypted_password(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    session_id: i64,
) -> Result<Option<String>, String> {
    if !state.is_unlocked() {
        return Ok(None);
    }
    match crypto::get_encrypted_password(&store, &state, session_id) {
        Ok(pwd) => Ok(Some(pwd)),
        Err(_) => Ok(None),
    }
}

#[tauri::command]
pub fn delete_encrypted_password(
    store: tauri::State<'_, SessionStore>,
    session_id: i64,
) -> Result<(), String> {
    crypto::delete_encrypted_password(&store, session_id)
}

#[tauri::command]
pub fn get_setting(
    store: tauri::State<'_, SessionStore>,
    key: String,
) -> Result<Option<String>, String> {
    store.get_setting(&key)
}

#[tauri::command]
pub fn set_setting(
    store: tauri::State<'_, SessionStore>,
    key: String,
    value: String,
) -> Result<(), String> {
    store.set_setting(&key, &value)
}

#[tauri::command]
pub fn read_clipboard() -> Result<String, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    Ok(clipboard.get_text().unwrap_or_default())
}

#[tauri::command]
pub fn list_monospace_fonts() -> Result<Vec<String>, String> {
    let source = SystemSource::new();
    let mut fonts = source
        .all_families()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|family_name| {
            let Ok(family) = source.select_family_by_name(family_name) else {
                return false;
            };
            let Some(handle) = family.fonts().first() else {
                return false;
            };
            match handle.load() {
                Ok(font) => font.is_monospace(),
                Err(_) => false,
            }
        })
        .collect::<Vec<_>>();

    fonts.sort_unstable_by_key(|family_name| family_name.to_lowercase());
    Ok(fonts)
}

#[tauri::command]
pub fn import_ssh_config(store: tauri::State<'_, SessionStore>) -> Result<usize, String> {
    let ssh_config_path = dirs::home_dir()
        .ok_or_else(|| "Could not find home directory".to_string())?
        .join(".ssh")
        .join("config");

    if !ssh_config_path.exists() {
        return Ok(0);
    }

    let entries = ssh_config::parse_ssh_config(&ssh_config_path)?;
    store.import_from_ssh_config(entries)
}

#[tauri::command]
pub fn list_ssh_config_entries() -> Result<Vec<serde_json::Value>, String> {
    let ssh_config_path = dirs::home_dir()
        .ok_or_else(|| "Could not find home directory".to_string())?
        .join(".ssh")
        .join("config");

    if !ssh_config_path.exists() {
        return Ok(Vec::new());
    }

    let entries = ssh_config::parse_ssh_config(&ssh_config_path)?;
    Ok(entries
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "host": e.host,
                "hostName": e.host_name,
                "port": e.port,
                "user": e.user,
                "identityFile": e.identity_file,
            })
        })
        .collect())
}

#[tauri::command]
pub fn list_port_forwards(
    store: tauri::State<'_, SessionStore>,
    session_id: i64,
) -> Result<Vec<PortForward>, String> {
    store.list_port_forwards(session_id)
}

#[tauri::command]
pub fn create_port_forward(
    store: tauri::State<'_, SessionStore>,
    input: CreatePortForwardInput,
) -> Result<PortForward, String> {
    store.create_port_forward(input)
}

#[tauri::command]
pub async fn update_port_forward(
    store: tauri::State<'_, SessionStore>,
    state: tauri::State<'_, MasterPasswordState>,
    pf_state: tauri::State<'_, crate::port_forward::PortForwardState>,
    ssh_state: tauri::State<'_, crate::ssh::SshState>,
    app: tauri::AppHandle,
    id: i64,
    input: UpdatePortForwardInput,
) -> Result<PortForward, String> {
    let updated = store.update_port_forward(id, input)?;
    crate::port_forward::restart_forward_if_running(
        &updated, &pf_state, &state, &ssh_state, &store, &app,
    )
    .await?;
    Ok(updated)
}

#[tauri::command]
pub fn delete_port_forward(store: tauri::State<'_, SessionStore>, id: i64) -> Result<(), String> {
    store.delete_port_forward(id)
}

#[tauri::command]
pub fn open_local_path(
    app: tauri::AppHandle,
    path: String,
    with: Option<String>,
) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    app.opener()
        .open_path(&path, with.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_local_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn show_open_with_dialog(path: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("rundll32.exe")
            .arg("shell32.dll,OpenAs_RunDLL")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Open with dialog is only implemented on Windows".to_string())
    }
}

#[tauri::command]
pub fn list_groups(store: tauri::State<'_, SessionStore>) -> Result<Vec<Group>, String> {
    store.list_groups()
}

#[tauri::command]
pub fn create_group(store: tauri::State<'_, SessionStore>, name: String) -> Result<Group, String> {
    store.create_group(name)
}

#[tauri::command]
pub fn delete_group(store: tauri::State<'_, SessionStore>, id: i64) -> Result<(), String> {
    store.delete_group(id)
}

#[tauri::command]
pub fn check_program(path: String) -> bool {
    #[cfg(target_os = "windows")]
    {
        if !path.to_lowercase().ends_with(".exe") {
            return std::path::Path::new(&format!("{}.exe", path)).exists()
                || std::path::Path::new(&path).exists();
        }
    }
    std::path::Path::new(&path).exists()
}
