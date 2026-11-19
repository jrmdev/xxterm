use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{stream::FuturesUnordered, StreamExt};
use russh::ChannelMsg;
use russh_sftp::client::{
    error::Error as SftpError, Config as SftpConfig, RawSftpSession, SftpSession,
};
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode, Version};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::ssh::{SharedSshHandle, SshState};

static SFTP_CACHE: OnceLock<Mutex<HashMap<String, Arc<SftpSession>>>> = OnceLock::new();

fn sftp_cache() -> &'static Mutex<HashMap<String, Arc<SftpSession>>> {
    SFTP_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn invalidate_sftp_cache(id: &str) {
    if let Ok(mut cache) = sftp_cache().lock() {
        cache.remove(id);
    }
}

const EDIT_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const EDIT_WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
const TRANSFER_BUFFER_SIZE: usize = 256 * 1024;
const TRANSFER_PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);
const SFTP_UPLOAD_CHUNK_SIZE: usize = 32 * 1024;
const SFTP_UPLOAD_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const SFTP_MAX_PACKET_LEN: u32 = 1024 * 1024;
const SFTP_MAX_CONCURRENT_WRITES: usize = 32;
const SFTP_UPLOAD_WRITE_DEPTH: usize = 16;
const POSIX_RENAME: &str = "posix-rename@openssh.com";
const SFTP_REQUEST_TIMEOUT_SECS: u64 = 300;
const PIPELINED_DOWNLOAD_MIN_SIZE: u64 = 8 * 1024 * 1024;
const PIPELINED_READ_SIZE: u64 = 16 * 1024;
const PIPELINED_READ_DEPTH: usize = 64;

fn transfer_staging_name() -> Result<String, String> {
    let nonce = ring::rand::generate::<[u8; 16]>(&ring::rand::SystemRandom::new())
        .map_err(|_| "Failed to generate a temporary transfer filename".to_string())?;
    Ok(format!(
        ".xxterm-{:032x}.part",
        u128::from_be_bytes(nonce.expose())
    ))
}

async fn create_local_download(destination: &Path) -> Result<(fs::File, PathBuf), String> {
    let staging_path = destination.with_file_name(transfer_staging_name()?);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging_path)
        .await
        .map_err(|e| {
            format!(
                "Failed to create a temporary file for {}: {e}",
                destination.display()
            )
        })?;
    Ok((file, staging_path))
}

async fn finish_local_download(
    mut file: fs::File,
    staging_path: &Path,
    destination: &Path,
    result: Result<(), String>,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    let flush_result = file
        .flush()
        .await
        .map_err(|e| format!("Failed to flush {}: {e}", destination.display()));
    drop(file);

    let result = async {
        result.and(flush_result)?;
        if cancel.is_cancelled() {
            return Err("Download canceled".to_string());
        }
        fs::rename(staging_path, destination)
            .await
            .map_err(|e| format!("Failed to save {}: {e}", destination.display()))
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(staging_path).await;
    }
    result
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub modified: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditUploadErrorEvent {
    terminal_id: String,
    session_id: i64,
    remote_path: String,
    local_path: String,
    error: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferProgressEvent {
    terminal_id: String,
    session_id: i64,
    queue_id: String,
    transfer_id: String,
    direction: String,
    path: String,
    display_path: String,
    transferred: u64,
    total: Option<u64>,
    done: bool,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified_at: Option<SystemTime>,
}

struct EditWatcherHandle {
    generation: u64,
    stop_tx: watch::Sender<bool>,
}

pub struct SftpEditState {
    watchers: Mutex<HashMap<String, HashMap<String, EditWatcherHandle>>>,
    next_generation: Mutex<u64>,
}

#[derive(Clone)]
struct TransferCancelToken {
    cancelled: Arc<AtomicBool>,
}

impl TransferCancelToken {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub struct SftpTransferState {
    queues: Mutex<HashMap<String, HashMap<String, TransferCancelToken>>>,
}

impl SftpTransferState {
    pub fn new() -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
        }
    }

    fn register_queue(
        &self,
        terminal_id: &str,
        queue_id: &str,
    ) -> Result<TransferCancelToken, String> {
        let token = TransferCancelToken::new();
        let mut queues = self.queues.lock().map_err(|e| e.to_string())?;
        queues
            .entry(terminal_id.to_string())
            .or_default()
            .insert(queue_id.to_string(), token.clone());
        Ok(token)
    }

    fn cancel_queue(&self, terminal_id: &str, queue_id: &str) -> Result<bool, String> {
        let queues = self.queues.lock().map_err(|e| e.to_string())?;
        let Some(token) = queues
            .get(terminal_id)
            .and_then(|items| items.get(queue_id))
        else {
            return Ok(false);
        };
        token.cancel();
        Ok(true)
    }

    fn finish_queue(&self, terminal_id: &str, queue_id: &str) -> Result<(), String> {
        let mut queues = self.queues.lock().map_err(|e| e.to_string())?;
        let Some(terminal_queues) = queues.get_mut(terminal_id) else {
            return Ok(());
        };
        terminal_queues.remove(queue_id);
        if terminal_queues.is_empty() {
            queues.remove(terminal_id);
        }
        Ok(())
    }
}

impl SftpEditState {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
            next_generation: Mutex::new(0),
        }
    }

    fn replace_watcher(
        &self,
        terminal_id: &str,
        remote_path: &str,
    ) -> Result<(u64, watch::Receiver<bool>), String> {
        let (stop_tx, stop_rx) = watch::channel(false);

        let generation = {
            let mut next = self.next_generation.lock().map_err(|e| e.to_string())?;
            *next += 1;
            *next
        };

        let previous = {
            let mut watchers = self.watchers.lock().map_err(|e| e.to_string())?;
            let terminal_watchers = watchers.entry(terminal_id.to_string()).or_default();
            terminal_watchers.insert(
                remote_path.to_string(),
                EditWatcherHandle {
                    generation,
                    stop_tx,
                },
            )
        };

        if let Some(handle) = previous {
            let _ = handle.stop_tx.send(true);
        }

        Ok((generation, stop_rx))
    }

    pub fn stop_terminal(&self, terminal_id: &str) -> Result<(), String> {
        let removed = self
            .watchers
            .lock()
            .map_err(|e| e.to_string())?
            .remove(terminal_id);

        if let Some(handles) = removed {
            for (_, handle) in handles {
                let _ = handle.stop_tx.send(true);
            }
        }

        Ok(())
    }

    fn remove_watcher_if_current(
        &self,
        terminal_id: &str,
        remote_path: &str,
        generation: u64,
    ) -> Result<(), String> {
        let mut watchers = self.watchers.lock().map_err(|e| e.to_string())?;
        let Some(terminal_watchers) = watchers.get_mut(terminal_id) else {
            return Ok(());
        };

        let should_remove = terminal_watchers
            .get(remote_path)
            .map(|handle| handle.generation == generation)
            .unwrap_or(false);

        if should_remove {
            terminal_watchers.remove(remote_path);
            if terminal_watchers.is_empty() {
                watchers.remove(terminal_id);
            }
        }

        Ok(())
    }
}

async fn create_sftp_session(shared_handle: &SharedSshHandle) -> Result<SftpSession, String> {
    let channel = {
        let handle = shared_handle.lock().await;
        handle
            .channel_open_session()
            .await
            .map_err(|e| format!("Failed to open SFTP channel: {}", e))?
    };

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("Failed to start SFTP subsystem: {}", e))?;

    SftpSession::new_with_config(channel.into_stream(), sftp_config())
        .await
        .map_err(|e| format!("Failed to initialize SFTP session: {}", e))
}

async fn create_raw_sftp_session(
    shared_handle: &SharedSshHandle,
) -> Result<(Arc<RawSftpSession>, Version), String> {
    let channel = {
        let handle = shared_handle.lock().await;
        handle
            .channel_open_session()
            .await
            .map_err(|e| format!("Failed to open SFTP channel: {}", e))?
    };

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("Failed to start SFTP subsystem: {}", e))?;

    let session = RawSftpSession::new_with_config(channel.into_stream(), sftp_config());
    let version = session
        .init()
        .await
        .map_err(|e| format!("Failed to initialize SFTP session: {}", e))?;
    Ok((Arc::new(session), version))
}

