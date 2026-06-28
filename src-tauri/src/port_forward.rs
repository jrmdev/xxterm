use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Mutex;

use russh::client;
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::watch;
use tokio::time::{sleep, Duration, Instant};

use crate::crypto::MasterPasswordState;
use crate::session_store::SessionStore;
use crate::ssh::{self, FwdCommand, SharedSshHandle, SshState};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ForwardStatus {
    pub id: i64,
    pub forward_type: String,
    pub local_port: u16,
    pub local_host: String,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
    pub status: String,
    pub error: Option<String>,
}

struct ForwardEntry {
    #[allow(dead_code)]
    forward_id: i64,
    local_bind: Option<LocalBind>,
    status: Mutex<String>,
    error: Mutex<Option<String>>,
    owner: Mutex<StartOwner>,
}

#[derive(Clone, Debug)]
struct LocalBind {
    host: String,
    addr: SocketAddr,
}

impl LocalBind {
    fn conflicts_with(&self, other: &LocalBind) -> bool {
        self.addr.port() == other.addr.port()
            && (self.addr.ip() == other.addr.ip()
                || self.addr.ip().is_unspecified()
                || other.addr.ip().is_unspecified())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StartOwner {
    Manual,
    Terminal(String),
}

pub struct PortForwardState {
    entries: Mutex<HashMap<i64, ForwardEntry>>,
    pending: Mutex<HashMap<i64, StartOwner>>,
    cancel_signals: Mutex<HashMap<i64, watch::Sender<bool>>>,
}

impl PortForwardState {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            cancel_signals: Mutex::new(HashMap::new()),
        }
    }

    fn prepare_start(
        &self,
        fwd: &crate::session_store::PortForward,
        owner: StartOwner,
    ) -> Result<bool, String> {
        let local_bind = local_bind_for_forward(fwd)?;

        if let Ok(mut pending) = self.pending.lock() {
            if let Some(existing_owner) = pending.get_mut(&fwd.id) {
                if matches!(owner, StartOwner::Manual) {
                    *existing_owner = StartOwner::Manual;
                    self.set_owner(fwd.id, StartOwner::Manual);
                }
                return Ok(false);
            }
        }

        if let Ok(mut entries) = self.entries.lock() {
            if let Some(bind) = &local_bind {
                if let Some((conflicting_id, conflicting_bind)) =
                    active_local_bind_conflict(&entries, fwd.id, bind)
                {
                    return Err(format!(
                        "Cannot start port forward: local port {} on {} is already in use by port forward {} (bound to {}). Stop the other forward first.",
                        fwd.local_port, bind.host, conflicting_id, conflicting_bind.host
                    ));
                }
            }

            if let Some(entry) = entries.get_mut(&fwd.id) {
                let is_running = entry
                    .status
                    .lock()
                    .ok()
                    .map(|status| matches!(status.as_str(), "starting" | "active" | "pending"))
                    .unwrap_or(false);

                if is_running {
                    if let StartOwner::Manual = owner {
                        if let Ok(mut entry_owner) = entry.owner.lock() {
                            *entry_owner = StartOwner::Manual;
                        }
                    }
                    return Ok(false);
                }

                if let Ok(mut status) = entry.status.lock() {
                    *status = "starting".to_string();
                }
                if let Ok(mut error) = entry.error.lock() {
                    *error = None;
                }
                if let Ok(mut entry_owner) = entry.owner.lock() {
                    *entry_owner = owner;
                }
                entry.local_bind = local_bind;
                return Ok(true);
            }

            entries.insert(
                fwd.id,
                ForwardEntry {
                    forward_id: fwd.id,
                    local_bind,
                    status: Mutex::new("starting".to_string()),
                    error: Mutex::new(None),
                    owner: Mutex::new(owner),
                },
            );
            return Ok(true);
        }

        Err("Failed to lock port forward state".to_string())
    }

