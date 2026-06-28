# xxterm

xxterm is a cross-platform terminal emulator with built-in SSH and SFTP tools. It is designed for people who want a single desktop app for local shells, remote server access, file transfers, port forwarding, and saved connection profiles.

It has been built for full **tmux** compatibility, with full OSC 52 clipboard sync, copy-on-select, and intelligent paste handling that works seamlessly both inside and outside tmux.

xxterm brings the most common terminal workflows into one app:

- Run local shells in tabs
- Connect to remote machines over SSH
- Browse and manage files over SFTP
- Create and manage port forwards
- Save reusable connection profiles
- Switch between color themes

## Local vs SSH Sessions

xxterm supports both local and remote terminal sessions.

### Local sessions

Open native shells on your machine, including common terminals such as PowerShell, bash, zsh, fish, cmd, WSL, or custom profiles.

Use local sessions when you want:

- A fast shell for work on your own machine
- Multiple tabs for separate tasks
- A terminal profile that matches the shell you prefer (built-in and customizable profiles)

### SSH sessions

Connect to remote hosts from the same tabbed terminal interface. SSH sessions support:

- Password authentication
- Private key authentication
- SSH agent authentication
- Jump hosts
- Saved credentials protected by a master password
- Save session details for later reuse
- Organise sessions into groups
- Edit or remove existing sessions

## SFTP Browser

The SFTP browser lets you work with files on remote hosts from inside xxterm. SFT browser opens in a left-side pane with each SSH session you connect to.

- Navigate directories
- Upload and download files
- Rename files and folders
- Delete files and folders
- Create new directories
- Track transfer progress
- Edit a remote file locally and sync changes back on save

## Port Forward Management

xxterm includes a port forwarding manager for SSH-based forwarding.

You can create and manage:

- Local forwards (`-L`)
- Remote forwards (`-R`)
- Dynamic SOCKS5 forwards (`-D`)

The port forward manager supports:

- Per-session forward definitions
- Start and stop controls
- Auto-start on connect
- Status tracking

## Tmux & Clipboard

xxterm is **purpose-built for tmux users**. It handles tmux's unique terminal protocols natively so clipboard operations work the same way whether you are in a shell, inside tmux, or outside it entirely. `set-clipboard` in tmux works out of the box.

Selecting text with the mouse automatically copies it to the clipboard. Pasting works with **Ctrl+V / Cmd+V**, **middle-click**, and **Shift + Ins**.

## Themes

xxterm includes multiple color themes for the terminal and the UI.

You can change:

- Terminal color scheme
- Application theme styling
- Font family
- Font size

## Portable configuration

All settings are stored in a sqlite database in `~/.config/xxterm/xxterm.db`. Saved passwords are encrypted at rest. You can create backups of this file or move it to other systems to bring your saved sessions and configurations.

## Built With

- Tauri 2
- SolidJS
- TypeScript
- Rust
- xterm.js
- russh
- russh-sftp
- portable-pty
- rusqlite

## Development

```bash
npm install
npm run tauri dev
```

## Build

Use the provided scripts to compile a release build:

Linux / Mac OS:
```bash
./build-portable.sh
```

Windows:
```bash
.\build-portable.bat
```