fn sftp_config() -> SftpConfig {
    SftpConfig {
        max_packet_len: SFTP_MAX_PACKET_LEN,
        max_concurrent_writes: SFTP_MAX_CONCURRENT_WRITES,
        request_timeout_secs: SFTP_REQUEST_TIMEOUT_SECS,
    }
}

async fn get_or_create_sftp(
    state: &SshState,
    id: &str,
) -> Result<(Arc<SftpSession>, SharedSshHandle), String> {
    let shared_handle = {
        let handles = state.ssh_handles.lock().map_err(|e| e.to_string())?;
        handles
            .get(id)
            .cloned()
            .ok_or_else(|| "SFTP is only available while an SSH session is connected".to_string())?
    };

    if let Ok(cache) = sftp_cache().lock() {
        if let Some(sftp) = cache.get(id) {
            return Ok((Arc::clone(sftp), shared_handle));
        }
    }

    let sftp = Arc::new(create_sftp_session(&shared_handle).await?);

    if let Ok(mut cache) = sftp_cache().lock() {
        cache.insert(id.to_string(), Arc::clone(&sftp));
    }

    Ok((sftp, shared_handle))
}

async fn with_sftp_session<T, F, Fut>(state: &SshState, id: &str, op: F) -> Result<T, String>
where
    F: FnOnce(Arc<SftpSession>, SharedSshHandle) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let (sftp, shared_handle) = get_or_create_sftp(state, id).await?;
    let result = op(sftp, shared_handle).await;
    if result.is_err() {
        invalidate_sftp_cache(id);
    }
    result
}

fn session_id_for_terminal(state: &SshState, id: &str) -> Result<i64, String> {
    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .get(id)
        .map(|session| session.session_id)
        .ok_or_else(|| "SFTP is only available while an SSH session is connected".to_string())
}

fn normalize_remote_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn local_download_dir(id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("xxterm")
        .join("downloads")
        .join(id)
}

fn sanitize_local_name(segment: &str) -> String {
    let mut sanitized = String::with_capacity(segment.len());
    for ch in segment.chars() {
        let invalid =
            matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control();
        sanitized.push(if invalid { '_' } else { ch });
    }

    let trimmed = sanitized.trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

fn local_download_path(id: &str, remote_path: &str) -> Result<PathBuf, String> {
    Ok(local_download_dir(id)
        .join("edit")
        .join(transfer_staging_name()?)
        .join(remote_display_name(remote_path)))
}

fn local_open_download_path(id: &str, remote_path: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    local_download_dir(id)
        .join("open")
        .join(nonce.to_string())
        .join(remote_display_name(remote_path))
}

fn remote_display_name(path: &str) -> String {
    path.split('/')
        .rfind(|segment| !segment.is_empty() && *segment != ".")
        .map(|segment| {
            if segment == ".." {
                "__parent__".to_string()
            } else {
                sanitize_local_name(segment)
            }
        })
        .unwrap_or_else(|| "root".to_string())
}

fn local_name_collision_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

fn transfer_id_for_path(direction: &str, path: &str) -> String {
    format!("{direction}:{path}")
}

fn batch_transfer_id(session_id: i64) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("download-batch:{session_id}:{nonce}")
}

fn queue_id(prefix: &str, session_id: i64) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}:{session_id}:{nonce}")
}

async fn ensure_local_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create local directory: {}", e))?;
    }
    Ok(())
}

async fn emit_transfer_progress(
    app: &AppHandle,
    id: &str,
    session_id: i64,
    queue_id: &str,
    transfer_id: &str,
    direction: &str,
    path: &str,
    display_path: &str,
    transferred: u64,
    total: Option<u64>,
    done: bool,
    error: Option<String>,
) {
    let _ = app.emit(
        "sftp-transfer-progress",
        TransferProgressEvent {
            terminal_id: id.to_string(),
            session_id,
            queue_id: queue_id.to_string(),
            transfer_id: transfer_id.to_string(),
            direction: direction.to_string(),
            path: path.to_string(),
            display_path: display_path.to_string(),
            transferred,
            total,
            done,
            error,
        },
    );
}

async fn emit_transfer_error(
    app: &AppHandle,
    id: &str,
    session_id: i64,
    queue_id: &str,
    transfer_id: &str,
    direction: &str,
    path: &str,
    display_path: &str,
    transferred: u64,
    total: Option<u64>,
    error: &str,
) {
    emit_transfer_progress(
        app,
        id,
        session_id,
        queue_id,
        transfer_id,
        direction,
        path,
        display_path,
        transferred,
        total,
        true,
        Some(error.to_string()),
    )
    .await;
}

async fn emit_edit_upload_error(
    app: &AppHandle,
    id: &str,
    session_id: i64,
    remote_path: &str,
    local_path: &Path,
    error: &str,
) {
    let _ = app.emit(
        "sftp-edit-upload-error",
        EditUploadErrorEvent {
            terminal_id: id.to_string(),
            session_id,
            remote_path: remote_path.to_string(),
            local_path: local_path.to_string_lossy().to_string(),
            error: error.to_string(),
        },
    );
}

struct PipelinedReadChunk {
    offset: u64,
    requested: usize,
    data: Vec<u8>,
}

type PipelinedReadFuture = Pin<Box<dyn Future<Output = Result<PipelinedReadChunk, String>> + Send>>;

fn queue_pipelined_read(
    reads: &mut FuturesUnordered<PipelinedReadFuture>,
    session: &Arc<RawSftpSession>,
    handle: &str,
    offset: u64,
    len: usize,
) {
    let session = Arc::clone(session);
    let handle = handle.to_string();
    reads.push(Box::pin(async move {
        let data = session
            .read(handle, offset, len as u32)
            .await
            .map_err(|e| format!("SFTP read failed at offset {}: {}", offset, e))?;

        Ok(PipelinedReadChunk {
            offset,
            requested: len,
            data: data.data,
        })
    }));
}

async fn download_remote_file_pipelined(
    ssh_handle: &SharedSshHandle,
    app: &AppHandle,
    id: &str,
    session_id: i64,
    remote_path: &str,
    local_path: &Path,
    local: &mut fs::File,
    queue_id: &str,
    transfer_id: &str,
    display_path: &str,
    cancel: &TransferCancelToken,
    file_size: u64,
    transferred: &mut u64,
    last_progress_emit: &mut Instant,
) -> Result<(), String> {
    let (raw, _) = tokio::select! {
        _ = cancel.cancelled() => return Err("Download canceled".to_string()),
        session = create_raw_sftp_session(ssh_handle) => session?,
    };
    let handle = tokio::select! {
        _ = cancel.cancelled() => return Err("Download canceled".to_string()),
        opened = raw.open(remote_path, OpenFlags::READ, FileAttributes::empty()) => {
            opened.map_err(|e| format!("Failed to open {remote_path}: {e}"))?.handle
        }
    };

    let mut reads = FuturesUnordered::new();
    let mut pending = BTreeMap::<u64, Vec<u8>>::new();
    let mut next_request_offset = 0_u64;
    let mut next_write_offset = 0_u64;

    let result: Result<(), String> = async {
        while next_write_offset < file_size {
            if cancel.is_cancelled() {
                return Err("Transfer canceled".to_string());
            }

            let window_end = next_write_offset
                .saturating_add(PIPELINED_READ_SIZE * PIPELINED_READ_DEPTH as u64)
                .min(file_size);
            while reads.len() < PIPELINED_READ_DEPTH && next_request_offset < window_end {
                let len = (file_size - next_request_offset).min(PIPELINED_READ_SIZE);
                if len > window_end - next_request_offset {
                    break;
                }
                queue_pipelined_read(&mut reads, &raw, &handle, next_request_offset, len as usize);
                next_request_offset += len;
            }

            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err("Download canceled".to_string()),
                chunk = reads.next() => chunk.ok_or_else(|| "SFTP read pipeline stalled".to_string())??,
            };

            if chunk.data.is_empty() {
                return Err(format!("Unexpected EOF reading {}", remote_path));
            }
            if chunk.data.len() > chunk.requested {
                return Err(format!(
                    "SFTP server returned too much data for {}",
                    remote_path
                ));
            }

            let actual_end = chunk.offset + chunk.data.len() as u64;
            let requested_end = chunk.offset + chunk.requested as u64;
            pending.insert(chunk.offset, chunk.data);

            if actual_end < requested_end && actual_end < file_size {
                let len = (requested_end.min(file_size) - actual_end) as usize;
                queue_pipelined_read(&mut reads, &raw, &handle, actual_end, len);
            }

            while let Some(data) = pending.remove(&next_write_offset) {
                local
                    .write_all(&data)
                    .await
                    .map_err(|e| format!("Failed to write {}: {}", local_path.display(), e))?;
                next_write_offset += data.len() as u64;
                *transferred = next_write_offset;

                if last_progress_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
                    emit_transfer_progress(
                        app,
                        id,
                        session_id,
                        queue_id,
                        transfer_id,
                        "download",
                        remote_path,
                        display_path,
                        *transferred,
                        Some(file_size),
                        false,
                        None,
                    )
                    .await;
                    *last_progress_emit = Instant::now();
                }
            }
        }

        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = raw.close_session();
        return result;
    }
    let close_result = tokio::select! {
        _ = cancel.cancelled() => Err("Download canceled".to_string()),
        result = async {
            let metadata = raw.fstat(&handle).await
                .map_err(|e| format!("Failed to verify {remote_path}: {e}"))?;
            if metadata.attrs.size.is_some_and(|size| size != file_size) {
                return Err(format!("{remote_path} changed size during download"));
            }
            raw.close(handle).await.map_err(|e| format!("Failed to close {remote_path}: {e}"))
        } => result.map(|_| ()),
    };
    let _ = raw.close_session();

    close_result?;
    Ok(())
}