    fn set_owner(&self, forward_id: i64, owner: StartOwner) {
        if let Ok(entries) = self.entries.lock() {
            if let Some(entry) = entries.get(&forward_id) {
                if let Ok(mut entry_owner) = entry.owner.lock() {
                    *entry_owner = owner;
                }
            }
        }
    }

    pub fn set_status(&self, forward_id: i64, status: String, error: Option<String>) {
        if let Ok(e) = self.entries.lock() {
            if let Some(entry) = e.get(&forward_id) {
                if let Ok(mut s) = entry.status.lock() {
                    *s = status;
                }
                if let Ok(mut err) = entry.error.lock() {
                    *err = error;
                }
            }
        }
    }

    fn add_pending(&self, forward_id: i64, owner: StartOwner) {
        if let Ok(mut p) = self.pending.lock() {
            p.insert(forward_id, owner);
        }
    }

    pub fn remove_pending(&self, forward_id: i64) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&forward_id);
        }
    }

    fn take_pending_for_session(
        &self,
        session_id: i64,
        session_store: &SessionStore,
    ) -> Vec<(i64, StartOwner)> {
        let forwards = match session_store.list_port_forwards(session_id) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        let mut result = Vec::new();
        if let Ok(mut p) = self.pending.lock() {
            for fwd in &forwards {
                if let Some(owner) = p.remove(&fwd.id) {
                    result.push((fwd.id, owner));
                }
            }
        }
        result
    }

    pub fn store_cancel(&self, forward_id: i64, tx: watch::Sender<bool>) {
        if let Ok(mut c) = self.cancel_signals.lock() {
            c.insert(forward_id, tx);
        }
    }

    pub fn remove_cancel(&self, forward_id: i64) {
        if let Ok(mut c) = self.cancel_signals.lock() {
            c.remove(&forward_id);
        }
    }

    pub fn cancel_forward(&self, forward_id: i64) {
        let mut had_running_task = false;
        if let Ok(mut c) = self.cancel_signals.lock() {
            if let Some(tx) = c.remove(&forward_id) {
                had_running_task = true;
                let _ = tx.send(true);
            }
        }
        self.remove_pending(forward_id);
        if !had_running_task {
            if let Ok(mut e) = self.entries.lock() {
                e.remove(&forward_id);
            }
        }
    }

    fn cancel_forward_if_owned(&self, forward_id: i64, owner: &StartOwner) {
        if let Ok(mut pending) = self.pending.lock() {
            if pending.get(&forward_id) == Some(owner) {
                pending.remove(&forward_id);
                if let Ok(mut entries) = self.entries.lock() {
                    entries.remove(&forward_id);
                }
                return;
            }
        }

        let should_cancel = self
            .entries
            .lock()
            .ok()
            .and_then(|entries| {
                entries.get(&forward_id).and_then(|entry| {
                    entry
                        .owner
                        .lock()
                        .ok()
                        .map(|entry_owner| *entry_owner == *owner)
                })
            })
            .unwrap_or(false);

        if should_cancel {
            self.cancel_forward(forward_id);
        }
    }

    pub fn unregister_session(&self, session_id: i64, session_store: &SessionStore) {
        if let Ok(forwards) = session_store.list_port_forwards(session_id) {
            for fwd in &forwards {
                self.cancel_forward(fwd.id);
            }
        }
    }

    pub fn get_status(
        &self,
        session_id: i64,
        session_store: &SessionStore,
    ) -> Result<Vec<ForwardStatus>, String> {
        let configs = session_store.list_port_forwards(session_id)?;
        let pending = self.pending.lock().map_err(|e| e.to_string())?;
        let entries = self.entries.lock().map_err(|e| e.to_string())?;

        let mut result = Vec::new();
        for cfg in configs {
            let (status, error) = if let Some(entry) = entries.get(&cfg.id) {
                let s = entry.status.lock().map_err(|e| e.to_string())?;
                let e = entry.error.lock().map_err(|e| e.to_string())?;
                (s.clone(), e.clone())
            } else if pending.contains_key(&cfg.id) {
                ("pending".to_string(), None)
            } else {
                ("stopped".to_string(), None)
            };

            result.push(ForwardStatus {
                id: cfg.id,
                forward_type: cfg.forward_type,
                local_port: cfg.local_port,
                local_host: cfg.local_host,
                remote_host: cfg.remote_host,
                remote_port: cfg.remote_port,
                status,
                error,
            });
        }
        Ok(result)
    }

    pub fn has_active_or_pending_for_session(
        &self,
        session_id: i64,
        session_store: &SessionStore,
    ) -> bool {
        let forwards = match session_store.list_port_forwards(session_id) {
            Ok(forwards) => forwards,
            Err(_) => return false,
        };

        let forward_ids: HashSet<i64> = forwards.into_iter().map(|fwd| fwd.id).collect();

        let has_pending = self
            .pending
            .lock()
            .ok()
            .map(|pending| pending.keys().any(|id| forward_ids.contains(id)))
            .unwrap_or(false);
        if has_pending {
            return true;
        }

        self.entries
            .lock()
            .ok()
            .map(|entries| {
                entries.iter().any(|(id, entry)| {
                    if !forward_ids.contains(id) {
                        return false;
                    }

                    entry
                        .status
                        .lock()
                        .ok()
                        .map(|status| matches!(status.as_str(), "starting" | "active" | "pending"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    fn running_owner(&self, forward_id: i64) -> Option<StartOwner> {
        if let Ok(entries) = self.entries.lock() {
            if let Some(entry) = entries.get(&forward_id) {
                let is_running = entry
                    .status
                    .lock()
                    .ok()
                    .map(|status| matches!(status.as_str(), "starting" | "active" | "pending"))
                    .unwrap_or(false);
                if is_running {
                    return entry.owner.lock().ok().map(|owner| owner.clone());
                }
            }
        }

        self.pending
            .lock()
            .ok()
            .and_then(|pending| pending.get(&forward_id).cloned())
    }

    async fn wait_for_stop(&self, forward_id: i64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let still_pending = self
                .pending
                .lock()
                .ok()
                .map(|pending| pending.contains_key(&forward_id))
                .unwrap_or(false);

            let still_running = self
                .entries
                .lock()
                .ok()
                .and_then(|entries| {
                    entries.get(&forward_id).map(|entry| {
                        entry
                            .status
                            .lock()
                            .ok()
                            .map(|status| {
                                matches!(status.as_str(), "starting" | "active" | "pending")
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if !still_pending && !still_running {
                return true;
            }

            if Instant::now() >= deadline {
                return false;
            }

            sleep(Duration::from_millis(25)).await;
        }
    }
}

fn local_bind_for_forward(
    fwd: &crate::session_store::PortForward,
) -> Result<Option<LocalBind>, String> {
    if !matches!(fwd.forward_type.as_str(), "local" | "dynamic") {
        return Ok(None);
    }

    let host = if fwd.local_host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        fwd.local_host.clone()
    };
    let addr = resolve_bind_addr(&format!("{}:{}", host, fwd.local_port)).map_err(|e| {
        format!(
            "Invalid local bind address {}:{}: {}",
            host, fwd.local_port, e
        )
    })?;

    Ok(Some(LocalBind { host, addr }))
}

fn active_local_bind_conflict<'a>(
    entries: &'a HashMap<i64, ForwardEntry>,
    forward_id: i64,
    bind: &LocalBind,
) -> Option<(i64, &'a LocalBind)> {
    entries.iter().find_map(|(id, entry)| {
        if *id == forward_id {
            return None;
        }

        let is_running = entry
            .status
            .lock()
            .ok()
            .map(|status| matches!(status.as_str(), "starting" | "active" | "pending"))
            .unwrap_or(false);
        if !is_running {
            return None;
        }

        entry
            .local_bind
            .as_ref()
            .filter(|existing| existing.conflicts_with(bind))
            .map(|existing| (*id, existing))
    })
}

fn maybe_disconnect_headless_session(
    session_id: i64,
    pf_state: &PortForwardState,
    ssh_state: &SshState,
    session_store: &SessionStore,
) -> Result<(), String> {
    if ssh::has_attached_terminal(ssh_state, session_id) {
        return Ok(());
    }

    if pf_state.has_active_or_pending_for_session(session_id, session_store) {
        return Ok(());
    }

    ssh::disconnect_headless_ssh(session_id, ssh_state)
}

fn finish_forward_run(
    app: &AppHandle,
    session_id: i64,
    forward_id: i64,
    status: &str,
    error: Option<String>,
) {
    app.state::<PortForwardState>()
        .set_status(forward_id, status.to_string(), error);
    app.state::<PortForwardState>().remove_cancel(forward_id);

    let _ = maybe_disconnect_headless_session(
        session_id,
        &app.state::<PortForwardState>(),
        &app.state::<SshState>(),
        &app.state::<SessionStore>(),
    );
}

fn queue_forward_start(
    pf_state: &PortForwardState,
    tx: Option<&tokio::sync::mpsc::UnboundedSender<FwdCommand>>,
    fwd: &crate::session_store::PortForward,
    owner: StartOwner,
    app: &AppHandle,
) {
    let (stop_tx, stop_rx) = watch::channel(false);
    pf_state.store_cancel(fwd.id, stop_tx);
    pf_state.remove_pending(fwd.id);

    let msg = match fwd.forward_type.as_str() {
        "local" => {
            let remote_host = fwd.remote_host.clone().unwrap_or_default();
            let remote_port = fwd.remote_port.unwrap_or(0);
            FwdCommand::StartLocal {
                session_id: fwd.session_id,
                local_host: fwd.local_host.clone(),
                local_port: fwd.local_port,
                remote_host,
                remote_port,
                forward_id: fwd.id,
                stop_rx: Some(stop_rx),
                app: app.clone(),
            }
        }
        "remote" => {
            let remote_host = fwd.remote_host.clone().unwrap_or_default();
            let remote_port = fwd.remote_port.unwrap_or(0);
            FwdCommand::StartRemote {
                session_id: fwd.session_id,
                local_host: fwd.local_host.clone(),
                local_port: fwd.local_port,
                remote_host,
                remote_port,
                forward_id: fwd.id,
                stop_rx: Some(stop_rx),
                app: app.clone(),
            }
        }
        "dynamic" => FwdCommand::StartDynamic {
            session_id: fwd.session_id,
            local_host: fwd.local_host.clone(),
            local_port: fwd.local_port,
            forward_id: fwd.id,
            stop_rx: Some(stop_rx),
            app: app.clone(),
        },
        _ => return,
    };

    if tx.and_then(|sender| sender.send(msg).ok()).is_none() {
        pf_state.set_status(fwd.id, "pending".to_string(), None);
        pf_state.add_pending(fwd.id, owner);
        pf_state.remove_cancel(fwd.id);
    }
}

pub async fn restart_forward_if_running(
    updated: &crate::session_store::PortForward,
    pf_state: &PortForwardState,
    master_pwd_state: &MasterPasswordState,
    ssh_state: &SshState,
    session_store: &SessionStore,
    app: &AppHandle,
) -> Result<(), String> {
    let Some(owner) = pf_state.running_owner(updated.id) else {
        return Ok(());
    };

    pf_state.cancel_forward(updated.id);
    let _ = pf_state
        .wait_for_stop(updated.id, Duration::from_secs(2))
        .await;

    let has_transport = ssh_state
        .fwd_commands
        .lock()
        .map_err(|e| e.to_string())?
        .contains_key(&updated.session_id);

    if !has_transport {
        ssh::connect_ssh_headless(
            updated.session_id,
            master_pwd_state,
            ssh_state,
            session_store,
            app,
        )
        .await?;
    }

    if !pf_state.prepare_start(updated, owner.clone())? {
        return Ok(());
    }

    let cmds = ssh_state.fwd_commands.lock().map_err(|e| e.to_string())?;
    queue_forward_start(pf_state, cmds.get(&updated.session_id), updated, owner, app);

    Ok(())
}

fn resolve_bind_addr(addr: &str) -> Result<SocketAddr, std::io::Error> {
    addr.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "no bind addresses resolved",
        )
    })
}

fn bind_tcp_listener(addr: &str) -> Result<TcpListener, std::io::Error> {
    let socket_addr = resolve_bind_addr(addr)?;
    let socket = if socket_addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.bind(socket_addr)?;
    socket.listen(1024)
}

pub(crate) async fn bridge_tcp_channel(
    stream: tokio::net::TcpStream,
    mut channel: russh::Channel<client::Msg>,
) {
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let (data_tx, mut data_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if data_tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            Some(data) = data_rx.recv() => {
                if channel.data(&data[..]).await.is_err() {
                    break;
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if tcp_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
            else => break,
        }
    }

    reader.abort();
}

async fn handle_socks5(
    mut stream: tokio::net::TcpStream,
    shared_handle: SharedSshHandle,
    forward_id: i64,
) {
    let mut header = [0u8; 2];
    if stream.read_exact(&mut header).await.is_err() {
        return;
    }
    if header[0] != 5 {
        return;
    }
    let nmethods = header[1] as usize;
    let mut methods = vec![0u8; nmethods];
    if stream.read_exact(&mut methods).await.is_err() {
        return;
    }
    if !methods.contains(&0) {
        let _ = stream.write_all(&[5, 0xff]).await;
        return;
    }
    if stream.write_all(&[5, 0]).await.is_err() {
        return;
    }

    let mut req_header = [0u8; 4];
    if stream.read_exact(&mut req_header).await.is_err() {
        return;
    }
    let cmd = req_header[1];
    let addr_type = req_header[3];

    let (remote_host, remote_port) = match addr_type {
        1 => {
            let mut addr = [0u8; 4];
            if stream.read_exact(&mut addr).await.is_err() {
                return;
            }
            let mut port = [0u8; 2];
            if stream.read_exact(&mut port).await.is_err() {
                return;
            }
            (
                format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]),
                u16::from_be_bytes(port),
            )
        }
        3 => {
            let mut len = [0u8; 1];
            if stream.read_exact(&mut len).await.is_err() {
                return;
            }
            let mut domain = vec![0u8; len[0] as usize];
            if stream.read_exact(&mut domain).await.is_err() {
                return;
            }
            let mut port = [0u8; 2];
            if stream.read_exact(&mut port).await.is_err() {
                return;
            }
            (
                String::from_utf8_lossy(&domain).to_string(),
                u16::from_be_bytes(port),
            )
        }
        4 => {
            let mut addr = [0u8; 16];
            if stream.read_exact(&mut addr).await.is_err() {
                return;
            }
            let mut port = [0u8; 2];
            if stream.read_exact(&mut port).await.is_err() {
                return;
            }
            (
                std::net::Ipv6Addr::from(addr).to_string(),
                u16::from_be_bytes(port),
            )
        }
        _ => {
            let _ = stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            return;
        }
    };

    if cmd != 1 {
        let _ = stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        return;
    }

    let channel = {
        let guard = shared_handle.lock().await;
        match guard
            .channel_open_direct_tcpip(&remote_host, remote_port as u32, "127.0.0.1", 0)
            .await
        {
            Ok(ch) => ch,
            Err(_) => {
                let _ = stream.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                return;
            }
        }
    };

    let _ = stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await;

    if let Err(e) = stream.set_nodelay(true) {
        log::warn!("fwd {}: set_nodelay failed: {}", forward_id, e);
    }

    bridge_tcp_channel(stream, channel).await;
}

pub async fn run_local_forward(
    shared_handle: SharedSshHandle,
    session_id: i64,
    local_host: String,
    local_port: u16,
    remote_host: String,
    remote_port: u16,
    forward_id: i64,
    mut stop_rx: watch::Receiver<bool>,
    app: AppHandle,
) {
    let bind_host = if local_host.is_empty() {
        "127.0.0.1"
    } else {
        &local_host
    };
    let addr = format!("{}:{}", bind_host, local_port);
    let listener = match bind_tcp_listener(&addr) {
        Ok(l) => l,
        Err(e) => {
            log::error!("fwd {}: bind {} failed: {}", forward_id, addr, e);
            finish_forward_run(&app, session_id, forward_id, "failed", Some(e.to_string()));
            return;
        }
    };

    app.state::<PortForwardState>()
        .set_status(forward_id, "active".to_string(), None);
    log::info!(
        "fwd {}: local forward {} -> {}:{} started",
        forward_id,
        addr,
        remote_host,
        remote_port
    );

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let h = shared_handle.clone();
                        let rh = remote_host.clone();
                        let rp = remote_port;
                        let lp = local_port;

                        tokio::spawn(async move {
                            let channel = {
                                let guard = h.lock().await;
                                match guard
                                    .channel_open_direct_tcpip(&rh, rp as u32, "127.0.0.1", lp as u32)
                                    .await
                                {
                                    Ok(ch) => ch,
                                    Err(e) => {
                                        log::error!("fwd {}: direct-tcpip failed: {}", forward_id, e);
                                        return;
                                    }
                                }
                            };
                            bridge_tcp_channel(stream, channel).await;
                        });
                    }
                    Err(e) => {
                        log::error!("fwd {}: accept error: {}", forward_id, e);
                    }
                }
            }
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    log::info!("fwd {}: local forward stopped", forward_id);
                    break;
                }
            }
        }
    }

    finish_forward_run(&app, session_id, forward_id, "stopped", None);
}

