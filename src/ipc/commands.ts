import { invoke } from "@tauri-apps/api/core";
import type { TerminalProfile } from "../stores/terminalProfiles";

export interface KeyPassphraseInput {
  sessionId: number;
  passphrase: string;
}

export interface PrivateKeyPassphraseStatus {
  requiresPassphrase: boolean;
  passphraseValid: boolean;
  error: string | null;
}

export async function createPty(id: string, cols: number, rows: number, profile?: TerminalProfile): Promise<void> {
  await invoke("create_pty", {
    id,
    cols,
    rows,
    profile: profile
      ? {
          program: profile.program,
          args: profile.args,
          defaultWorkingDirectory: profile.defaultWorkingDirectory,
        }
      : null,
  });
}

export async function writePty(id: string, data: string): Promise<void> {
  await invoke("write_pty", { id, data });
}

export async function resizePty(id: string, cols: number, rows: number): Promise<void> {
  await invoke("resize_pty", { id, cols, rows });
}

export async function closePty(id: string): Promise<void> {
  await invoke("close_pty", { id });
}

export async function listSessions(): Promise<any[]> {
  return await invoke("list_sessions");
}

export async function getSession(id: number): Promise<any> {
  return await invoke("get_session", { id });
}

export async function createSession(input: any): Promise<any> {
  return await invoke("create_session", { input });
}

export async function updateSession(id: number, input: any): Promise<any> {
  return await invoke("update_session", { id, input });
}

export async function deleteSession(id: number): Promise<void> {
  await invoke("delete_session", { id });
}

export async function getMasterPasswordStatus(): Promise<{ hasMasterPassword: boolean; unlocked: boolean }> {
  return await invoke("get_master_password_status");
}

export async function getAppBootstrapState(): Promise<{
  fontFamily: string | null;
  fontSize: string | null;
  colorScheme: string | null;
  codeEditorCommand: string | null;
  scrollback: string | null;
  hasMasterPassword: boolean;
  unlocked: boolean;
}> {
  return await invoke("get_app_bootstrap_state");
}

export async function setMasterPassword(password: string): Promise<void> {
  await invoke("set_master_password", { password });
}

export async function unlockMasterPassword(password: string): Promise<void> {
  await invoke("unlock_master_password", { password });
}

export async function lockMasterPassword(): Promise<void> {
  await invoke("lock_master_password");
}

export async function changeMasterPassword(oldPassword: string, newPassword: string): Promise<void> {
  await invoke("change_master_password", { oldPassword, newPassword });
}

export async function storeEncryptedPassword(sessionId: number, password: string): Promise<void> {
  await invoke("store_encrypted_password", { sessionId, password });
}

export async function getEncryptedPassword(sessionId: number): Promise<string | null> {
  return await invoke("get_encrypted_password", { sessionId });
}

export async function deleteEncryptedPassword(sessionId: number): Promise<void> {
  await invoke("delete_encrypted_password", { sessionId });
}

export async function inspectPrivateKeyPassphrase(sessionId: number, passphrase?: string): Promise<PrivateKeyPassphraseStatus> {
  return await invoke("inspect_private_key_passphrase", {
    input: {
      sessionId,
      passphrase: passphrase ?? null,
    },
  });
}

export async function storeEncryptedKeyPassphrase(sessionId: number, passphrase: string): Promise<void> {
  await invoke("store_encrypted_key_passphrase", { sessionId, passphrase });
}

export async function getEncryptedKeyPassphrase(sessionId: number): Promise<string | null> {
  return await invoke("get_encrypted_key_passphrase", { sessionId });
}

export async function hasEncryptedKeyPassphrase(sessionId: number): Promise<boolean> {
  return await invoke("has_encrypted_key_passphrase", { sessionId });
}

export async function deleteEncryptedKeyPassphrase(sessionId: number): Promise<void> {
  await invoke("delete_encrypted_key_passphrase", { sessionId });
}

export async function getSetting(key: string): Promise<string | null> {
  return await invoke("get_setting", { key });
}

export async function setSetting(key: string, value: string): Promise<void> {
  await invoke("set_setting", { key, value });
}

export async function readClipboard(): Promise<string> {
  return await invoke<string>("read_clipboard");
}

export async function listMonospaceFonts(): Promise<string[]> {
  return await invoke("list_monospace_fonts");
}

export async function openLocalPath(path: string, withProgram?: string): Promise<void> {
  await invoke("open_local_path", { path, with: withProgram ?? null });
}

export async function revealLocalPath(path: string): Promise<void> {
  await invoke("reveal_local_path", { path });
}

export async function showOpenWithDialog(path: string): Promise<void> {
  await invoke("show_open_with_dialog", { path });
}