async fn file_fingerprint(path: &Path) -> Option<FileFingerprint> {
    let metadata = fs::metadata(path).await.ok()?;
    Some(FileFingerprint {
        len: metadata.len(),
        modified_at: metadata.modified().ok(),
    })
}

async fn await_download_operation<T, E: std::fmt::Display>(
    cancel: &TransferCancelToken,
    context: &str,
    operation: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    tokio::select! {
        _ = cancel.cancelled() => Err("Download canceled".to_string()),
        result = operation => result.map_err(|error| format!("{context}: {error}")),
    }
}

async fn download_remote_file(
    sftp: &SftpSession,
    ssh_handle: &SharedSshHandle,
    app: &AppHandle,
    id: &str,
    session_id: i64,
    remote_path: &str,
    local_path: &Path,
    queue_id: &str,
    transfer_id: &str,
    display_path: &str,
    cancel: &TransferCancelToken,
    finalize: bool,
) -> Result<String, String> {
    if cancel.is_cancelled() {
        return Err("Transfer canceled".to_string());
    }

    let metadata = await_download_operation(
        cancel,
        &format!("Failed to inspect {remote_path}"),
        sftp.metadata(remote_path),
    )
    .await?;

    let file_size = metadata.len();
    let total = metadata.size;
    emit_transfer_progress(
        app,
        id,
        session_id,
        queue_id,
        transfer_id,
        "download",
        remote_path,
        display_path,
        0,
        total,
        false,
        None,
    )
    .await;

    let mut transferred = 0_u64;
    let mut last_progress_emit = Instant::now();
    let result: Result<String, String> = async {
        let (mut local, staging_path) = create_local_download(local_path).await?;
        let copy_result = async {
            if file_size >= PIPELINED_DOWNLOAD_MIN_SIZE {
                download_remote_file_pipelined(
                    ssh_handle,
                    app,
                    id,
                    session_id,
                    remote_path,
                    local_path,
                    &mut local,
                    queue_id,
                    transfer_id,
                    display_path,
                    cancel,
                    file_size,
                    &mut transferred,
                    &mut last_progress_emit,
                )
                .await?;
            } else {
                let mut remote = await_download_operation(
                    cancel,
                    &format!("Failed to open {remote_path}"),
                    sftp.open(remote_path),
                )
                .await?;
                let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
                loop {
                    if cancel.is_cancelled() {
                        return Err("Transfer canceled".to_string());
                    }
                    let count = tokio::select! {
                        _ = cancel.cancelled() => return Err("Download canceled".to_string()),
                        read = remote.read(&mut buffer) => {
                            read.map_err(|e| format!("Failed to read {}: {}", remote_path, e))?
                        },
                    };
                    if count == 0 {
                        break;
                    }
                    local
                        .write_all(&buffer[..count])
                        .await
                        .map_err(|e| format!("Failed to write {}: {}", local_path.display(), e))?;
                    transferred += count as u64;
                    if last_progress_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
                        emit_transfer_progress(
                            app,
                            id,
                            session_id,
                            queue_id,
                            transfer_id,
                            "download",
                            remote_path,
                            display_path,
                            transferred,
                            total,
                            false,
                            None,
                        )
                        .await;
                        last_progress_emit = Instant::now();
                    }
                }
            }

            if total.is_some_and(|size| size != transferred) {
                return Err(format!("{remote_path} changed size during download"));
            }
            Ok(())
        }
        .await;
        finish_local_download(local, &staging_path, local_path, copy_result, cancel).await?;
        Ok(local_path.to_string_lossy().to_string())
    }
    .await;

    match result {
        Ok(local_path) => {
            emit_transfer_progress(
                app,
                id,
                session_id,
                queue_id,
                transfer_id,
                "download",
                remote_path,
                display_path,
                transferred,
                total,
                finalize,
                None,
            )
            .await;
            Ok(local_path)
        }
        Err(error) => {
            if finalize {
                emit_transfer_error(
                    app,
                    id,
                    session_id,
                    queue_id,
                    transfer_id,
                    "download",
                    remote_path,
                    display_path,
                    transferred,
                    total,
                    &error,
                )
                .await;
            }
            Err(error)
        }
    }
}

async fn sftp_download_impl(
    app: &AppHandle,
    state: &SshState,
    transfer_state: &SftpTransferState,
    id: &str,
    session_id: i64,
    remote_path: &str,
    local_path: &Path,
) -> Result<String, String> {
    ensure_local_parent(local_path).await?;

    let lookup_id = id.to_string();
    let session_lookup_id = lookup_id.clone();
    let remote_path = remote_path.to_string();
    let local_path = local_path.to_path_buf();
    let queue_id = queue_id("download", session_id);
    let cancel = transfer_state.register_queue(id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let transfer_id = transfer_id_for_path("download", &remote_path);
    let display_path = remote_path.clone();
    let result = with_sftp_session(
        state,
        &session_lookup_id,
        move |sftp, ssh_handle| async move {
            download_remote_file(
                &sftp,
                &ssh_handle,
                app,
                &lookup_id,
                session_id,
                &remote_path,
                &local_path,
                &queue_id,
                &transfer_id,
                &display_path,
                &cancel,
                true,
            )
            .await
        },
    )
    .await;
    transfer_state.finish_queue(id, &finish_queue_id)?;
    result
}

async fn sftp_upload_impl(
    app: &AppHandle,
    state: &SshState,
    transfer_state: &SftpTransferState,
    id: &str,
    session_id: i64,
    remote_path: &str,
    local_path: &Path,
) -> Result<(), String> {
    let ssh_handle = upload_ssh_handle(state, id)?;
    let queue_id = queue_id("upload", session_id);
    let cancel = transfer_state.register_queue(id, &queue_id)?;
    let transfer_id = transfer_id_for_path("upload", remote_path);
    let progress = UploadProgress {
        app,
        id,
        session_id,
        queue_id: &queue_id,
        transfer_id: &transfer_id,
        finalize: true,
    };
    let result = upload_local_file(
        &ssh_handle,
        &progress,
        remote_path,
        local_path,
        remote_path,
        &cancel,
    )
    .await;
    transfer_state.finish_queue(id, &queue_id)?;
    result
}

fn upload_ssh_handle(state: &SshState, id: &str) -> Result<SharedSshHandle, String> {
    state
        .ssh_handles
        .lock()
        .map_err(|e| e.to_string())?
        .get(id)
        .cloned()
        .ok_or_else(|| "SFTP is only available while an SSH session is connected".to_string())
}

async fn await_upload_operation<T, E, F>(
    cancel: &TransferCancelToken,
    context: &str,
    operation: F,
) -> Result<T, String>
where
    E: std::fmt::Display,
    F: Future<Output = Result<T, E>>,
{
    tokio::select! {
        _ = cancel.cancelled() => Err("Transfer canceled".to_string()),
        result = tokio::time::timeout(SFTP_UPLOAD_OPERATION_TIMEOUT, operation) => {
            match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(format!("{context}: {error}")),
                Err(_) => Err(format!(
                    "{context}: timed out after {} seconds",
                    SFTP_UPLOAD_OPERATION_TIMEOUT.as_secs()
                )),
            }
        }
    }
}