pub async fn run_remote_forward(
    shared_handle: SharedSshHandle,
    session_id: i64,
    local_host: String,
    local_port: u16,
    remote_host: String,
    remote_port: u16,
    forward_id: i64,
    mut stop_rx: watch::Receiver<bool>,
    app: AppHandle,
) {
    let bind_addr = if local_host.is_empty() {
        "127.0.0.1"
    } else {
        &local_host
    };
    let bind_port = local_port as u32;

    // Register the forward destination in the remote forward map
    let dest = format!("{}:{}", remote_host, remote_port);
    let remote_key = (bind_addr.to_string(), bind_port);
    if let Ok(mut map) = app.state::<SshState>().remote_forwards.by_port.lock() {
        map.insert(remote_key.clone(), dest.clone());
    }

    // Request the server to start listening
    {
        let guard = shared_handle.lock().await;
        match guard.tcpip_forward(bind_addr, bind_port).await {
            Ok(allocated_port) => {
                log::info!(
                    "fwd {}: tcpip-forward {}:{} (allocated port {})",
                    forward_id,
                    bind_addr,
                    bind_port,
                    allocated_port
                );
            }
            Err(e) => {
                log::error!("fwd {}: tcpip-forward request failed: {}", forward_id, e);
                finish_forward_run(&app, session_id, forward_id, "failed", Some(e.to_string()));
                if let Ok(mut map) = app.state::<SshState>().remote_forwards.by_port.lock() {
                    map.remove(&remote_key);
                }
                return;
            }
        }
    }

    app.state::<PortForwardState>()
        .set_status(forward_id, "active".to_string(), None);
    log::info!(
        "fwd {}: remote forward {}:{} -> {} started",
        forward_id,
        bind_addr,
        bind_port,
        dest
    );

    // Wait for stop signal
    stop_rx.changed().await.ok();
    if !*stop_rx.borrow() {
        // Also wait for actual stop
        let _ = stop_rx.changed().await;
    }

    log::info!("fwd {}: remote forward stopped", forward_id);

    // Cancel the tcpip-forward and unregister
    {
        let guard = shared_handle.lock().await;
        let _ = guard.cancel_tcpip_forward(bind_addr, bind_port).await;
    }
    if let Ok(mut map) = app.state::<SshState>().remote_forwards.by_port.lock() {
        map.remove(&remote_key);
    }

    finish_forward_run(&app, session_id, forward_id, "stopped", None);
}

