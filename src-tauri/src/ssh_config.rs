use std::path::Path;

use crate::session_store::SshConfigEntry;

pub fn parse_ssh_config(path: &Path) -> Result<Vec<SshConfigEntry>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let mut entries = Vec::new();
    let mut current_host: Option<String> = None;
    let mut current_host_name: Option<String> = None;
    let mut current_port: Option<u16> = None;
    let mut current_user: Option<String> = None;
    let mut current_identity_file: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Host ") {
            if let Some(host) = current_host.take() {
                entries.push(SshConfigEntry {
                    host,
                    host_name: current_host_name.take(),
                    port: current_port.take(),
                    user: current_user.take(),
                    identity_file: current_identity_file.take(),
                });
            }
            current_host = Some(rest.trim().to_string());
            current_host_name = None;
            current_port = None;
            current_user = None;
            current_identity_file = None;
        } else if current_host.is_some() {
            if let Some(val) = trimmed.strip_prefix("HostName ") {
                current_host_name = Some(val.trim().to_string());
            } else if let Some(val) = trimmed.strip_prefix("Port ") {
                current_port = val.trim().parse::<u16>().ok();
            } else if let Some(val) = trimmed.strip_prefix("User ") {
                current_user = Some(val.trim().to_string());
            } else if let Some(val) = trimmed.strip_prefix("IdentityFile ") {
                let id_path = val.trim().to_string();
                let expanded = if id_path.starts_with('~') {
                    if let Some(home) = dirs::home_dir() {
                        home.join(&id_path[2..]).to_string_lossy().to_string()
                    } else {
                        id_path
                    }
                } else {
                    id_path
                };
                current_identity_file = Some(expanded);
            }
        }
    }

    if let Some(host) = current_host {
        entries.push(SshConfigEntry {
            host,
            host_name: current_host_name,
            port: current_port,
            user: current_user,
            identity_file: current_identity_file,
        });
    }

    Ok(entries)
}