async fn remote_upload_destination(
    raw: &RawSftpSession,
    remote_path: &str,
    cancel: &TransferCancelToken,
) -> Result<Option<FileAttributes>, String> {
    await_upload_operation(cancel, &format!("Failed to inspect {remote_path}"), async {
        match raw.lstat(remote_path).await {
            Ok(metadata) => Ok(Some(metadata.attrs)),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    })
    .await
}

async fn commit_remote_upload(
    raw: &RawSftpSession,
    staging_path: &str,
    remote_path: &str,
    replace: bool,
) -> Result<(), String> {
    if replace {
        let payload = russh_sftp::ser::to_bytes(&(staging_path, remote_path))
            .map_err(|e| format!("Failed to encode upload destination: {e}"))?;
        match raw.extended(POSIX_RENAME, payload.to_vec()).await {
            Ok(Packet::Status(status)) if status.status_code == StatusCode::Ok => Ok(()),
            Ok(Packet::Status(status)) => Err(format!(
                "{:?}: {}",
                status.status_code, status.error_message
            )),
            Ok(_) => Err("Unexpected reply while publishing upload".to_string()),
            Err(error) => Err(error.to_string()),
        }
    } else {
        raw.rename(staging_path, remote_path)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

struct UploadProgress<'a> {
    app: &'a AppHandle,
    id: &'a str,
    session_id: i64,
    queue_id: &'a str,
    transfer_id: &'a str,
    finalize: bool,
}

async fn resolve_upload_link(
    raw: &RawSftpSession,
    path: &str,
    cancel: &TransferCancelToken,
) -> Result<String, String> {
    let names = await_upload_operation(
        cancel,
        "Failed to resolve upload symlink",
        raw.realpath(path),
    )
    .await?;
    names
        .files
        .into_iter()
        .next()
        .map(|file| file.filename)
        .ok_or_else(|| format!("Could not resolve the destination of {path}"))
}

impl UploadProgress<'_> {
    async fn emit(
        &self,
        path: &str,
        display_path: &str,
        transferred: u64,
        total: Option<u64>,
        done: bool,
        error: Option<String>,
    ) {
        emit_transfer_progress(
            self.app,
            self.id,
            self.session_id,
            self.queue_id,
            self.transfer_id,
            "upload",
            path,
            display_path,
            transferred,
            total,
            done,
            error,
        )
        .await;
    }
}

async fn write_remote_upload<R: tokio::io::AsyncRead + Unpin>(
    raw: &Arc<RawSftpSession>,
    handle: &str,
    source: &mut R,
    progress: &UploadProgress<'_>,
    remote_path: &str,
    display_path: &str,
    total: Option<u64>,
    transferred: &mut u64,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    let mut writes = FuturesUnordered::new();
    let mut offset = 0_u64;
    let mut eof = false;
    let mut last_emit = Instant::now();
    let context = format!("Failed to write {remote_path}");
    while !eof || !writes.is_empty() {
        while !eof && writes.len() < SFTP_UPLOAD_WRITE_DEPTH {
            let mut chunk = vec![0; SFTP_UPLOAD_CHUNK_SIZE];
            let count = await_upload_operation(
                cancel,
                "Failed to read upload source",
                source.read(&mut chunk),
            )
            .await?;
            if count == 0 {
                eof = true;
                break;
            }
            chunk.truncate(count);
            let request_offset = offset;
            let handle = handle.to_string();
            writes.push(async move {
                raw.write(handle, request_offset, chunk)
                    .await
                    .map(|_| count)
            });
            offset += count as u64;
        }
        if !writes.is_empty() {
            let count = await_upload_operation(cancel, &context, async {
                writes.next().await.expect("write pipeline is nonempty")
            })
            .await?;
            *transferred += count as u64;
            if last_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
                progress
                    .emit(remote_path, display_path, *transferred, total, false, None)
                    .await;
                last_emit = Instant::now();
            }
        }
    }
    Ok(())
}

async fn upload_from_reader<R: tokio::io::AsyncRead + Unpin>(
    ssh_handle: &SharedSshHandle,
    progress: &UploadProgress<'_>,
    remote_path: &str,
    display_path: &str,
    mut source: R,
    total: Option<u64>,
    local_source: Option<(&Path, FileFingerprint)>,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    progress
        .emit(remote_path, display_path, 0, total, false, None)
        .await;
    let mut transferred = 0;
    let result = async {
        let (raw, version) = await_upload_operation(
            cancel, "Failed to initialize SFTP upload session", create_raw_sftp_session(ssh_handle),
        ).await?;
        let mut original = remote_upload_destination(&raw, remote_path, cancel).await?;
        let resolved_link = if original.as_ref().is_some_and(|attrs| attrs.is_symlink()) {
            let target = resolve_upload_link(&raw, remote_path, cancel).await?;
            original = remote_upload_destination(&raw, &target, cancel).await?;
            if original.is_none() {
                return Err(format!("Cannot upload through the dangling symlink {remote_path}"));
            }
            Some(target)
        } else {
            None
        };
        let destination = resolved_link.as_deref().unwrap_or(remote_path);
        if let Some(metadata) = &original {
            if !metadata.file_type().is_file() {
                return Err(format!("Cannot replace {remote_path}: the destination must be a regular file"));
            }
            if version.extensions.get(POSIX_RENAME).map(String::as_str) != Some("1") {
                return Err(format!("Cannot safely replace {remote_path}: this server does not support atomic replacement"));
            }
            let existing = await_upload_operation(
                cancel, &format!("Cannot write to {remote_path}"),
                raw.open(destination, OpenFlags::WRITE, FileAttributes::empty()),
            ).await?;
            await_upload_operation(cancel, "Failed to close existing upload destination", raw.close(existing.handle)).await?;
        }

        let name = transfer_staging_name()?;
        let staging_path = match destination.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{name}"),
            None => name,
        };
        let mut staging_attrs = FileAttributes::empty();
        if original.is_some() {
            staging_attrs.permissions = Some(0o600);
        }
        let handle = await_upload_operation(
            cancel, &format!("Failed to create a temporary file for {remote_path}"),
            raw.open(&staging_path, OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE, staging_attrs),
        ).await?.handle;
        let result = async {
            write_remote_upload(&raw, &handle, &mut source, progress, remote_path, display_path, total, &mut transferred, cancel).await?;
            if let Some((path, initial)) = &local_source {
                if transferred != initial.len || file_fingerprint(path).await.as_ref() != Some(initial) {
                    return Err(format!("{} changed during upload", path.display()));
                }
            }
            if let Some(metadata) = &original {
                let staged = await_upload_operation(cancel, "Failed to inspect temporary upload", raw.fstat(&handle)).await?.attrs;
                let mut attributes = FileAttributes::empty();
                attributes.permissions = metadata.permissions.map(|mode| mode & 0o777);
                if (staged.uid, staged.gid) != (metadata.uid, metadata.gid) {
                    attributes.uid = metadata.uid;
                    attributes.gid = metadata.gid;
                }
                await_upload_operation(
                    cancel, "Failed to preserve destination permissions or ownership",
                    raw.setstat(&staging_path, attributes),
                ).await?;
            }
            Ok::<_, String>(())
        }.await;
        let close_timeout = if result.is_ok() {
            SFTP_UPLOAD_OPERATION_TIMEOUT
        } else {
            Duration::from_secs(2)
        };
        let close = tokio::time::timeout(close_timeout, raw.close(handle)).await;
        let result = async {
            result?;
            close.map_err(|_| "Timed out closing temporary upload".to_string())?
                .map_err(|e| format!("Failed to close temporary upload: {e}"))?;
            if let Some(original) = &original {
                let current = remote_upload_destination(&raw, destination, cancel).await?;
                let fingerprint = |attrs: &FileAttributes| (attrs.size, attrs.mtime, attrs.permissions, attrs.uid, attrs.gid);
                if current.as_ref().map(fingerprint) != Some(fingerprint(original)) {
                    return Err(format!("Cannot replace {remote_path}: the destination changed during upload"));
                }
            }
            if let Some(target) = &resolved_link {
                if resolve_upload_link(&raw, remote_path, cancel).await? != *target {
                    return Err(format!("Cannot replace {remote_path}: its symlink target changed during upload"));
                }
            }
            if cancel.is_cancelled() {
                return Err("Upload canceled".to_string());
            }
            tokio::time::timeout(
                SFTP_UPLOAD_OPERATION_TIMEOUT,
                commit_remote_upload(&raw, &staging_path, destination, original.is_some()),
            ).await
                .map_err(|_| format!("Could not confirm that {remote_path} was saved: the server timed out"))?
                .map_err(|e| format!("Failed to save {remote_path}: {e}"))
        }.await;
        if result.is_err() {
            let _ = tokio::time::timeout(Duration::from_secs(2), raw.remove(&staging_path)).await;
        }
        result
    }.await;
    if progress.finalize || result.is_ok() {
        progress
            .emit(
                remote_path,
                display_path,
                transferred,
                total,
                progress.finalize,
                result.as_ref().err().cloned(),
            )
            .await;
    }
    result
}