pub async fn run_dynamic_forward(
    shared_handle: SharedSshHandle,
    session_id: i64,
    local_host: String,
    local_port: u16,
    forward_id: i64,
    mut stop_rx: watch::Receiver<bool>,
    app: AppHandle,
) {
    let bind_host = if local_host.is_empty() {
        "127.0.0.1"
    } else {
        &local_host
    };
    let addr = format!("{}:{}", bind_host, local_port);
    let listener = match bind_tcp_listener(&addr) {
        Ok(l) => l,
        Err(e) => {
            log::error!("fwd {}: SOCKS bind {} failed: {}", forward_id, addr, e);
            finish_forward_run(&app, session_id, forward_id, "failed", Some(e.to_string()));
            return;
        }
    };

    app.state::<PortForwardState>()
        .set_status(forward_id, "active".to_string(), None);
    log::info!("fwd {}: dynamic SOCKS5 on {} started", forward_id, addr);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let h = shared_handle.clone();
                        let fid = forward_id;
                        tokio::spawn(async move {
                            handle_socks5(stream, h, fid).await;
                        });
                    }
                    Err(e) => {
                        log::error!("fwd {}: socks accept error: {}", forward_id, e);
                    }
                }
            }
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    log::info!("fwd {}: dynamic forward stopped", forward_id);
                    break;
                }
            }
        }
    }

    finish_forward_run(&app, session_id, forward_id, "stopped", None);
}