export async function connectSsh(id: string, sessionId: number, cols: number, rows: number, keyPassphrases?: KeyPassphraseInput[]): Promise<void> {
  await invoke("connect_ssh", { id, sessionId, cols, rows, keyPassphrases: keyPassphrases ?? null });
}

export async function disconnectSsh(id: string): Promise<void> {
  await invoke("disconnect_ssh", { id });
}

export async function importSshConfig(): Promise<number> {
  return await invoke("import_ssh_config");
}

export async function listSshConfigEntries(): Promise<any[]> {
  return await invoke("list_ssh_config_entries");
}

export async function listPortForwards(sessionId: number): Promise<any[]> {
  return await invoke("list_port_forwards", { sessionId });
}

export async function createPortForward(input: any): Promise<any> {
  return await invoke("create_port_forward", { input });
}

export async function updatePortForward(id: number, input: any): Promise<any> {
  return await invoke("update_port_forward", { id, input });
}

export async function deletePortForward(id: number): Promise<void> {
  await invoke("delete_port_forward", { id });
}

export async function listGroups(): Promise<{ id: number; name: string }[]> {
  return await invoke("list_groups");
}

export async function createGroup(name: string): Promise<{ id: number; name: string }> {
  return await invoke("create_group", { name });
}

export async function deleteGroup(id: number): Promise<void> {
  await invoke("delete_group", { id });
}

export async function startPortForwards(sessionId: number, keyPassphrases?: KeyPassphraseInput[]): Promise<void> {
  await invoke("start_port_forwards", { sessionId, keyPassphrases: keyPassphrases ?? null });
}

export async function stopPortForwards(sessionId: number): Promise<void> {
  await invoke("stop_port_forwards", { sessionId });
}

export async function autoStartPortForwards(sessionId: number, terminalId: string, keyPassphrases?: KeyPassphraseInput[]): Promise<void> {
  await invoke("auto_start_port_forwards", { sessionId, terminalId, keyPassphrases: keyPassphrases ?? null });
}

export async function autoStopPortForwards(sessionId: number, terminalId: string): Promise<void> {
  await invoke("auto_stop_port_forwards", { sessionId, terminalId });
}

export async function getPortForwardStatus(sessionId: number): Promise<any[]> {
  return await invoke("get_port_forward_status", { sessionId });
}

export async function startSinglePortForward(sessionId: number, forwardId: number, keyPassphrases?: KeyPassphraseInput[]): Promise<void> {
  await invoke("start_single_port_forward", { sessionId, forwardId, keyPassphrases: keyPassphrases ?? null });
}

export async function stopSinglePortForward(sessionId: number, forwardId: number): Promise<void> {
  await invoke("stop_single_port_forward", { sessionId, forwardId });
}

export async function sftpListDir(id: string, path: string): Promise<{ path: string; entries: any[] }> {
  return await invoke("sftp_list_dir", { id, path });
}

export async function sftpDownload(id: string, remotePath: string): Promise<string> {
  return await invoke("sftp_download", { id, remotePath });
}

export async function sftpDownloadSelection(id: string, remotePaths: string[], destinationDir: string): Promise<void> {
  await invoke("sftp_download_selection", { id, remotePaths, destinationDir });
}

export async function sftpEdit(id: string, remotePath: string, withProgram?: string): Promise<string> {
  return await invoke("sftp_edit", { id, remotePath, with: withProgram ?? null });
}

export async function sftpUpload(id: string, remotePath: string, localPath: string): Promise<void> {
  await invoke("sftp_upload", { id, remotePath, localPath });
}

export async function sftpUploadSelection(id: string, entries: { remotePath: string; localPath: string }[]): Promise<void> {
  await invoke("sftp_upload_selection", { id, entries });
}

export async function sftpUploadBytes(id: string, remotePath: string, data: Uint8Array): Promise<void> {
  await invoke("sftp_upload_bytes", { id, remotePath, data: Array.from(data) });
}

export async function sftpDelete(id: string, path: string, isDir: boolean): Promise<void> {
  await invoke("sftp_delete", { id, path, isDir });
}

export async function sftpDeleteSelection(id: string, entries: { path: string; isDir: boolean }[]): Promise<void> {
  await invoke("sftp_delete_selection", { id, entries });
}

export async function sftpRename(id: string, oldPath: string, newPath: string): Promise<void> {
  await invoke("sftp_rename", { id, oldPath, newPath });
}

export async function sftpCreateDir(id: string, path: string): Promise<void> {
  await invoke("sftp_create_dir", { id, path });
}

export async function cancelSftpQueue(id: string, queueId: string): Promise<void> {
  await invoke("cancel_sftp_queue", { id, queueId });
}
