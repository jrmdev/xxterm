use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{stream::FuturesUnordered, StreamExt};
use russh::ChannelMsg;
use russh_sftp::client::{Config as SftpConfig, RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags};
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
const SFTP_REQUEST_TIMEOUT_SECS: u64 = 300;
const PIPELINED_DOWNLOAD_MIN_SIZE: u64 = 8 * 1024 * 1024;
const PIPELINED_READ_SIZE: u64 = 16 * 1024;
const PIPELINED_READ_DEPTH: usize = 64;

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

    pub fn stop_watcher(&self, terminal_id: &str, remote_path: &str) -> Result<(), String> {
        let removed = {
            let mut watchers = self.watchers.lock().map_err(|e| e.to_string())?;
            let Some(terminal_watchers) = watchers.get_mut(terminal_id) else {
                return Ok(());
            };
            let removed = terminal_watchers.remove(remote_path);
            if terminal_watchers.is_empty() {
                watchers.remove(terminal_id);
            }
            removed
        };

        if let Some(handle) = removed {
            let _ = handle.stop_tx.send(true);
        }

        Ok(())
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
) -> Result<Arc<RawSftpSession>, String> {
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
    session
        .init()
        .await
        .map_err(|e| format!("Failed to initialize SFTP session: {}", e))?;
    Ok(Arc::new(session))
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
        path.replace('\\', "/")
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

fn local_download_path(id: &str, remote_path: &str) -> PathBuf {
    let mut local_path = local_download_dir(id);
    let mut segments = remote_path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .peekable();

    if segments.peek().is_none() {
        return local_path.join("root");
    }

    for segment in segments {
        let sanitized = if segment == ".." {
            "__parent__".to_string()
        } else {
            sanitize_local_name(segment)
        };
        local_path.push(sanitized);
    }

    local_path
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
    let raw = create_raw_sftp_session(ssh_handle).await?;
    let handle = raw
        .open(remote_path, OpenFlags::READ, FileAttributes::empty())
        .await
        .map_err(|e| format!("Failed to open {}: {}", remote_path, e))?
        .handle;

    let mut reads = FuturesUnordered::new();
    let mut pending = BTreeMap::<u64, Vec<u8>>::new();
    let mut next_request_offset = 0_u64;
    let mut next_write_offset = 0_u64;

    let result: Result<(), String> = async {
        while next_write_offset < file_size {
            if cancel.is_cancelled() {
                return Err("Transfer canceled".to_string());
            }

            while reads.len() < PIPELINED_READ_DEPTH && next_request_offset < file_size {
                let len = (file_size - next_request_offset).min(PIPELINED_READ_SIZE) as usize;
                queue_pipelined_read(&mut reads, &raw, &handle, next_request_offset, len);
                next_request_offset += len as u64;
            }

            let chunk = reads
                .next()
                .await
                .ok_or_else(|| "SFTP read pipeline stalled".to_string())??;

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

    let close_result = raw
        .close(handle)
        .await
        .map_err(|e| format!("Failed to close {}: {}", remote_path, e));
    let _ = raw.close_session();

    result?;
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

    let metadata = sftp
        .metadata(remote_path)
        .await
        .map_err(|e| format!("Failed to inspect {}: {}", remote_path, e))?;

    let file_size = metadata.len();
    let total = Some(file_size);
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
        let mut local = fs::File::create(local_path)
            .await
            .map_err(|e| format!("Failed to create {}: {}", local_path.display(), e))?;

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
            let mut remote = sftp
                .open(remote_path)
                .await
                .map_err(|e| format!("Failed to open {}: {}", remote_path, e))?;
            let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
            loop {
                if cancel.is_cancelled() {
                    return Err("Transfer canceled".to_string());
                }
                let count = remote
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read {}: {}", remote_path, e))?;
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

        local
            .flush()
            .await
            .map_err(|e| format!("Failed to flush {}: {}", local_path.display(), e))?;
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
            let _ = fs::remove_file(local_path).await;
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
    let lookup_id = id.to_string();
    let session_lookup_id = lookup_id.clone();
    let remote_path = remote_path.to_string();
    let local_path = local_path.to_path_buf();
    let queue_id = queue_id("upload", session_id);
    let cancel = transfer_state.register_queue(id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let transfer_id = transfer_id_for_path("upload", &remote_path);
    let display_path = remote_path.clone();
    let result = with_sftp_session(state, &session_lookup_id, move |_, ssh_handle| async move {
        upload_local_file(
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
        )
        .await
    })
    .await;
    transfer_state.finish_queue(id, &finish_queue_id)?;
    result
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

async fn upload_local_file(
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
) -> Result<(), String> {
    let local_meta = fs::metadata(local_path)
        .await
        .map_err(|e| format!("Failed to inspect {}: {}", local_path.display(), e))?;
    if cancel.is_cancelled() {
        return Err("Transfer canceled".to_string());
    }
    let total = Some(local_meta.len());

    emit_transfer_progress(
        app,
        id,
        session_id,
        queue_id,
        transfer_id,
        "upload",
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
    let result: Result<(), String> = async {
        let mut local = fs::File::open(local_path)
            .await
            .map_err(|e| format!("Failed to open {}: {}", local_path.display(), e))?;
        let raw = await_upload_operation(
            cancel,
            "Failed to initialize SFTP upload session",
            create_raw_sftp_session(ssh_handle),
        )
        .await?;
        let handle = await_upload_operation(
            cancel,
            &format!("Failed to open {} for upload", remote_path),
            raw.open(
                remote_path,
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                FileAttributes::empty(),
            ),
        )
        .await?
        .handle;

        let upload_result: Result<(), String> = async {
            let mut buffer = vec![0_u8; SFTP_UPLOAD_CHUNK_SIZE];
            loop {
                if cancel.is_cancelled() {
                    return Err("Transfer canceled".to_string());
                }
                let count = local
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read {}: {}", local_path.display(), e))?;
                if count == 0 {
                    break;
                }
                await_upload_operation(
                    cancel,
                    &format!("Failed to write {}", remote_path),
                    raw.write(handle.clone(), transferred, buffer[..count].to_vec()),
                )
                .await?;
                transferred += count as u64;
                if last_progress_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
                    emit_transfer_progress(
                        app,
                        id,
                        session_id,
                        queue_id,
                        transfer_id,
                        "upload",
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

            await_upload_operation(
                cancel,
                &format!("Failed to close {} after upload", remote_path),
                raw.close(handle),
            )
            .await?;
            Ok(())
        }
        .await;

        let _ = raw.close_session();
        upload_result
    }
    .await;

    match result {
        Ok(()) => {
            emit_transfer_progress(
                app,
                id,
                session_id,
                queue_id,
                transfer_id,
                "upload",
                remote_path,
                display_path,
                transferred,
                total,
                true,
                None,
            )
            .await;
            Ok(())
        }
        Err(error) => {
            emit_transfer_error(
                app,
                id,
                session_id,
                queue_id,
                transfer_id,
                "upload",
                remote_path,
                display_path,
                transferred,
                total,
                &error,
            )
            .await;
            Err(error)
        }
    }
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

    async fn maybe_emit(&mut self, path: &str, display_path: &str, transferred: u64, total: u64) {
        if self.last_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
            self.emit(path, display_path, transferred, Some(total), false, None)
                .await;
        }
    }

    fn batch_complete_display(&self) -> String {
        if self.total_files == 0 {
            "No files".to_string()
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
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    ensure_local_parent(local_path).await?;
    progress.total_files = progress.total_files.saturating_add(1);
    let display_name = remote_display_name(remote_path);
    if cancel.is_cancelled() {
        return Err("Download canceled".to_string());
    }
    let file_size = sftp
        .metadata(remote_path)
        .await
        .map_err(|e| format!("Failed to inspect {}: {}", remote_path, e))?
        .len();

    progress
        .emit(remote_path, &display_name, 0, Some(file_size), false, None)
        .await;

    let result: Result<(), String> = async {
        let mut local = fs::File::create(local_path)
            .await
            .map_err(|e| format!("Failed to create {}: {}", local_path.display(), e))?;

        let mut file_transferred = 0_u64;
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
            let mut remote = sftp
                .open(remote_path)
                .await
                .map_err(|e| format!("Failed to open {}: {}", remote_path, e))?;
            let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
            loop {
                if cancel.is_cancelled() {
                    return Err("Download canceled".to_string());
                }
                let count = remote
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read {}: {}", remote_path, e))?;
                if count == 0 {
                    break;
                }

                local
                    .write_all(&buffer[..count])
                    .await
                    .map_err(|e| format!("Failed to write {}: {}", local_path.display(), e))?;
                file_transferred = file_transferred.saturating_add(count as u64);
                progress
                    .maybe_emit(remote_path, &display_name, file_transferred, file_size)
                    .await;
            }
        }

        local
            .flush()
            .await
            .map_err(|e| format!("Failed to flush {}: {}", local_path.display(), e))?;
        Ok(())
    }
    .await;

    if let Err(error) = result {
        let _ = fs::remove_file(local_path).await;
        return Err(error);
    }

    progress
        .emit(
            remote_path,
            &display_name,
            file_size,
            Some(file_size),
            false,
            None,
        )
        .await;

    Ok(())
}

async fn download_remote_tree_streaming(
    sftp: &SftpSession,
    ssh_handle: &SharedSshHandle,
    progress: &mut BatchDownloadProgress<'_>,
    remote_path: &str,
    local_path: &Path,
    failures: &mut Vec<String>,
    cancel: &TransferCancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("Download canceled".to_string());
    }

    let metadata = match sftp.metadata(remote_path).await {
        Ok(m) => m,
        Err(e) => {
            failures.push(format!("{} ({})", remote_path, e));
            return Ok(());
        }
    };

    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        if let Err(error) = download_remote_file_in_batch(
            sftp,
            ssh_handle,
            progress,
            remote_path,
            local_path,
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

    let entries = match sftp.read_dir(remote_path).await {
        Ok(e) => e,
        Err(e) => {
            failures.push(format!("{} ({})", remote_path, e));
            return Ok(());
        }
    };
    let entries = entries
        .into_iter()
        .filter(|entry| {
            let name = entry.file_name();
            name != "." && name != ".."
        })
        .collect::<Vec<_>>();
    let folder_name = remote_display_name(remote_path);
    let total_entries = entries.len() as u64;
    let mut completed_entries = 0_u64;

    progress
        .emit(
            remote_path,
            &folder_name,
            0,
            Some(total_entries),
            false,
            None,
        )
        .await;

    if total_entries == 0 {
        progress
            .emit(remote_path, &folder_name, 0, Some(0), false, None)
            .await;
        return Ok(());
    }

    let mut local_names = HashSet::new();
    for entry in entries {
        if cancel.is_cancelled() {
            return Err("Download canceled".to_string());
        }

        let name = entry.file_name();
        let local_name = sanitize_local_name(&name);
        if !local_names.insert(local_name_collision_key(&local_name)) {
            failures.push(format!(
                "{} (local filename collides with another item after sanitization)",
                entry.path()
            ));
            completed_entries = completed_entries.saturating_add(1);
            progress
                .emit(
                    remote_path,
                    &folder_name,
                    completed_entries,
                    Some(total_entries),
                    false,
                    None,
                )
                .await;
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
            cancel,
        )
        .await
        {
            if error == "Download canceled" {
                return Err(error);
            }
            failures.push(format!("{} ({})", child_remote_path, error));
        }

        completed_entries = completed_entries.saturating_add(1);
        progress
            .emit(
                remote_path,
                &folder_name,
                completed_entries,
                Some(total_entries),
                false,
                None,
            )
            .await;
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
    generation: u64,
    mut stop_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut last_uploaded = file_fingerprint(&local_path).await;
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
        let mut entries = Vec::new();
        let dir_entries = sftp
            .read_dir(&canonical_path)
            .await
            .map_err(|e| format!("Failed to read directory {}: {}", canonical_path, e))?;

        for entry in dir_entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }

            let metadata = entry.metadata();
            let file_type = entry.file_type();

            entries.push(FileEntry {
                path: entry.path(),
                name,
                is_dir: file_type.is_dir(),
                is_symlink: file_type.is_symlink(),
                size: metadata.size,
                modified: metadata.modified().ok().and_then(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_secs())
                }),
            });
        }

        entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));

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
                .emit("", "Starting download...", 0, Some(1), false, None)
                .await;

            for remote_path in &normalized_paths {
                let local_path = destination_dir.join(remote_display_name(remote_path));
                if let Err(error) = download_remote_tree_streaming(
                    &sftp,
                    &ssh_handle,
                    &mut progress,
                    remote_path,
                    &local_path,
                    &mut failures,
                    &cancel,
                )
                .await
                {
                    progress
                        .emit(
                            "",
                            "Download canceled",
                            1,
                            Some(1),
                            true,
                            Some(error.clone()),
                        )
                        .await;
                    return Err(error);
                }
            }

            let display_path = progress.batch_complete_display();

            progress
                .emit(
                    "",
                    &display_path,
                    1,
                    Some(1),
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
    let local_path = local_download_path(&id, &remote_path);

    edit_state.stop_watcher(&id, &remote_path)?;
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

    let (generation, stop_rx) = edit_state.replace_watcher(&id, &remote_path)?;
    spawn_edit_watcher(
        app.clone(),
        id.clone(),
        session_id,
        remote_path.clone(),
        local_path.clone(),
        generation,
        stop_rx,
    );

    let open_result = app
        .opener()
        .open_path(
            &local_path_string,
            with.as_deref().filter(|value| !value.is_empty()),
        )
        .map_err(|e| e.to_string());

    if let Err(error) = open_result {
        edit_state.stop_watcher(&id, &remote_path)?;
        return Err(error);
    }

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

    let lookup_id = id.clone();
    let session_lookup_id = lookup_id.clone();
    let queue_id = queue_id("upload-batch", session_id);
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let result = with_sftp_session(
        state.inner(),
        &session_lookup_id,
        move |_, ssh_handle| async move {
            let mut failures = Vec::new();

            for entry in &normalized {
                if cancel.is_cancelled() {
                    return Err("Upload canceled".to_string());
                }
                let local_path = PathBuf::from(&entry.local_path);
                let transfer_id = transfer_id_for_path("upload", &entry.remote_path);
                let display_path = remote_display_name(&entry.remote_path);

                if let Err(error) = upload_local_file(
                    &ssh_handle,
                    &app,
                    &lookup_id,
                    session_id,
                    &entry.remote_path,
                    &local_path,
                    &queue_id,
                    &transfer_id,
                    &display_path,
                    &cancel,
                )
                .await
                {
                    if error == "Upload canceled" || error == "Transfer canceled" {
                        return Err("Upload canceled".to_string());
                    }
                    failures.push(format!("{} ({})", entry.local_path, error));
                }
            }

            if failures.is_empty() {
                Ok(())
            } else {
                Err(format_batch_upload_error(&failures))
            }
        },
    )
    .await;
    transfer_state.finish_queue(&id, &finish_queue_id)?;
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
    let total = Some(data.len() as u64);

    let lookup_id = id.clone();
    let task_id = id.clone();
    let queue_id = queue_id("upload-bytes", session_id);
    let cancel = transfer_state.register_queue(&id, &queue_id)?;
    let finish_queue_id = queue_id.clone();
    let transfer_id = transfer_id_for_path("upload", &remote_path);
    let display_path = remote_path.clone();
    let result = with_sftp_session(state.inner(), &lookup_id, move |sftp, _| async move {
        emit_transfer_progress(
            &app,
            &task_id,
            session_id,
            &queue_id,
            &transfer_id,
            "upload",
            &remote_path,
            &display_path,
            0,
            total,
            false,
            None,
        )
        .await;

        let mut transferred = 0_u64;
        let mut last_progress_emit = Instant::now();
        let result: Result<(), String> = async {
            let mut remote = sftp
                .open_with_flags(
                    &remote_path,
                    OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE | OpenFlags::READ,
                )
                .await
                .map_err(|e| format!("Failed to open {} for upload: {}", remote_path, e))?;

            for chunk in data.chunks(TRANSFER_BUFFER_SIZE) {
                if cancel.is_cancelled() {
                    return Err("Upload canceled".to_string());
                }
                remote
                    .write_all(chunk)
                    .await
                    .map_err(|e| format!("Failed to write {}: {}", remote_path, e))?;
                transferred += chunk.len() as u64;
                if last_progress_emit.elapsed() >= TRANSFER_PROGRESS_EMIT_INTERVAL {
                    emit_transfer_progress(
                        &app,
                        &task_id,
                        session_id,
                        &queue_id,
                        &transfer_id,
                        "upload",
                        &remote_path,
                        &display_path,
                        transferred,
                        total,
                        false,
                        None,
                    )
                    .await;
                    last_progress_emit = Instant::now();
                }
            }
            remote
                .flush()
                .await
                .map_err(|e| format!("Failed to flush {}: {}", remote_path, e))?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                emit_transfer_progress(
                    &app,
                    &task_id,
                    session_id,
                    &queue_id,
                    &transfer_id,
                    "upload",
                    &remote_path,
                    &display_path,
                    transferred,
                    total,
                    true,
                    None,
                )
                .await;
                Ok(())
            }
            Err(error) => {
                emit_transfer_error(
                    &app,
                    &task_id,
                    session_id,
                    &queue_id,
                    &transfer_id,
                    "upload",
                    &remote_path,
                    &display_path,
                    transferred,
                    total,
                    &error,
                )
                .await;
                Err(error)
            }
        }
    })
    .await;
    transfer_state.finish_queue(&id, &finish_queue_id)?;
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
        Some(0) | None => Ok(()),
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

                if let Err(error) = result {
                    failures.push(error);
                }
                progress.tick().await;
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