#[tauri::command]
pub async fn start_port_forwards(
    session_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
    master_pwd_state: tauri::State<'_, MasterPasswordState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let forwards = session_store.list_port_forwards(session_id)?;
    let has_transport = ssh_state
        .fwd_commands
        .lock()
        .map_err(|e| e.to_string())?
        .contains_key(&session_id);

    if !has_transport {
        ssh::connect_ssh_headless(
            session_id,
            &master_pwd_state,
            &ssh_state,
            &session_store,
            &app,
        )
        .await?;
    }

    let cmds = ssh_state.fwd_commands.lock().map_err(|e| e.to_string())?;
    let tx = cmds.get(&session_id);

    for fwd in &forwards {
        if !pf_state.prepare_start(fwd, StartOwner::Manual)? {
            continue;
        }

        queue_forward_start(&pf_state, tx, fwd, StartOwner::Manual, &app);
    }

    Ok(())
}

#[tauri::command]
pub async fn auto_start_port_forwards(
    session_id: i64,
    terminal_id: String,
    pf_state: tauri::State<'_, PortForwardState>,
    master_pwd_state: tauri::State<'_, MasterPasswordState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let forwards: Vec<_> = session_store
        .list_port_forwards(session_id)?
        .into_iter()
        .filter(|fwd| fwd.always_start)
        .collect();
    if forwards.is_empty() {
        return Ok(());
    }

    let has_transport = ssh_state
        .fwd_commands
        .lock()
        .map_err(|e| e.to_string())?
        .contains_key(&session_id);

    if !has_transport {
        ssh::connect_ssh_headless(
            session_id,
            &master_pwd_state,
            &ssh_state,
            &session_store,
            &app,
        )
        .await?;
    }

    let cmds = ssh_state.fwd_commands.lock().map_err(|e| e.to_string())?;
    let tx = cmds.get(&session_id);
    let owner = StartOwner::Terminal(terminal_id);

    for fwd in &forwards {
        if !pf_state.prepare_start(fwd, owner.clone())? {
            continue;
        }

        queue_forward_start(&pf_state, tx, fwd, owner.clone(), &app);
    }

    Ok(())
}

