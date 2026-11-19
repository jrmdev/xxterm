use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, Weak};

use russh::client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OnceCell};
use tokio::task::JoinSet;
use tokio::time::{sleep_until, timeout, timeout_at, Duration, Instant};

use crate::crypto::MasterPasswordState;
use crate::session_store::{PortForward, SessionStore};
use crate::ssh::{self, ForwardConnection, ForwardHandle, KeyPassphraseInput, RemoteForwardTarget};

#[cfg(test)]
#[path = "port_forward_tests.rs"]
mod tests;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const START_TIMEOUT_ERROR: &str = "Start timed out after 10 seconds.";
const STATUS_CHANGED_EVENT: &str = "port-forward-status-changed";

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

#[derive(Clone, PartialEq, Eq)]
enum StartOwner {
    Manual,
    Terminal(String),
}

struct ForwardEntry {
    session_id: i64,
    running: Option<RunningForward>,
    error: Option<String>,
}

struct RunningForward {
    owner: StartOwner,
    active: bool,
    stop: watch::Sender<bool>,
    done: watch::Receiver<()>,
    transport: Weak<ForwardTransport>,
    passphrases: Weak<[KeyPassphraseInput]>,
}

#[derive(Default)]
struct ForwardTransport {
    connection: OnceCell<Result<ForwardConnection, String>>,
    remote: bool,
}

impl ForwardTransport {
    fn is_usable(&self) -> bool {
        match self.connection.get() {
            None => true,
            Some(Ok(connection)) => !connection.is_closed(),
            Some(Err(_)) => false,
        }
    }

    fn close(&self) {
        if let Some(Ok(connection)) = self.connection.get() {
            connection.close();
        }
    }
}

pub struct PortForwardState {
    entries: Mutex<HashMap<i64, ForwardEntry>>,
}

impl PortForwardState {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn prepare_start(
        &self,
        forward: PortForward,
        owner: StartOwner,
        passphrases: Arc<[KeyPassphraseInput]>,
        deadline: Instant,
        reuse: Option<Arc<ForwardTransport>>,
        app: &AppHandle,
    ) -> Result<(), String> {
        let id = forward.id;
        let session_id = forward.session_id;
        let remote = forward.forward_type == "remote";
        let mut entries = self.entries.lock().map_err(|e| e.to_string())?;
        if let Some(running) = entries
            .get_mut(&id)
            .and_then(|entry| entry.running.as_mut())
        {
            if owner == StartOwner::Manual {
                running.owner = owner;
            }
            return Ok(());
        }

        // Local and SOCKS listeners share one connection per session. A remote
        // listener owns its transport so a lost SSH reply can be rolled back by
        // closing the connection that owns the server-side port.
        let transport = reuse
            .filter(|transport| transport.remote == remote && transport.is_usable())
            .or_else(|| {
                if remote {
                    return None;
                }
                entries.values().find_map(|entry| {
                    if entry.session_id != session_id {
                        return None;
                    }
                    let transport = entry.running.as_ref()?.transport.upgrade()?;
                    (!transport.remote && transport.is_usable()).then_some(transport)
                })
            })
            .unwrap_or_else(|| {
                Arc::new(ForwardTransport {
                    remote,
                    ..Default::default()
                })
            });

        let (stop, stop_rx) = watch::channel(false);
        let (done, done_rx) = watch::channel(());
        entries.insert(
            id,
            ForwardEntry {
                session_id,
                running: Some(RunningForward {
                    owner,
                    active: false,
                    stop,
                    done: done_rx,
                    transport: Arc::downgrade(&transport),
                    passphrases: Arc::downgrade(&passphrases),
                }),
                error: None,
            },
        );
        let app = app.clone();
        tokio::spawn(async move {
            let master_password = app.state::<MasterPasswordState>();
            let session_store = app.state::<SessionStore>();
            let connect = ssh::connect_forward_transport(
                session_id,
                &passphrases,
                &master_password,
                &session_store,
            );
            let result = run_forward(&forward, &transport, connect, deadline, stop_rx, || {
                app.state::<PortForwardState>().mark_active(id);
                let _ = app.emit(STATUS_CHANGED_EVENT, session_id);
            })
            .await;
            drop(transport);
            app.state::<PortForwardState>()
                .finish_forward_run(id, result);
            let _ = app.emit(STATUS_CHANGED_EVENT, session_id);
            drop(done);
        });
        Ok(())
    }

