use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use base64::Engine as _;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::ssh::{self, SshState};
use crate::terminal_events::TerminalExitEvent;

pub struct PtyState {
    pub sessions: Mutex<HashMap<String, PtySession>>,
}

pub struct PtySession {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
    pub exit_requested: Arc<AtomicBool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyProfile {
    pub program: String,
    pub args: String,
    pub default_working_directory: String,
}

impl PtyState {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

fn parse_command_line_arguments(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    let mut has_content = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if matches!(chars.peek(), Some('"') | Some('\'')) {
                    if let Some(next) = chars.next() {
                        current.push(next);
                        has_content = true;
                    }
                } else {
                    current.push(ch);
                    has_content = true;
                }
            }
            '"' | '\'' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                    has_content = true;
                } else {
                    current.push(ch);
                    has_content = true;
                }
            }
            ch if ch.is_whitespace() && quote.is_none() => {
                if has_content {
                    args.push(current);
                    current = String::new();
                    has_content = false;
                }
            }
            _ => {
                current.push(ch);
                has_content = true;
            }
        }
    }

    if quote.is_some() {
        return Err("Command-Line Arguments contains an unterminated quote".to_string());
    }

    if has_content {
        args.push(current);
    }

    Ok(args)
}

fn configure_local_pty_environment(cmd: &mut CommandBuilder) {
    if cfg!(unix) {
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // Leave TERM_PROGRAM unset: some TUIs switch rendering paths based on it
        // and behave better when the local PTY looks like a generic xterm.
    }
}

#[tauri::command]
pub fn create_pty(
    id: String,
    cols: u16,
    rows: u16,
    profile: Option<PtyProfile>,
    state: tauri::State<PtyState>,
    app: AppHandle,
) -> Result<(), String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let program = profile
        .as_ref()
        .map(|profile| profile.program.trim())
        .filter(|program| !program.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_shell);
    let mut cmd = CommandBuilder::new(&program);
    configure_local_pty_environment(&mut cmd);
    if let Some(profile) = profile.as_ref() {
        for arg in parse_command_line_arguments(&profile.args)? {
            cmd.arg(arg);
        }
        let working_directory = profile.default_working_directory.trim();
        if !working_directory.is_empty() {
            cmd.cwd(working_directory);
        }
    }
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let killer = child.clone_killer();
    let exit_requested = Arc::new(AtomicBool::new(false));

    {
        let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        sessions.insert(
            id.clone(),
            PtySession {
                master: pair.master,
                writer,
                killer,
                exit_requested: exit_requested.clone(),
            },
        );
    }

    let handle = app.clone();
    let sid = id.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    log::info!("pty {}: EOF", sid);
                    break;
                }
                Ok(n) => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = handle.emit(&format!("pty-output-{}", sid), &encoded);
                }
                Err(e) => {
                    log::error!("pty {}: read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    let handle = app.clone();
    let sid = id.clone();
    std::thread::spawn(move || {
        let exit_event = match child.wait() {
            Ok(status) => {
                if exit_requested.load(Ordering::SeqCst) {
                    TerminalExitEvent::user_initiated()
                } else if let Some(signal) = status.signal() {
                    TerminalExitEvent::unexpected(format!("[Process terminated by {}]", signal))
                } else {
                    TerminalExitEvent::unexpected(format!(
                        "[Process exited with code {}]",
                        status.exit_code()
                    ))
                }
            }
            Err(err) => {
                if exit_requested.load(Ordering::SeqCst) {
                    TerminalExitEvent::user_initiated()
                } else {
                    TerminalExitEvent::unexpected(format!(
                        "[Process terminated unexpectedly: {}]",
                        err
                    ))
                }
            }
        };

        if let Ok(mut sessions) = handle.state::<PtyState>().sessions.lock() {
            sessions.remove(&sid);
        }
        let _ = handle.emit(&format!("pty-exit-{}", sid), exit_event);
    });

    Ok(())
}

#[tauri::command]
pub fn write_pty(
    id: String,
    data: String,
    pty_state: tauri::State<PtyState>,
    ssh_state: tauri::State<SshState>,
) -> Result<(), String> {
    {
        let mut sessions = pty_state.sessions.lock().map_err(|e| e.to_string())?;
        if let Some(session) = sessions.get_mut(&id) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&data)
                .map_err(|e| e.to_string())?;
            session
                .writer
                .write_all(&bytes)
                .map_err(|e| e.to_string())?;
            session.writer.flush().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    ssh::write_ssh_internal(&id, &data, &ssh_state)
}

#[tauri::command]
pub fn resize_pty(
    id: String,
    cols: u16,
    rows: u16,
    pty_state: tauri::State<PtyState>,
    ssh_state: tauri::State<SshState>,
) -> Result<(), String> {
    {
        let sessions = pty_state.sessions.lock().map_err(|e| e.to_string())?;
        if let Some(session) = sessions.get(&id) {
            session
                .master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    ssh::resize_ssh_internal(&id, cols, rows, &ssh_state)
}

#[tauri::command]
pub fn close_pty(
    id: String,
    pty_state: tauri::State<PtyState>,
    ssh_state: tauri::State<SshState>,
) -> Result<(), String> {
    {
        let mut sessions = pty_state.sessions.lock().map_err(|e| e.to_string())?;
        if let Some(mut session) = sessions.remove(&id) {
            session.exit_requested.store(true, Ordering::SeqCst);
            let _ = session.killer.kill();
            return Ok(());
        }
    }
    ssh::disconnect_ssh_internal(&id, &ssh_state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_pty_environment_overrides_terminal_defaults() {
        let mut cmd = CommandBuilder::new("/bin/bash");
        cmd.env("TERM", "dumb");

        configure_local_pty_environment(&mut cmd);

        #[cfg(unix)]
        {
            assert_eq!(
                cmd.get_env("TERM").and_then(|v| v.to_str()),
                Some("xterm-256color")
            );
            assert_eq!(
                cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
                Some("truecolor")
            );
            assert_eq!(cmd.get_env("TERM_PROGRAM"), None);
        }
    }
}