#[tauri::command]
pub async fn stop_port_forwards(
    session_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
) -> Result<(), String> {
    let forwards = session_store.list_port_forwards(session_id)?;
    for fwd in &forwards {
        pf_state.cancel_forward(fwd.id);
    }

    maybe_disconnect_headless_session(session_id, &pf_state, &ssh_state, &session_store)?;

    Ok(())
}

#[tauri::command]
pub async fn auto_stop_port_forwards(
    session_id: i64,
    terminal_id: String,
    pf_state: tauri::State<'_, PortForwardState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
) -> Result<(), String> {
    let forwards = session_store.list_port_forwards(session_id)?;
    let owner = StartOwner::Terminal(terminal_id);

    for fwd in &forwards {
        pf_state.cancel_forward_if_owned(fwd.id, &owner);
    }

    maybe_disconnect_headless_session(session_id, &pf_state, &ssh_state, &session_store)?;
    Ok(())
}

#[tauri::command]
pub async fn start_single_port_forward(
    session_id: i64,
    forward_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
    master_pwd_state: tauri::State<'_, MasterPasswordState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let forwards = session_store.list_port_forwards(session_id)?;
    let fwd = forwards
        .into_iter()
        .find(|f| f.id == forward_id)
        .ok_or_else(|| format!("Port forward {} not found", forward_id))?;

    let has_transport = ssh_state
        .fwd_commands
        .lock()
        .map_err(|e| e.to_string())?
        .contains_key(&session_id);

    if !has_transport {
        ssh::connect_ssh_headless(
            session_id,
            &master_pwd_state,
            &ssh_state,
            &session_store,
            &app,
        )
        .await?;
    }

    if !pf_state.prepare_start(&fwd, StartOwner::Manual)? {
        return Ok(());
    }

    let cmds = ssh_state.fwd_commands.lock().map_err(|e| e.to_string())?;
    queue_forward_start(
        &pf_state,
        cmds.get(&session_id),
        &fwd,
        StartOwner::Manual,
        &app,
    );

    Ok(())
}