async fn upload_local_file(
    ssh_handle: &SharedSshHandle,
    progress: &UploadProgress<'_>,
    remote_path: &str,
    local_path: &Path,
    display_path: &str,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    let metadata = fs::metadata(local_path)
        .await
        .map_err(|e| format!("Failed to inspect {}: {e}", local_path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", local_path.display()));
    }
    let local = fs::File::open(local_path)
        .await
        .map_err(|e| format!("Failed to open {}: {e}", local_path.display()))?;
    let fingerprint = FileFingerprint {
        len: metadata.len(),
        modified_at: metadata.modified().ok(),
    };
    upload_from_reader(
        ssh_handle,
        progress,
        remote_path,
        display_path,
        local,
        Some(metadata.len()),
        Some((local_path, fingerprint)),
        cancel,
    )
    .await
}

struct BatchDownloadProgress<'a> {
    app: &'a AppHandle,
    id: &'a str,
    session_id: i64,
    queue_id: &'a str,
    transfer_id: &'a str,
    last_emit: Instant,
    total_files: u64,
}

impl<'a> BatchDownloadProgress<'a> {
    async fn emit(
        &mut self,
        path: &str,
        display_path: &str,
        transferred: u64,
        total: Option<u64>,
        done: bool,
        error: Option<String>,
    ) {
        emit_transfer_progress(
            self.app,
            self.id,
            self.session_id,
            self.queue_id,
            self.transfer_id,
            "download",
            path,
            display_path,
            transferred,
            total,
            done,
            error,
        )
        .await;
        self.last_emit = Instant::now();
    }

    async fn maybe_emit(
        &mut self,
        path: &str,
        display_path: &str,
        transferred: u64,
        total: Option<u64>,
    ) {
        if self.last_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
            self.emit(path, display_path, transferred, total, false, None)
                .await;
        }
    }

    fn batch_complete_display(&self) -> String {
        if self.total_files == 0 {
            "Download complete".to_string()
        } else if self.total_files == 1 {
            "1 file downloaded".to_string()
        } else {
            format!("{} files downloaded", self.total_files)
        }
    }
}

async fn download_remote_file_in_batch(
    sftp: &SftpSession,
    ssh_handle: &SharedSshHandle,
    progress: &mut BatchDownloadProgress<'_>,
    remote_path: &str,
    local_path: &Path,
    listed_metadata: Option<FileAttributes>,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    ensure_local_parent(local_path).await?;
    let display_name = remote_display_name(remote_path);
    if cancel.is_cancelled() {
        return Err("Download canceled".to_string());
    }
    let metadata = match listed_metadata {
        Some(metadata) => metadata,
        None => {
            await_download_operation(
                cancel,
                &format!("Failed to inspect {remote_path}"),
                sftp.metadata(remote_path),
            )
            .await?
        }
    };
    if metadata.file_type().is_dir() {
        return Err("Cannot download a directory link inside a folder".to_string());
    }
    let total = metadata.size;
    let file_size = metadata.len();

    progress
        .emit(remote_path, &display_name, 0, total, false, None)
        .await;

    let (mut local, staging_path) = create_local_download(local_path).await?;
    let mut file_transferred = 0_u64;
    let result: Result<(), String> = async {
        if file_size >= PIPELINED_DOWNLOAD_MIN_SIZE {
            download_remote_file_pipelined(
                ssh_handle,
                progress.app,
                progress.id,
                progress.session_id,
                remote_path,
                local_path,
                &mut local,
                progress.queue_id,
                progress.transfer_id,
                &display_name,
                cancel,
                file_size,
                &mut file_transferred,
                &mut progress.last_emit,
            )
            .await?;
        } else {
            let mut remote = await_download_operation(
                cancel,
                &format!("Failed to open {remote_path}"),
                sftp.open(remote_path),
            )
            .await?;
            let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
            loop {
                if cancel.is_cancelled() {
                    return Err("Download canceled".to_string());
                }
                let count = tokio::select! {
                    _ = cancel.cancelled() => return Err("Download canceled".to_string()),
                    read = remote.read(&mut buffer) => {
                        read.map_err(|e| format!("Failed to read {}: {}", remote_path, e))?
                    },
                };
                if count == 0 {
                    break;
                }

                local
                    .write_all(&buffer[..count])
                    .await
                    .map_err(|e| format!("Failed to write {}: {}", local_path.display(), e))?;
                file_transferred = file_transferred.saturating_add(count as u64);
                progress
                    .maybe_emit(remote_path, &display_name, file_transferred, total)
                    .await;
            }
        }

        if total.is_some_and(|size| size != file_transferred) {
            return Err(format!("{remote_path} changed size during download"));
        }
        Ok(())
    }
    .await;

    finish_local_download(local, &staging_path, local_path, result, cancel).await?;
    progress.total_files = progress.total_files.saturating_add(1);

    progress
        .maybe_emit(remote_path, &display_name, file_transferred, total)
        .await;

    Ok(())
}