    fn mark_active(&self, id: i64) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(running) = entries
                .get_mut(&id)
                .and_then(|entry| entry.running.as_mut())
            {
                running.active = true;
            }
        }
    }

    fn finish_forward_run(&self, id: i64, result: Result<(), String>) {
        if let Ok(mut entries) = self.entries.lock() {
            let Some(entry) = entries.get_mut(&id) else {
                return;
            };
            let stopped = entry
                .running
                .as_ref()
                .is_some_and(|running| *running.stop.borrow());
            if let Err(error) = result {
                if !stopped {
                    entry.running = None;
                    entry.error = Some(error);
                    return;
                }
            }
            entries.remove(&id);
        }
    }

    pub fn cancel_forward(&self, id: i64, session_id: Option<i64>) {
        if let Ok(mut entries) = self.entries.lock() {
            let Some(entry) = entries.get(&id) else {
                return;
            };
            if session_id.is_some_and(|id| entry.session_id != id) {
                return;
            }
            if let Some(running) = &entry.running {
                running.stop.send_replace(true);
            } else {
                entries.remove(&id);
            }
        }
    }

    pub fn cancel_session(&self, session_id: i64, terminal_id: Option<&str>) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, entry| {
                if entry.session_id != session_id {
                    return true;
                }
                if let Some(running) = &entry.running {
                    if terminal_id.is_none_or(
                        |id| matches!(&running.owner, StartOwner::Terminal(owner) if owner == id),
                    ) {
                        running.stop.send_replace(true);
                    }
                    true
                } else {
                    terminal_id.is_some()
                }
            });
        }
    }

    pub fn get_status(
        &self,
        session_id: i64,
        session_store: &SessionStore,
    ) -> Result<Vec<ForwardStatus>, String> {
        let configs = session_store.list_port_forwards(session_id)?;
        let entries = self.entries.lock().map_err(|e| e.to_string())?;

        Ok(configs
            .into_iter()
            .map(|cfg| {
                let (status, error) = match entries.get(&cfg.id) {
                    Some(entry) => match &entry.running {
                        Some(running) => (if running.active { "active" } else { "starting" }, None),
                        None => ("failed", entry.error.clone()),
                    },
                    None => ("stopped", None),
                };
                ForwardStatus {
                    id: cfg.id,
                    forward_type: cfg.forward_type,
                    local_port: cfg.local_port,
                    local_host: cfg.local_host,
                    remote_host: cfg.remote_host,
                    remote_port: cfg.remote_port,
                    status: status.to_string(),
                    error,
                }
            })
            .collect())
    }
}

pub async fn restart_forward_if_running(
    updated: PortForward,
    pf_state: &PortForwardState,
    app: &AppHandle,
) -> Result<(), String> {
    let (owner, mut done, transport, passphrases) = {
        let entries = pf_state.entries.lock().map_err(|e| e.to_string())?;
        let Some(running) = entries
            .get(&updated.id)
            .and_then(|entry| entry.running.as_ref())
        else {
            return Ok(());
        };
        let transport = running.transport.upgrade();
        let passphrases = running
            .passphrases
            .upgrade()
            .unwrap_or_else(|| Arc::from([]));
        running.stop.send_replace(true);
        (
            running.owner.clone(),
            running.done.clone(),
            transport,
            passphrases,
        )
    };
    let _ = done.changed().await;
    pf_state.prepare_start(
        updated,
        owner,
        passphrases,
        Instant::now() + START_TIMEOUT,
        transport,
        app,
    )
}

enum StartedForward {
    Listener(TcpListener),
    Remote(u32),
}

fn unbracket_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

async fn wait_for_start<T>(
    startup: impl Future<Output = Result<T, String>>,
    stop: &mut watch::Receiver<bool>,
    deadline: Instant,
) -> Result<Option<T>, String> {
    tokio::select! {
        biased;
        _ = stop.changed() => Ok(None),
        _ = sleep_until(deadline) => Err(START_TIMEOUT_ERROR.to_string()),
        result = startup => result.map(Some),
    }
}