#[allow(unused_variables)]
#[tauri::command]
pub async fn stop_single_port_forward(
    session_id: i64,
    forward_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
    ssh_state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
) -> Result<(), String> {
    pf_state.cancel_forward(forward_id);
    maybe_disconnect_headless_session(session_id, &pf_state, &ssh_state, &session_store)?;
    Ok(())
}

#[tauri::command]
pub async fn get_port_forward_status(
    session_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
    session_store: tauri::State<'_, SessionStore>,
) -> Result<Vec<ForwardStatus>, String> {
    pf_state.get_status(session_id, &session_store)
}

pub fn process_pending_forwards(app: &AppHandle, session_id: i64) {
    let pf_state = app.state::<PortForwardState>();
    let session_store = app.state::<SessionStore>();

    let pending = pf_state.take_pending_for_session(session_id, &session_store);
    if pending.is_empty() {
        return;
    }

    let forwards = match session_store.list_port_forwards(session_id) {
        Ok(f) => f,
        Err(_) => return,
    };

    let ssh_state = app.state::<SshState>();
    let cmds = match ssh_state.fwd_commands.lock() {
        Ok(c) => c,
        Err(_) => return,
    };

    let tx = match cmds.get(&session_id) {
        Some(tx) => tx,
        None => return,
    };

    for (fwd_id, owner) in pending {
        let fwd = match forwards.iter().find(|f| f.id == fwd_id) {
            Some(f) => f,
            None => continue,
        };
        queue_forward_start(&pf_state, Some(tx), fwd, owner, app);
    }
}