async fn download_remote_tree_streaming(
    sftp: &SftpSession,
    ssh_handle: &SharedSshHandle,
    progress: &mut BatchDownloadProgress<'_>,
    remote_path: &str,
    local_path: &Path,
    listed_metadata: Option<FileAttributes>,
    failures: &mut Vec<String>,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("Download canceled".to_string());
    }

    let metadata = match listed_metadata {
        Some(metadata) => metadata,
        None => match await_download_operation(
            cancel,
            "Failed to inspect item",
            sftp.metadata(remote_path),
        )
        .await
        {
            Ok(metadata) => metadata,
            Err(_) if cancel.is_cancelled() => return Err("Download canceled".to_string()),
            Err(error) => {
                failures.push(format!("{remote_path} ({error})"));
                return Ok(());
            }
        },
    };

    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        if let Err(error) = download_remote_file_in_batch(
            sftp,
            ssh_handle,
            progress,
            remote_path,
            local_path,
            Some(metadata),
            cancel,
        )
        .await
        {
            if error == "Download canceled" {
                return Err(error);
            }
            failures.push(format!("{} ({})", remote_path, error));
            return Ok(());
        }
        return Ok(());
    }

    if let Err(error) = fs::create_dir_all(local_path).await {
        failures.push(format!(
            "{} (Failed to create local directory: {})",
            remote_path, error
        ));
        return Ok(());
    }

    let folder_name = remote_display_name(remote_path);
    progress
        .emit(
            remote_path,
            &format!("scanning {folder_name}"),
            0,
            None,
            false,
            None,
        )
        .await;
    let entries = match await_download_operation(
        cancel,
        "Failed to list directory",
        sftp.read_dir(remote_path),
    )
    .await
    {
        Ok(e) => e,
        Err(_) if cancel.is_cancelled() => return Err("Download canceled".to_string()),
        Err(e) => {
            failures.push(format!("{} ({})", remote_path, e));
            return Ok(());
        }
    };
    let mut local_names = HashSet::new();
    for entry in entries {
        if cancel.is_cancelled() {
            return Err("Download canceled".to_string());
        }

        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let local_name = sanitize_local_name(&name);
        if !local_names.insert(local_name_collision_key(&local_name)) {
            failures.push(format!(
                "{} (local filename collides with another item after sanitization)",
                entry.path()
            ));
            continue;
        }
        let child_local_path = local_path.join(local_name);
        let child_remote_path = entry.path();
        let child_file_type = entry.file_type();

        if child_file_type.is_dir() && !child_file_type.is_symlink() {
            Box::pin(download_remote_tree_streaming(
                sftp,
                ssh_handle,
                progress,
                &child_remote_path,
                &child_local_path,
                Some(entry.metadata()),
                failures,
                cancel,
            ))
            .await?;
        } else if let Err(error) = download_remote_file_in_batch(
            sftp,
            ssh_handle,
            progress,
            &child_remote_path,
            &child_local_path,
            (!child_file_type.is_symlink()).then(|| entry.metadata()),
            cancel,
        )
        .await
        {
            if error == "Download canceled" {
                return Err(error);
            }
            failures.push(format!("{} ({})", child_remote_path, error));
        }
    }

    Ok(())
}

fn format_batch_download_error(failures: &[String]) -> String {
    match failures {
        [] => "Failed to download selection".to_string(),
        [single] => format!("Failed to download: {single}"),
        _ => format!(
            "Failed to download {} items: {}",
            failures.len(),
            failures.join("; ")
        ),
    }
}

fn spawn_edit_watcher(
    app: AppHandle,
    terminal_id: String,
    session_id: i64,
    remote_path: String,
    local_path: PathBuf,
    mut last_uploaded: Option<FileFingerprint>,
    generation: u64,
    mut stop_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut pending: Option<(FileFingerprint, Instant)> = None;
        let mut last_failed: Option<FileFingerprint> = None;

        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                _ = tokio::time::sleep(EDIT_WATCH_POLL_INTERVAL) => {}
            }

            let Some(current) = file_fingerprint(&local_path).await else {
                pending = None;
                continue;
            };

            if last_uploaded.as_ref() == Some(&current) {
                pending = None;
                last_failed = None;
                continue;
            }

            let should_reset_pending = pending
                .as_ref()
                .map(|(fingerprint, _)| fingerprint != &current)
                .unwrap_or(true);

            if should_reset_pending {
                pending = Some((current.clone(), Instant::now()));
            }

            let should_upload = pending
                .as_ref()
                .map(|(fingerprint, changed_at)| {
                    fingerprint == &current && changed_at.elapsed() >= EDIT_WATCH_DEBOUNCE
                })
                .unwrap_or(false);

            if !should_upload {
                continue;
            }

            if last_failed.as_ref() == Some(&current) {
                continue;
            }

            let upload_result = {
                let ssh_state = app.state::<SshState>();
                let transfer_state = app.state::<SftpTransferState>();
                sftp_upload_impl(
                    &app,
                    ssh_state.inner(),
                    transfer_state.inner(),
                    &terminal_id,
                    session_id,
                    &remote_path,
                    &local_path,
                )
                .await
            };

            match upload_result {
                Ok(()) => {
                    last_uploaded = Some(current);
                    last_failed = None;
                    pending = None;
                }
                Err(error) if error == "Transfer canceled" || error == "Upload canceled" => {
                    last_failed = Some(current);
                    pending = None;
                }
                Err(error) => {
                    emit_edit_upload_error(
                        &app,
                        &terminal_id,
                        session_id,
                        &remote_path,
                        &local_path,
                        &error,
                    )
                    .await;
                    last_failed = Some(current);
                    pending = None;
                }
            }
        }

        let _ = app.state::<SftpEditState>().remove_watcher_if_current(
            &terminal_id,
            &remote_path,
            generation,
        );
    });
}

#[tauri::command]
pub async fn sftp_list_dir(
    id: String,
    path: String,
    state: tauri::State<'_, SshState>,
) -> Result<DirectoryListing, String> {
    let path = normalize_remote_path(&path);
    let lookup_id = id.clone();
    with_sftp_session(state.inner(), &lookup_id, move |sftp, _| async move {
        let canonical_path =
            if path.starts_with('/') && !path.split('/').any(|s| s == "." || s == "..") {
                path.clone()
            } else {
                sftp.canonicalize(&path)
                    .await
                    .map_err(|e| format!("Failed to resolve directory {}: {}", path, e))?
            };
        let dir_entries = sftp
            .read_dir(&canonical_path)
            .await
            .map_err(|e| format!("Failed to read directory {}: {}", canonical_path, e))?;

        let mut entries = futures::stream::iter(dir_entries.into_iter().filter_map(|entry| {
            let name = entry.file_name();
            (name != "." && name != "..").then_some((name, entry))
        }))
        .map(|(name, entry)| {
            let sftp = &sftp;
            async move {
                let metadata = entry.metadata();
                let file_type = entry.file_type();
                let path = entry.path();
                let is_symlink = file_type.is_symlink();
                let is_dir = file_type.is_dir()
                    || (is_symlink
                        && sftp
                            .metadata(&path)
                            .await
                            .map(|target_metadata| target_metadata.file_type().is_dir())
                            .unwrap_or(false));
                FileEntry {
                    path,
                    name,
                    is_dir,
                    is_symlink,
                    size: metadata.size,
                    modified: metadata.modified().ok().and_then(|time| {
                        time.duration_since(UNIX_EPOCH)
                            .ok()
                            .map(|duration| duration.as_secs())
                    }),
                }
            }
        })
        .buffer_unordered(16)
        .collect::<Vec<_>>()
        .await;

        entries.sort_by_cached_key(|entry| {
            (!entry.is_dir, entry.name.to_lowercase(), entry.name.clone())
        });

        Ok(DirectoryListing {
            path: canonical_path,
            entries,
        })
    })
    .await
}

