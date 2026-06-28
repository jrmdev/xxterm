# xxterm

<img width="1500" height="877" alt="image" src="https://github.com/user-attachments/assets/abdccac4-ce8c-4de3-bad2-b60b2b9424c6" />

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

## Portable configuration

All settings are stored in a sqlite database in `~/.config/xxterm/xxterm.db`. Saved passwords are encrypted at rest. You can create backups of this file or move it to other systems to bring your saved sessions and configurations.

## Screenshots

### Session Manager & Main Window

<img width="1227" height="841" alt="image" src="https://github.com/user-attachments/assets/e1fb2588-b613-4287-8979-8369058a9eb9" />

### SFTP Browser & tmux

<img width="1227" height="841" alt="image" src="https://github.com/user-attachments/assets/12b5cb83-60f0-4822-a73e-db95a5898a54" />

### Global Settings

<img width="520" height="731" alt="image1" src="https://github.com/user-attachments/assets/8496a30d-1287-4b65-b6d1-cf96e0fa44dd" />

### SSH Session Configuration

<img width="501" height="621" alt="image2" src="https://github.com/user-attachments/assets/8713f2e2-9ea3-4c16-8728-a72c3b703cca" />

### Terminal Profiles

<img width="859" height="525" alt="image5" src="https://github.com/user-attachments/assets/bd5fedc2-c756-4cc0-8701-8c49f04bfd9e" />

### Port Forwards (Per-Session)

<img width="559" height="461" alt="image4" src="https://github.com/user-attachments/assets/4caf15c4-d20a-43ad-a86f-5e2196888f54" />

### Port Frwards (Left Pane View)

<img width="1003" height="260" alt="image6" src="https://github.com/user-attachments/assets/804697ab-5e89-4c6f-b051-7ef6f84c347a" />

### Session Recovery

<img width="898" height="420" alt="image7" src="https://github.com/user-attachments/assets/14a846f7-ecb4-4ce7-ad31-db9be098b1b5" />

## Themes

xxterm includes over 40 color themes for the terminal and the UI. Below are a few examples.

<img width="1280" height="1024" alt="screenshot_grid_1280x1024" src="https://github.com/user-attachments/assets/0b1f1f5a-7a01-4f3a-b213-564b2f0e69e6" />

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