async fn run_forward(
    config: &PortForward,
    transport: &ForwardTransport,
    connect: impl Future<Output = Result<ForwardConnection, String>>,
    deadline: Instant,
    mut stop: watch::Receiver<bool>,
    on_started: impl FnOnce(),
) -> Result<(), String> {
    let host = match config.local_host.as_str() {
        "" => "127.0.0.1",
        host => unbracket_host(host),
    };
    let startup = async {
        let connection = transport
            .connection
            .get_or_init(|| connect)
            .await
            .as_ref()
            .map_err(Clone::clone)?;

        let started = if transport.remote {
            let port = config.local_port as u32;
            let target = |listen_port| RemoteForwardTarget {
                listen_port,
                host: unbracket_host(config.remote_host.as_deref().unwrap_or_default()).to_owned(),
                port: config.remote_port.unwrap_or_default(),
            };
            connection.set_remote_target(Some(target(port)));
            let allocated = connection
                .handle
                .tcpip_forward(host, port)
                .await
                .map_err(|e| format!("SSH server could not open the remote listener: {e}"))?;
            if port == 0 {
                connection.set_remote_target(Some(target(allocated)));
                StartedForward::Remote(allocated)
            } else {
                StartedForward::Remote(port)
            }
        } else {
            let listener = TcpListener::bind((host, config.local_port))
                .await
                .map_err(|e| format!("Cannot listen on {host}:{}: {e}", config.local_port))?;
            StartedForward::Listener(listener)
        };
        Ok((connection, started))
    };

    let (connection, started) = match wait_for_start(startup, &mut stop, deadline).await {
        Ok(Some(started)) => started,
        result => {
            if transport.remote {
                // The server may already have bound the port even if its reply
                // never arrived. Dropping the request alone cannot undo that.
                transport.close();
            }
            return result.map(|_| ());
        }
    };
    on_started();

    match started {
        StartedForward::Listener(listener) => {
            serve_listener(listener, connection, config, &mut stop).await
        }
        StartedForward::Remote(port) => {
            tokio::select! {
                _ = stop.changed() => {
                    connection.set_remote_target(None);
                    let cancelled = timeout(Duration::from_secs(2), connection.handle.cancel_tcpip_forward(host, port)).await;
                    if !matches!(cancelled, Ok(Ok(()))) {
                        connection.close();
                    }
                    Ok(())
                }
                _ = connection.wait_closed() => Err("SSH forwarding connection closed.".to_string()),
            }
        }
    }
}

async fn serve_listener(
    listener: TcpListener,
    connection: &ForwardConnection,
    config: &PortForward,
    stop: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let mut clients = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break Ok(()),
            _ = connection.wait_closed() => break Err("SSH forwarding connection closed.".to_string()),
            _ = clients.join_next(), if !clients.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let handle = connection.handle.clone();
                    let id = config.id;
                    if config.forward_type == "dynamic" {
                        clients.spawn(handle_socks5(stream, peer, handle, id));
                    } else {
                        let host = unbracket_host(config.remote_host.as_deref().unwrap_or_default()).to_owned();
                        let port = config.remote_port.unwrap_or_default();
                        clients.spawn(async move {
                            let deadline = Instant::now() + START_TIMEOUT;
                            if let Some(channel) = open_direct_channel(&handle, &host, port, peer, id, deadline).await {
                                bridge_tcp_channel(stream, channel).await;
                            }
                        });
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset) => {}
                Err(e) => break Err(format!("Port forward could not accept connections: {e}")),
            },
        }
    };
    drop(listener);
    clients.shutdown().await;
    result
}

pub(crate) async fn bridge_tcp_channel(
    mut stream: TcpStream,
    channel: russh::Channel<client::Msg>,
) {
    let mut channel_stream = channel.into_stream();
    if let Err(e) = copy_bidirectional(&mut stream, &mut channel_stream).await {
        log::debug!("port forward bridge closed with error: {e}");
    }
}

async fn open_direct_channel(
    handle: &ForwardHandle,
    host: &str,
    port: u16,
    peer: SocketAddr,
    id: i64,
    deadline: Instant,
) -> Option<russh::Channel<client::Msg>> {
    let origin = peer.ip().to_string();
    match timeout_at(
        deadline,
        handle.channel_open_direct_tcpip(host, port.into(), origin, peer.port().into()),
    )
    .await
    {
        Ok(Ok(channel)) => Some(channel),
        Ok(Err(e)) => {
            log::debug!("forward {id}: SSH direct-tcpip failed: {e}");
            None
        }
        Err(_) => {
            log::debug!("forward {id}: SSH direct-tcpip timed out");
            None
        }
    }
}