#[tauri::command]
pub async fn sftp_download(
    app: AppHandle,
    id: String,
    remote_path: String,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<String, String> {
    let remote_path = normalize_remote_path(&remote_path);
    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let local_path = local_open_download_path(&id, &remote_path);
    sftp_download_impl(
        &app,
        state.inner(),
        transfer_state.inner(),
        &id,
        session_id,
        &remote_path,
        &local_path,
    )
    .await
}

#[tauri::command]
pub async fn sftp_download_selection(
    app: AppHandle,
    id: String,
    remote_paths: Vec<String>,
    destination_dir: String,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    if remote_paths.is_empty() {
        return Ok(());
    }

    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let destination_dir = PathBuf::from(destination_dir);
    let normalized_paths = remote_paths
        .into_iter()
        .map(|path| normalize_remote_path(&path))
        .collect::<Vec<_>>();
    let mut local_names = HashSet::new();
    for remote_path in &normalized_paths {
        let local_name = remote_display_name(remote_path);
        if !local_names.insert(local_name_collision_key(&local_name)) {
            return Err(format!(
                "Cannot download selection: multiple items map to the local name {}",
                local_name
            ));
        }
    }
    let transfer_id = batch_transfer_id(session_id);
    let queue_id = transfer_id.clone();
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let lookup_id = id.clone();
    let session_lookup_id = lookup_id.clone();

    let result = with_sftp_session(
        state.inner(),
        &session_lookup_id,
        move |sftp, ssh_handle| async move {
            let mut failures = Vec::new();
            let mut progress = BatchDownloadProgress {
                app: &app,
                id: &lookup_id,
                session_id,
                queue_id: &queue_id,
                transfer_id: &transfer_id,
                last_emit: Instant::now(),
                total_files: 0,
            };

            progress
                .emit("", "preparing files", 0, None, false, None)
                .await;

            for remote_path in &normalized_paths {
                let local_path = destination_dir.join(remote_display_name(remote_path));
                if let Err(error) = download_remote_tree_streaming(
                    &sftp,
                    &ssh_handle,
                    &mut progress,
                    remote_path,
                    &local_path,
                    None,
                    &mut failures,
                    &cancel,
                )
                .await
                {
                    progress
                        .emit("", "Download canceled", 0, None, true, Some(error.clone()))
                        .await;
                    return Err(error);
                }
            }

            let display_path = progress.batch_complete_display();

            progress
                .emit(
                    "",
                    &display_path,
                    0,
                    None,
                    true,
                    if failures.is_empty() {
                        None
                    } else {
                        Some(format_batch_download_error(&failures))
                    },
                )
                .await;

            match failures.is_empty() {
                true => Ok(()),
                false => Err(format_batch_download_error(&failures)),
            }
        },
    )
    .await;
    transfer_state.finish_queue(&id, &finish_queue_id)?;
    result
}

#[tauri::command]
pub async fn sftp_edit(
    app: AppHandle,
    id: String,
    remote_path: String,
    with: Option<String>,
    state: tauri::State<'_, SshState>,
    edit_state: tauri::State<'_, SftpEditState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<String, String> {
    let remote_path = normalize_remote_path(&remote_path);
    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let local_path = local_download_path(&id, &remote_path)?;

    let local_path_string = sftp_download_impl(
        &app,
        state.inner(),
        transfer_state.inner(),
        &id,
        session_id,
        &remote_path,
        &local_path,
    )
    .await?;

    let fingerprint = file_fingerprint(&local_path).await;
    app.opener()
        .open_path(
            &local_path_string,
            with.as_deref().filter(|value| !value.is_empty()),
        )
        .map_err(|e| e.to_string())?;

    let (generation, stop_rx) = edit_state.replace_watcher(&id, &remote_path)?;
    spawn_edit_watcher(
        app,
        id,
        session_id,
        remote_path,
        local_path,
        fingerprint,
        generation,
        stop_rx,
    );

    Ok(local_path_string)
}

#[tauri::command]
pub async fn sftp_upload(
    app: AppHandle,
    id: String,
    remote_path: String,
    local_path: String,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    let remote_path = normalize_remote_path(&remote_path);
    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let local_path_buf = PathBuf::from(local_path);
    sftp_upload_impl(
        &app,
        state.inner(),
        transfer_state.inner(),
        &id,
        session_id,
        &remote_path,
        &local_path_buf,
    )
    .await
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSelectionEntry {
    remote_path: String,
    local_path: String,
}

fn format_batch_upload_error(failures: &[String]) -> String {
    match failures {
        [] => "Failed to upload selection".to_string(),
        [single] => format!("Failed to upload: {single}"),
        _ => format!(
            "Failed to upload {} files: {}",
            failures.len(),
            failures.join("; ")
        ),
    }
}

#[tauri::command]
pub async fn sftp_upload_selection(
    app: AppHandle,
    id: String,
    entries: Vec<UploadSelectionEntry>,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }

    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let normalized = entries
        .into_iter()
        .map(|entry| UploadSelectionEntry {
            remote_path: normalize_remote_path(&entry.remote_path),
            local_path: entry.local_path,
        })
        .collect::<Vec<_>>();
    let mut remote_paths = HashSet::new();
    for entry in &normalized {
        if !remote_paths.insert(entry.remote_path.as_str()) {
            return Err(format!(
                "Cannot upload selection: multiple files target {}",
                entry.remote_path
            ));
        }
    }

    let ssh_handle = upload_ssh_handle(state.inner(), &id)?;
    let queue_id = queue_id("upload-batch", session_id);
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let progress = UploadProgress {
        app: &app,
        id: &id,
        session_id,
        queue_id: &queue_id,
        transfer_id: &queue_id,
        finalize: false,
    };
    let mut uploaded = 0;
    progress
        .emit("", "Preparing files", 0, None, false, None)
        .await;
    let result = async {
        let mut failures = Vec::new();

        for entry in &normalized {
            if cancel.is_cancelled() {
                return Err("Upload canceled".to_string());
            }
            let local_path = PathBuf::from(&entry.local_path);
            let display_path = remote_display_name(&entry.remote_path);

            if let Err(error) = upload_local_file(
                &ssh_handle,
                &progress,
                &entry.remote_path,
                &local_path,
                &display_path,
                &cancel,
            )
            .await
            {
                if error == "Upload canceled" || error == "Transfer canceled" {
                    return Err("Upload canceled".to_string());
                }
                failures.push(format!("{} ({})", entry.local_path, error));
            } else {
                uploaded += 1;
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(format_batch_upload_error(&failures))
        }
    }
    .await;
    let summary = format!("{uploaded} of {} files uploaded", normalized.len());
    progress
        .emit("", &summary, 0, None, true, result.as_ref().err().cloned())
        .await;
    transfer_state.finish_queue(&id, &queue_id)?;
    result
}

#[tauri::command]
pub async fn sftp_upload_bytes(
    app: AppHandle,
    id: String,
    remote_path: String,
    data: Vec<u8>,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    let remote_path = normalize_remote_path(&remote_path);
    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let ssh_handle = upload_ssh_handle(state.inner(), &id)?;
    let total = Some(data.len() as u64);
    let queue_id = queue_id("upload-bytes", session_id);
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let transfer_id = transfer_id_for_path("upload", &remote_path);
    let progress = UploadProgress {
        app: &app,
        id: &id,
        session_id,
        queue_id: &queue_id,
        transfer_id: &transfer_id,
        finalize: true,
    };
    let result = upload_from_reader(
        &ssh_handle,
        &progress,
        &remote_path,
        &remote_path,
        std::io::Cursor::new(data),
        total,
        None,
        &cancel,
    )
    .await;
    transfer_state.finish_queue(&id, &queue_id)?;
    result
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteProgressEvent {
    terminal_id: String,
    queue_id: String,
    deleted: u64,
    total: u64,
    done: bool,
    error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSelectionEntry {
    path: String,
    is_dir: bool,
}

struct DeleteProgressState<'a> {
    app: &'a AppHandle,
    terminal_id: &'a str,
    queue_id: &'a str,
    deleted: u64,
    total: u64,
    last_emit: Instant,
}

impl<'a> DeleteProgressState<'a> {
    async fn tick(&mut self) {
        self.deleted += 1;
        if self.last_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
            self.emit(false, None).await;
            self.last_emit = Instant::now();
        }
    }

    async fn emit(&self, done: bool, error: Option<String>) {
        let _ = self.app.emit(
            "sftp-delete-progress",
            DeleteProgressEvent {
                terminal_id: self.terminal_id.to_string(),
                queue_id: self.queue_id.to_string(),
                deleted: self.deleted,
                total: self.total,
                done,
                error,
            },
        );
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn validate_delete_path(path: &str) -> Result<(), String> {
    if path == "/" {
        return Err("Refusing to delete the remote root directory".to_string());
    }

    match path.trim_end_matches('/') {
        "" | "." => Err("Refusing to delete the current directory".to_string()),
        _ => Ok(()),
    }
}

async fn exec_ssh_command(handle: &SharedSshHandle, command: &str) -> Result<(), String> {
    let mut channel = {
        let h = handle.lock().await;
        h.channel_open_session()
            .await
            .map_err(|e| format!("Failed to open SSH channel: {}", e))?
    };

    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    let mut exit_status = None;
    let mut stderr_output = String::new();

    loop {
        match channel.wait().await {
            Some(ChannelMsg::ExitStatus { exit_status: code }) => {
                exit_status = Some(code);
            }
            Some(ChannelMsg::ExtendedData { ref data, ext }) => {
                if ext == 1 {
                    stderr_output.push_str(&String::from_utf8_lossy(data));
                }
            }
            // exit-status typically arrives after EOF; keep draining until the
            // channel closes so failed commands are not reported as success.
            Some(ChannelMsg::Eof) => {}
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }

    match exit_status {
        Some(0) => Ok(()),
        None => Err("SSH closed before confirming the result of the delete".to_string()),
        Some(code) => {
            let msg = if stderr_output.trim().is_empty() {
                format!("Command exited with code {}", code)
            } else {
                stderr_output.trim().to_string()
            };
            Err(msg)
        }
    }
}

#[tauri::command]
pub async fn sftp_delete(
    id: String,
    path: String,
    is_dir: bool,
    state: tauri::State<'_, SshState>,
) -> Result<(), String> {
    let path = normalize_remote_path(&path);
    validate_delete_path(&path)?;
    with_sftp_session(state.inner(), &id, move |sftp, ssh_handle| async move {
        if is_dir {
            let command = format!("rm -rf -- {}", shell_escape(&path));
            exec_ssh_command(&ssh_handle, &command).await
        } else {
            sftp.remove_file(&path)
                .await
                .map_err(|e| format!("Failed to delete file {}: {}", path, e))
        }
    })
    .await
}

#[tauri::command]
pub async fn sftp_rename(
    id: String,
    old_path: String,
    new_path: String,
    state: tauri::State<'_, SshState>,
) -> Result<(), String> {
    let old_path = normalize_remote_path(&old_path);
    let new_path = normalize_remote_path(&new_path);
    with_sftp_session(state.inner(), &id, move |sftp, _| async move {
        sftp.rename(&old_path, &new_path)
            .await
            .map_err(|e| format!("Failed to rename {} to {}: {}", old_path, new_path, e))
    })
    .await
}

#[tauri::command]
pub async fn sftp_create_dir(
    id: String,
    path: String,
    state: tauri::State<'_, SshState>,
) -> Result<(), String> {
    let path = normalize_remote_path(&path);
    with_sftp_session(state.inner(), &id, move |sftp, _| async move {
        sftp.create_dir(&path)
            .await
            .map_err(|e| format!("Failed to create directory {}: {}", path, e))
    })
    .await
}

#[tauri::command]
pub async fn sftp_delete_selection(
    app: AppHandle,
    id: String,
    entries: Vec<DeleteSelectionEntry>,
    state: tauri::State<'_, SshState>,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }

    let normalized: Vec<DeleteSelectionEntry> = entries
        .into_iter()
        .map(|entry| DeleteSelectionEntry {
            path: normalize_remote_path(&entry.path),
            is_dir: entry.is_dir,
        })
        .collect();
    for entry in &normalized {
        validate_delete_path(&entry.path)?;
    }

    let session_id = session_id_for_terminal(state.inner(), &id)?;
    let lookup_id = id.clone();
    let progress_terminal_id = id.clone();
    let queue_id = queue_id("delete-batch", session_id);
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let result = with_sftp_session(
        state.inner(),
        &lookup_id,
        move |sftp, ssh_handle| async move {
            let total = normalized.len() as u64;
            let mut progress = DeleteProgressState {
                app: &app,
                terminal_id: &progress_terminal_id,
                queue_id: &queue_id,
                deleted: 0,
                total,
                last_emit: Instant::now(),
            };

            progress.emit(false, None).await;

            let mut failures = Vec::new();
            for entry in &normalized {
                if cancel.is_cancelled() {
                    let error = "Delete canceled".to_string();
                    progress.emit(true, Some(error.clone())).await;
                    return Err(error);
                }

                let result = if entry.is_dir {
                    let command = format!("rm -rf -- {}", shell_escape(&entry.path));
                    exec_ssh_command(&ssh_handle, &command).await
                } else {
                    sftp.remove_file(&entry.path)
                        .await
                        .map_err(|e| format!("Failed to delete file {}: {}", entry.path, e))
                };

                match result {
                    Ok(()) => progress.tick().await,
                    Err(error) => failures.push(format!("{} ({error})", entry.path)),
                }
            }

            if failures.is_empty() {
                progress.emit(true, None).await;
                Ok(())
            } else {
                let error = if failures.len() == 1 {
                    failures.into_iter().next().unwrap()
                } else {
                    format!(
                        "Failed to delete {} items: {}",
                        failures.len(),
                        failures.join("; ")
                    )
                };
                progress.emit(true, Some(error.clone())).await;
                Err(error)
            }
        },
    )
    .await;
    transfer_state.finish_queue(&id, &finish_queue_id)?;
    result
}

#[tauri::command]
pub fn cancel_sftp_queue(
    id: String,
    queue_id: String,
    transfer_state: tauri::State<'_, SftpTransferState>,
) -> Result<(), String> {
    match transfer_state.cancel_queue(&id, &queue_id)? {
        true => Ok(()),
        false => Err("Transfer is no longer active".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_publishes_only_a_successful_copy() {
        let directory = std::env::temp_dir().join(transfer_staging_name().unwrap());
        fs::create_dir(&directory).await.unwrap();
        let destination = directory.join("existing.txt");
        fs::write(&destination, b"original").await.unwrap();

        let (mut staged_file, staging_path) = create_local_download(&destination).await.unwrap();
        staged_file.write_all(b"partial").await.unwrap();
        let cancel = TransferCancelToken::new();
        cancel.cancel();
        let result =
            finish_local_download(staged_file, &staging_path, &destination, Ok(()), &cancel).await;
        assert_eq!(result.unwrap_err(), "Download canceled");
        assert_eq!(fs::read(&destination).await.unwrap(), b"original");
        assert!(!staging_path.exists());

        let (mut staged_file, staging_path) = create_local_download(&destination).await.unwrap();
        staged_file.write_all(b"failed").await.unwrap();
        let cancel = TransferCancelToken::new();
        let result = finish_local_download(
            staged_file,
            &staging_path,
            &destination,
            Err("read failed".to_string()),
            &cancel,
        )
        .await;
        assert_eq!(result.unwrap_err(), "read failed");
        assert_eq!(fs::read(&destination).await.unwrap(), b"original");
        assert!(!staging_path.exists());

        let (mut staged_file, staging_path) = create_local_download(&destination).await.unwrap();
        staged_file.write_all(b"complete").await.unwrap();
        finish_local_download(staged_file, &staging_path, &destination, Ok(()), &cancel)
            .await
            .unwrap();
        assert_eq!(fs::read(&destination).await.unwrap(), b"complete");
        assert!(!staging_path.exists());

        fs::remove_file(destination).await.unwrap();
        fs::remove_dir(directory).await.unwrap();
    }

    #[test]
    fn remote_paths_and_edit_copies_keep_distinct_file_identities() {
        assert_eq!(normalize_remote_path(r"/first\second"), r"/first\second");
        let first = local_download_path("terminal", "/dir/a:b").unwrap();
        let second = local_download_path("terminal", "/dir/a?b").unwrap();
        let reopened = local_download_path("terminal", "/dir/a:b").unwrap();
        assert_eq!(first.file_name(), second.file_name());
        assert_ne!(first, second);
        assert_ne!(first, reopened);
    }

    #[tokio::test]
    async fn replacement_uses_the_atomic_rename_protocol() {
        use russh_sftp::{protocol::Status, server::Handler};

        struct RenameServer;

        impl Handler for RenameServer {
            type Error = StatusCode;

            fn unimplemented(&self) -> Self::Error {
                StatusCode::OpUnsupported
            }

            async fn extended(
                &mut self,
                id: u32,
                request: String,
                data: Vec<u8>,
            ) -> Result<Packet, Self::Error> {
                assert_eq!(request, POSIX_RENAME);
                assert_eq!(data, b"\0\0\0\x05a.tmp\0\0\0\x05a.txt");
                Ok(Packet::Status(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: String::new(),
                    language_tag: String::new(),
                }))
            }
        }

        let (client, server) = tokio::io::duplex(4096);
        russh_sftp::server::run(server, RenameServer).await;
        let raw = RawSftpSession::new_with_config(client, sftp_config());
        tokio::time::timeout(Duration::from_secs(5), async {
            raw.init().await.unwrap();
            commit_remote_upload(&raw, "a.tmp", "a.txt", true)
                .await
                .unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn pending_upload_operation_stops_when_cancelled() {
        let cancel = TransferCancelToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.cancel();
        });

        let pending = std::future::pending::<Result<(), String>>();
        let result = await_upload_operation(&cancel, "pending upload", pending).await;

        assert_eq!(result, Err("Transfer canceled".to_string()));
    }
}
