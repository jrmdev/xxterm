use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::null_mut;

use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};
use windows_sys::core::w;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, MAX_PATH};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const CMD_ARGUMENT: &str = "--admin-terminal=cmd";
const POWERSHELL_ARGUMENT: &str = "--admin-terminal=powershell";

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminShell {
    Cmd,
    PowerShell,
}

impl AdminShell {
    fn launch_argument(self) -> &'static str {
        match self {
            Self::Cmd => CMD_ARGUMENT,
            Self::PowerShell => POWERSHELL_ARGUMENT,
        }
    }

    fn terminal(self) -> Result<AdminTerminal, String> {
        let relative_program = match self {
            Self::Cmd => "cmd.exe",
            Self::PowerShell => r"WindowsPowerShell\v1.0\powershell.exe",
        };
        let program = system_directory()
            .map_err(|error| error.to_string())?
            .join(relative_program)
            .into_os_string()
            .into_string()
            .map_err(|_| "Windows system directory is not valid Unicode".to_string())?;
        Ok(AdminTerminal {
            shell: self,
            program,
        })
    }
}

#[derive(Serialize)]
pub struct AdminTerminal {
    shell: AdminShell,
    program: String,
}

pub struct AdminTerminalState {
    pub elevated: bool,
    pub startup_shell: Option<AdminShell>,
}

impl AdminTerminalState {
    pub fn new() -> Self {
        let elevated = is_elevated().unwrap_or_else(|error| {
            log::warn!("Could not check Windows process elevation: {error}");
            false
        });
        let startup_shell = if elevated && !tauri::is_dev() {
            match std::env::args_os().nth(1).as_deref() {
                Some(arg) if arg == OsStr::new(CMD_ARGUMENT) => Some(AdminShell::Cmd),
                Some(arg) if arg == OsStr::new(POWERSHELL_ARGUMENT) => Some(AdminShell::PowerShell),
                _ => None,
            }
        } else {
            None
        };
        Self {
            elevated,
            startup_shell,
        }
    }
}

fn is_elevated() -> io::Result<bool> {
    let mut token = null_mut();
    // The token is opened for query only and closed after reading its elevation status.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut info = TOKEN_ELEVATION::default();
        let mut returned_size = 0;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            (&mut info as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned_size,
        );
        let elevation = if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(info.TokenIsElevated != 0)
        };
        CloseHandle(token);
        elevation
    }
}

fn system_directory() -> io::Result<PathBuf> {
    let mut buffer = [0u16; MAX_PATH as usize];
    // The API writes at most the supplied number of UTF-16 code units.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= buffer.len() {
        return Err(io::Error::other(
            "Windows system directory path is too long",
        ));
    }
    Ok(OsString::from_wide(&buffer[..length]).into())
}

#[tauri::command]
pub fn get_startup_admin_terminal(
    state: State<'_, AdminTerminalState>,
) -> Result<Option<AdminTerminal>, String> {
    state.startup_shell.map(AdminShell::terminal).transpose()
}

#[tauri::command]
pub fn open_admin_terminal(
    shell: AdminShell,
    state: State<'_, AdminTerminalState>,
    window: WebviewWindow,
) -> Result<Option<AdminTerminal>, String> {
    if state.elevated {
        return shell.terminal().map(Some);
    }
    // Only elevate a build with embedded frontend assets.
    if tauri::is_dev() {
        return Err("Open this profile from a built copy of xxterm.".into());
    }

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable: Vec<u16> = executable.as_os_str().encode_wide().chain([0]).collect();
    let parameters: Vec<u16> = shell.launch_argument().encode_utf16().chain([0]).collect();
    let directory = system_directory().map_err(|error| error.to_string())?;
    let directory: Vec<u16> = directory.as_os_str().encode_wide().chain([0]).collect();
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd: window.hwnd().map_err(|error| error.to_string())?.0,
        lpVerb: w!("runas"),
        lpFile: executable.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // This synchronous command runs on the window's COM-initialized UI thread.
    // The strings stay alive through the call and the shell manages the child process.
    if unsafe { ShellExecuteExW(&mut launch) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_CANCELLED as i32) {
            return Err(error.to_string());
        }
    }
    Ok(None)
}