async fn read_socks_destination(stream: &mut TcpStream) -> std::io::Result<Option<(String, u16)>> {
    let mut header = [0; 2];
    stream.read_exact(&mut header).await?;
    if header[0] != 5 {
        return Ok(None);
    }
    let mut buffer = [0; 255];
    let methods = &mut buffer[..header[1] as usize];
    stream.read_exact(methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 0xff]).await?;
        return Ok(None);
    }
    stream.write_all(&[5, 0]).await?;

    let mut request = [0; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != 5 || request[2] != 0 {
        return Ok(None);
    }
    if request[1] != 1 {
        write_socks_reply(stream, 7).await?;
        return Ok(None);
    }

    let host = match request[3] {
        1 => {
            let mut bytes = [0; 4];
            stream.read_exact(&mut bytes).await?;
            Ipv4Addr::from(bytes).to_string()
        }
        3 => {
            let size = stream.read_u8().await? as usize;
            let name = &mut buffer[..size];
            stream.read_exact(name).await?;
            if name.is_empty() || !name.iter().all(u8::is_ascii_graphic) {
                write_socks_reply(stream, 8).await?;
                return Ok(None);
            }
            String::from_utf8_lossy(name).into_owned()
        }
        4 => {
            let mut bytes = [0; 16];
            stream.read_exact(&mut bytes).await?;
            Ipv6Addr::from(bytes).to_string()
        }
        _ => {
            write_socks_reply(stream, 8).await?;
            return Ok(None);
        }
    };
    let port = stream.read_u16().await?;
    Ok(Some((host, port)))
}

async fn write_socks_reply(stream: &mut TcpStream, code: u8) -> std::io::Result<()> {
    // SSH does not expose the server's outbound socket address.
    stream.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

async fn handle_socks5(mut stream: TcpStream, peer: SocketAddr, handle: ForwardHandle, id: i64) {
    let deadline = Instant::now() + START_TIMEOUT;
    let _ = stream.set_nodelay(true);
    let Ok(Ok(Some((host, port)))) =
        timeout_at(deadline, read_socks_destination(&mut stream)).await
    else {
        return;
    };
    let channel = open_direct_channel(&handle, &host, port, peer, id, deadline).await;
    let Some(channel) = channel else {
        let _ = write_socks_reply(&mut stream, 1).await;
        return;
    };
    if write_socks_reply(&mut stream, 0).await.is_ok() {
        bridge_tcp_channel(stream, channel).await;
    } else {
        let _ = channel.close().await;
    }
}

#[tauri::command]
pub async fn start_port_forwards(
    session_id: i64,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
    pf_state: tauri::State<'_, PortForwardState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    let passphrases: Arc<[_]> = key_passphrases.unwrap_or_default().into();
    for forward in session_store.list_port_forwards(session_id)? {
        pf_state.prepare_start(
            forward,
            StartOwner::Manual,
            passphrases.clone(),
            deadline,
            None,
            &app,
        )?;
    }
    Ok(())
}

#[tauri::command]
pub async fn auto_start_port_forwards(
    session_id: i64,
    terminal_id: String,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
    pf_state: tauri::State<'_, PortForwardState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    let passphrases: Arc<[_]> = key_passphrases.unwrap_or_default().into();
    let owner = StartOwner::Terminal(terminal_id);
    for forward in session_store
        .list_port_forwards(session_id)?
        .into_iter()
        .filter(|forward| forward.always_start)
    {
        pf_state.prepare_start(
            forward,
            owner.clone(),
            passphrases.clone(),
            deadline,
            None,
            &app,
        )?;
    }
    Ok(())
}

#[tauri::command]
pub async fn start_single_port_forward(
    session_id: i64,
    forward_id: i64,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
    pf_state: tauri::State<'_, PortForwardState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    let forward = session_store
        .list_port_forwards(session_id)?
        .into_iter()
        .find(|forward| forward.id == forward_id)
        .ok_or_else(|| format!("Port forward {forward_id} not found"))?;
    pf_state.prepare_start(
        forward,
        StartOwner::Manual,
        key_passphrases.unwrap_or_default().into(),
        deadline,
        None,
        &app,
    )
}

#[tauri::command]
pub async fn stop_port_forwards(
    session_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
) -> Result<(), String> {
    pf_state.cancel_session(session_id, None);
    Ok(())
}

#[tauri::command]
pub async fn auto_stop_port_forwards(
    session_id: i64,
    terminal_id: String,
    pf_state: tauri::State<'_, PortForwardState>,
) -> Result<(), String> {
    pf_state.cancel_session(session_id, Some(&terminal_id));
    Ok(())
}

#[tauri::command]
pub async fn stop_single_port_forward(
    session_id: i64,
    forward_id: i64,
    pf_state: tauri::State<'_, PortForwardState>,
) -> Result<(), String> {
    pf_state.cancel_forward(forward_id, Some(session_id));
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
