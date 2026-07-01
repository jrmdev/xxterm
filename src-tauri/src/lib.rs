mod commands;
mod crypto;
mod port_forward;
mod pty;
mod session_store;
mod sftp;
mod ssh;
mod ssh_config;
mod terminal_events;

use crypto::MasterPasswordState;
use port_forward::PortForwardState;
use pty::PtyState;
use session_store::SessionStore;
use sftp::{SftpEditState, SftpTransferState};
use ssh::SshState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::env::set_var("MESA_DEBUG", "silent");
    env_logger::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(PtyState::new())
        .manage(SshState::new())
        .manage(MasterPasswordState::new())
        .manage(PortForwardState::new())
        .manage(SftpEditState::new())
        .manage(SftpTransferState::new())
        .setup(|app| {
            let version = app.package_info().version.to_string();
            if let Some(window) = app.get_webview_window("main") {
                window.set_title(&format!("xxterm {version}"))?;
            }
            let store = SessionStore::new()?;
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pty::create_pty,
            pty::write_pty,
            pty::resize_pty,
            pty::close_pty,
            ssh::connect_ssh,
            ssh::disconnect_ssh,
            commands::list_sessions,
            commands::get_session,
            commands::create_session,
            commands::update_session,
            commands::delete_session,
            commands::list_groups,
            commands::create_group,
            commands::delete_group,
            commands::get_master_password_status,
            commands::get_app_bootstrap_state,
            commands::set_master_password,
            commands::unlock_master_password,
            commands::lock_master_password,
            commands::change_master_password,
            commands::store_encrypted_password,
            commands::get_encrypted_password,
            commands::delete_encrypted_password,
            commands::inspect_private_key_passphrase,
            commands::store_encrypted_key_passphrase,
            commands::get_encrypted_key_passphrase,
            commands::has_encrypted_key_passphrase,
            commands::delete_encrypted_key_passphrase,
            commands::get_setting,
            commands::set_setting,
            commands::read_clipboard,
            commands::list_monospace_fonts,
            commands::import_ssh_config,
            commands::list_ssh_config_entries,
            commands::open_local_path,
            commands::reveal_local_path,
            commands::show_open_with_dialog,
            commands::list_port_forwards,
            commands::create_port_forward,
            commands::update_port_forward,
            commands::delete_port_forward,
            sftp::sftp_list_dir,
            sftp::sftp_download,
            sftp::sftp_download_selection,
            sftp::sftp_edit,
            sftp::sftp_upload,
            sftp::sftp_upload_selection,
            sftp::sftp_upload_bytes,
            sftp::sftp_delete,
            sftp::sftp_delete_selection,
            sftp::sftp_rename,
            sftp::sftp_create_dir,
            sftp::cancel_sftp_queue,
            port_forward::auto_start_port_forwards,
            port_forward::auto_stop_port_forwards,
            port_forward::start_port_forwards,
            port_forward::stop_port_forwards,
            port_forward::start_single_port_forward,
            port_forward::stop_single_port_forward,
            port_forward::get_port_forward_status,
            commands::check_program,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
