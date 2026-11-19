use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use base64::Engine as _;
use russh::client;
use russh::client::AuthResult;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::{Algorithm, HashAlg};
use russh::ChannelMsg;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::copy_bidirectional;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, oneshot, watch};

use crate::crypto;
use crate::crypto::MasterPasswordState;
use crate::session_store::{Session, SessionStore};
use crate::sftp::SftpEditState;
use crate::terminal_events::TerminalExitEvent;

pub type SharedSshHandle = Arc<AsyncMutex<client::Handle<SshHandler>>>;
pub(crate) type ForwardHandle = Arc<client::Handle<SshHandler>>;
type DynamicAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPassphraseInput {
    pub session_id: i64,
    pub passphrase: String,
}

type RemoteForwardRoute = Arc<Mutex<Option<RemoteForwardTarget>>>;

pub(crate) struct RemoteForwardTarget {
    pub listen_port: u32,
    pub host: String,
    pub port: u16,
}

pub(crate) struct ForwardConnection {
    pub handle: ForwardHandle,
    socket: ForwardSocket,
    closed: watch::Receiver<()>,
    remote_target: RemoteForwardRoute,
}

impl ForwardConnection {
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed() || self.socket.closed.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.socket.close();
    }

    pub async fn wait_closed(&self) {
        let _ = self.closed.clone().changed().await;
    }

    pub fn set_remote_target(&self, target: Option<RemoteForwardTarget>) {
        if let Ok(mut current) = self.remote_target.lock() {
            *current = target;
        }
    }
}

struct ForwardSocket {
    socket: std::net::TcpStream,
    closed: AtomicBool,
}

impl ForwardSocket {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            let _ = self.socket.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for ForwardSocket {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct SshState {
    pub sessions: Mutex<HashMap<String, SshSession>>,
    pub ssh_handles: Mutex<HashMap<String, SharedSshHandle>>,
}

pub struct SshSession {
    pub writer: mpsc::UnboundedSender<Vec<u8>>,
    pub resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    pub abort: Option<oneshot::Sender<()>>,
    pub exit_requested: Arc<AtomicBool>,
    #[allow(dead_code)]
    pub session_id: i64,
}

impl SshState {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ssh_handles: Mutex::new(HashMap::new()),
        }
    }
}

pub(crate) struct SshHandler {
    pub agent_forwarding: bool,
    pub(crate) remote_forward: Option<RemoteForwardRoute>,
    pub(crate) _connected: Option<watch::Sender<()>>,
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let destination = self.remote_forward.as_ref().and_then(|route| {
            let route = route.lock().ok()?;
            let target = route.as_ref()?;
            (target.listen_port == 0 || target.listen_port == connected_port)
                .then(|| (target.host.clone(), target.port))
        });
        tokio::spawn(async move {
            if let Some((host, port)) = destination {
                let connection = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    tokio::net::TcpStream::connect((host.as_str(), port)),
                )
                .await;
                match connection {
                    Ok(Ok(tcp)) => {
                        crate::port_forward::bridge_tcp_channel(tcp, channel).await;
                        return;
                    }
                    error => {
                        log::debug!("remote forward: connect to {host}:{port} failed: {error:?}")
                    }
                }
            }
            let _ = channel.close().await;
        });
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if !self.agent_forwarding {
            return Ok(());
        }

        match connect_ssh_agent().await {
            Ok(agent) => {
                tokio::spawn(async move {
                    let mut agent_stream = agent.into_inner();
                    let mut channel_stream = channel.into_stream();
                    if let Err(err) =
                        copy_bidirectional(&mut channel_stream, &mut agent_stream).await
                    {
                        log::warn!("ssh agent forwarding bridge failed: {}", err);
                    }
                });
            }
            Err(err) => {
                log::warn!(
                    "ssh agent forwarding requested by server, but local agent is unavailable: {}",
                    err
                );
            }
        }

        Ok(())
    }
}

#[cfg(unix)]
async fn connect_ssh_agent() -> Result<DynamicAgentClient, String> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|e| format!("Failed to connect to SSH agent: {}", e))
}

#[cfg(windows)]
async fn connect_ssh_agent() -> Result<DynamicAgentClient, String> {
    let mut errors = Vec::new();

    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
        match AgentClient::connect_named_pipe(&sock).await {
            Ok(client) => return Ok(client.dynamic()),
            Err(e) => errors.push(format!("SSH_AUTH_SOCK {}: {}", sock, e)),
        }
    }

    match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(client) => return Ok(client.dynamic()),
        Err(e) => errors.push(format!("OpenSSH agent: {}", e)),
    }

    match AgentClient::connect_pageant().await {
        Ok(client) => Ok(client.dynamic()),
        Err(e) => {
            errors.push(format!("Pageant: {}", e));
            Err(format!(
                "Failed to connect to SSH agent: {}",
                errors.join("; ")
            ))
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn connect_ssh_agent() -> Result<DynamicAgentClient, String> {
    Err("SSH agent auth is not supported on this platform".to_string())
}

async fn best_rsa_hash_alg(
    ssh_session: &client::Handle<SshHandler>,
    algorithm: &Algorithm,
    log_context: &str,
) -> Option<HashAlg> {
    if matches!(algorithm, Algorithm::Rsa { .. }) {
        match ssh_session.best_supported_rsa_hash().await {
            Ok(hash_alg) => hash_alg.flatten(),
            Err(err) => {
                log::warn!(
                    "{}: failed to query server RSA algorithms: {}",
                    log_context,
                    err
                );
                None
            }
        }
    } else {
        None
    }
}

async fn authenticate_with_agent(
    ssh_session: &mut client::Handle<SshHandler>,
    username: &str,
) -> Result<AuthResult, String> {
    let mut agent = connect_ssh_agent().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("Failed to enumerate SSH agent identities: {}", e))?;

    if identities.is_empty() {
        return Err("SSH agent has no identities loaded".to_string());
    }

    let mut last_failure = None;
    let mut last_error = None;

    for identity in identities {
        let identity_algorithm = identity.public_key().algorithm();
        let hash_alg = best_rsa_hash_alg(ssh_session, &identity_algorithm, "ssh agent").await;
        let auth_result = match identity {
            AgentIdentity::PublicKey { key, .. } => ssh_session
                .authenticate_publickey_with(username, key, hash_alg, &mut agent)
                .await
                .map_err(|e| e.to_string()),
            AgentIdentity::Certificate { certificate, .. } => ssh_session
                .authenticate_certificate_with(username, certificate, hash_alg, &mut agent)
                .await
                .map_err(|e| e.to_string()),
        };

        match auth_result {
            Ok(AuthResult::Success) => return Ok(AuthResult::Success),
            Ok(failure @ AuthResult::Failure { .. }) => last_failure = Some(failure),
            Err(err) => {
                log::warn!("ssh agent: identity authentication attempt failed: {}", err);
                last_error = Some(err);
            }
        }
    }

    if let Some(failure) = last_failure {
        return Ok(failure);
    }

    Err(last_error.unwrap_or_else(|| "SSH agent authentication failed".to_string()))
}

#[derive(Clone)]
struct ResolvedSession {
    name: String,
    host: String,
    port: u16,
    username: String,
    auth_method: String,
    agent_forwarding: bool,
    password: Option<String>,
    private_key_path: Option<String>,
    key_passphrase: Option<String>,
    startup_command: Option<String>,
}

fn emit_ssh_error(handle: &AppHandle, sid: &str, message: impl AsRef<str>) {
    let msg = format!("\r\n\x1b[31m{}\x1b[0m", message.as_ref());
    let encoded = base64::engine::general_purpose::STANDARD.encode(msg);
    let _ = handle.emit(&format!("pty-output-{}", sid), &encoded);
}

fn resolve_session_chain(
    target_session_id: i64,
    session_store: &SessionStore,
) -> Result<Vec<Session>, String> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current_session_id = Some(target_session_id);

    while let Some(session_id) = current_session_id {
        if !seen.insert(session_id) {
            return Err("Jump host cycle detected in session configuration".to_string());
        }

        let session = session_store.get_session(session_id)?;
        current_session_id = session.jump_host_session_id;
        chain.push(session);
    }

    chain.reverse();
    Ok(chain)
}

fn resolve_session_password(
    session: &Session,
    master_pwd_state: &MasterPasswordState,
    session_store: &SessionStore,
) -> Result<Option<String>, String> {
    if session.auth_method != "password" {
        return Ok(None);
    }

    crypto::get_encrypted_password(session_store, master_pwd_state, session.id)
        .map(Some)
        .map_err(|_| {
            format!(
                "No password stored for session {} (master password may be locked)",
                session.name
            )
        })
}

fn resolve_connection_chain(
    target_session_id: i64,
    master_pwd_state: &MasterPasswordState,
    session_store: &SessionStore,
    startup_command: Option<String>,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
) -> Result<Vec<ResolvedSession>, String> {
    let sessions = resolve_session_chain(target_session_id, session_store)?;
    let target_index = sessions.len().saturating_sub(1);
    let key_passphrases: HashMap<i64, String> = key_passphrases
        .unwrap_or_default()
        .into_iter()
        .map(|item| (item.session_id, item.passphrase))
        .collect();

    sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| {
            let password = resolve_session_password(&session, master_pwd_state, session_store)?;
            let key_passphrase = key_passphrases.get(&session.id).cloned();
            Ok(ResolvedSession {
                name: session.name.clone(),
                host: session.host,
                port: session.port,
                username: session.username,
                auth_method: session.auth_method,
                agent_forwarding: session.agent_forwarding,
                password,
                private_key_path: session.private_key_path,
                key_passphrase,
                startup_command: if index == target_index {
                    startup_command.clone()
                } else {
                    None
                },
            })
        })
        .collect()
}

async fn connect_authenticated_session(
    session: &ResolvedSession,
    upstream: Option<&client::Handle<SshHandler>>,
    forward_socket: Option<&mut Option<ForwardSocket>>,
    handler: SshHandler,
) -> Result<client::Handle<SshHandler>, String> {
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        keepalive_max: 3,
        ..client::Config::default()
    });
    let address = (session.host.as_str(), session.port);
    let describe_connect_error = |error: anyhow::Error| {
        format!(
            "SSH connection failed for {} ({}:{}): {error}",
            session.name, session.host, session.port
        )
    };
    let mut ssh_session = if let Some(upstream) = upstream {
        let channel = upstream
            .channel_open_direct_tcpip(address.0, address.1.into(), "127.0.0.1", 0)
            .await
            .map_err(|error| {
                format!(
                    "Failed to open jump host channel to {} ({}:{}): {error}",
                    session.name, session.host, session.port
                )
            })?;
        client::connect_stream(config, channel.into_stream(), handler)
            .await
            .map_err(describe_connect_error)?
    } else if let Some(forward_socket) = forward_socket {
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .and_then(|socket| {
                socket.set_nodelay(true)?;
                socket.into_std()
            })
            .map_err(|error| describe_connect_error(error.into()))?;
        *forward_socket = Some(ForwardSocket {
            socket: socket
                .try_clone()
                .map_err(|error| describe_connect_error(error.into()))?,
            closed: AtomicBool::new(false),
        });
        let socket = tokio::net::TcpStream::from_std(socket)
            .map_err(|error| describe_connect_error(error.into()))?;
        client::connect_stream(config, socket, handler)
            .await
            .map_err(describe_connect_error)?
    } else {
        client::connect(config, address, handler)
            .await
            .map_err(describe_connect_error)?
    };

    let auth_result = match session.auth_method.as_str() {
        "password" => {
            let password = session
                .password
                .as_ref()
                .ok_or_else(|| format!("No password stored for session {}", session.name))?;
            ssh_session
                .authenticate_password(&session.username, password)
                .await
        }
        "key" => {
            let path = session.private_key_path.clone().ok_or_else(|| {
                format!(
                    "No private key path configured for session {}",
                    session.name
                )
            })?;
            let passphrase = session.key_passphrase.clone();
            let key = tokio::task::spawn_blocking(move || {
                russh::keys::load_secret_key(&path, passphrase.as_deref())
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| {
                format!(
                    "Failed to load private key for session {}: {e}",
                    session.name
                )
            })?;
            let hash_alg = best_rsa_hash_alg(&ssh_session, &key.algorithm(), "ssh key").await;
            ssh_session
                .authenticate_publickey(
                    &session.username,
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
                )
                .await
        }
        "agent" => Ok(authenticate_with_agent(&mut ssh_session, &session.username)
            .await
            .map_err(|e| {
                format!(
                    "SSH agent authentication failed for session {}: {e}",
                    session.name
                )
            })?),
        other => {
            return Err(format!(
                "Unknown auth method for session {}: {other}",
                session.name
            ))
        }
    };

    match auth_result {
        Ok(AuthResult::Success) => Ok(ssh_session),
        Ok(_) => Err(format!(
            "Authentication failed for session {}",
            session.name
        )),
        Err(e) => Err(format!(
            "Authentication error for session {}: {e}",
            session.name
        )),
    }
}

pub(crate) async fn connect_forward_transport(
    session_id: i64,
    passphrases: &[KeyPassphraseInput],
    master_pwd_state: &MasterPasswordState,
    session_store: &SessionStore,
) -> Result<ForwardConnection, String> {
    let chain = resolve_connection_chain(
        session_id,
        master_pwd_state,
        session_store,
        None,
        Some(passphrases.to_vec()),
    )?;
    connect_forward_chain(&chain).await
}

async fn connect_forward_chain(chain: &[ResolvedSession]) -> Result<ForwardConnection, String> {
    let mut socket = None;
    let mut upstream = None;
    let (connected, closed) = watch::channel(());
    let remote_target = Arc::new(Mutex::new(None));
    for (index, session) in chain.iter().enumerate() {
        let is_last = index + 1 == chain.len();
        let handler = SshHandler {
            remote_forward: is_last.then(|| remote_target.clone()),
            agent_forwarding: false,
            _connected: is_last.then(|| connected.clone()),
        };
        upstream = Some(
            connect_authenticated_session(session, upstream.as_ref(), Some(&mut socket), handler)
                .await?,
        );
    }

    Ok(ForwardConnection {
        handle: Arc::new(upstream.ok_or("No SSH session configured")?),
        socket: socket.ok_or("No SSH transport socket")?,
        closed,
        remote_target,
    })
}

#[cfg(test)]
pub(crate) async fn connect_test_forward_transport(
    addr: std::net::SocketAddr,
) -> Result<ForwardConnection, String> {
    connect_forward_chain(&[ResolvedSession {
        name: "test SSH".to_string(),
        host: addr.ip().to_string(),
        port: addr.port(),
        username: "test".to_string(),
        auth_method: "password".to_string(),
        agent_forwarding: false,
        password: Some("test".to_string()),
        private_key_path: None,
        key_passphrase: None,
        startup_command: None,
    }])
    .await
}

fn spawn_ssh_task(
    app: AppHandle,
    id: String,
    cols: u16,
    rows: u16,
    connection_chain: Vec<ResolvedSession>,
    exit_requested: Arc<AtomicBool>,
) -> (
    mpsc::UnboundedSender<Vec<u8>>,
    mpsc::UnboundedSender<(u16, u16)>,
    oneshot::Sender<()>,
) {
    let (writer, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();
    let (abort_tx, mut abort_rx) = oneshot::channel::<()>();

    let handle = app.clone();
    let sid = id.clone();

    tokio::spawn(async move {
        let default_exit_message = "[Process exited]".to_string();
        let exit_event = async {
            let mut upstream = None;
            for session in &connection_chain {
                let handler = SshHandler {
                    remote_forward: None,
                    agent_forwarding: session.auth_method == "agent" && session.agent_forwarding,
                    _connected: None,
                };
                match connect_authenticated_session(session, upstream.as_ref(), None, handler).await {
                    Ok(next) => upstream = Some(next),
                    Err(error) => {
                        emit_ssh_error(&handle, &sid, error);
                        return if exit_requested.load(Ordering::SeqCst) {
                            TerminalExitEvent::user_initiated()
                        } else {
                            TerminalExitEvent::unexpected(default_exit_message.clone())
                        };
                    }
                }
            }
            let Some(ssh_handle) = upstream else {
                emit_ssh_error(&handle, &sid, "No SSH session configured");
                return TerminalExitEvent::unexpected(default_exit_message.clone());
            };
            let shared_handle = Arc::new(AsyncMutex::new(ssh_handle));
            if let Ok(mut handles) = handle.state::<SshState>().ssh_handles.lock() {
                handles.insert(sid.clone(), shared_handle.clone());
            }

            let mut channel = {
                let guard = shared_handle.lock().await;
                let ch = match guard.channel_open_session().await {
                    Ok(ch) => ch,
                    Err(e) => {
                        emit_ssh_error(&handle, &sid, format!("Failed to open channel: {}", e));
                        return TerminalExitEvent::unexpected(default_exit_message.clone());
                    }
                };

                if let Err(e) = ch
                    .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
                    .await
                {
                    emit_ssh_error(&handle, &sid, format!("Failed to request PTY: {}", e));
                    return TerminalExitEvent::unexpected(default_exit_message.clone());
                }

                if connection_chain
                    .last()
                    .map(|session| session.auth_method == "agent" && session.agent_forwarding)
                    .unwrap_or(false)
                {
                    if let Err(e) = ch.agent_forward(true).await {
                        emit_ssh_error(&handle, &sid, format!("Failed to enable SSH agent forwarding: {}", e));
                    }
                }

                if let Err(shell_err) = ch.request_shell(true).await {
                    if let Err(exec_err) = ch.exec(true, "bash".as_bytes()).await {
                        emit_ssh_error(
                            &handle,
                            &sid,
                            format!("Failed to start shell: {}; fallback exec failed: {}", shell_err, exec_err),
                        );
                        return TerminalExitEvent::unexpected(default_exit_message.clone());
                    }
                }

                if let Some(command) = connection_chain
                    .last()
                    .and_then(|session| session.startup_command.as_deref())
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                {
                    if let Err(e) = ch.data(format!("{command}\n").as_bytes()).await {
                        emit_ssh_error(&handle, &sid, format!("Failed to run startup command: {}", e));
                    }
                }
                ch
            };
            let mut exit_event = TerminalExitEvent::unexpected(default_exit_message.clone());

            loop {
                tokio::select! {
                    msg = channel.wait() => {
                        match msg {
                            Some(ChannelMsg::Data { ref data }) => {
                                if !data.is_empty() {
                                    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                                    let _ = handle.emit(&format!("pty-output-{}", sid), &encoded);
                                }
                            }
                            // None means the transport died (e.g. keepalive timeout);
                            // treat it like a closed channel instead of stalling the loop.
                            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                                log::info!("ssh {}: channel closed", sid);
                                break;
                            }
                            Some(ChannelMsg::ExitStatus { exit_status }) => {
                                exit_event = TerminalExitEvent::unexpected(format!(
                                    "[Process exited with code {}]",
                                    exit_status
                                ));
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                    Some(data) = rx.recv() => {
                        if let Err(e) = channel.data(&data[..]).await {
                            log::error!("ssh {}: write error: {}", sid, e);
                            exit_event = TerminalExitEvent::unexpected(default_exit_message.clone());
                            break;
                        }
                    }
                    Some((new_cols, new_rows)) = resize_rx.recv() => {
                        let _ = channel
                            .window_change(new_cols as u32, new_rows as u32, 0, 0)
                            .await;
                    }
                    _ = &mut abort_rx => {
                        log::info!("ssh {}: abort signal received", sid);
                        exit_event = TerminalExitEvent::user_initiated();
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                        break;
                    }
                    else => break,
                }
            }

            exit_event
        }.await;

        if let Ok(mut h) = handle.state::<SshState>().ssh_handles.lock() {
            h.remove(&sid);
        }
        let _ = handle.state::<SftpEditState>().stop_terminal(&sid);
        crate::sftp::invalidate_sftp_cache(&sid);
        if let Ok(mut sessions) = handle.state::<SshState>().sessions.lock() {
            sessions.remove(&sid);
        }
        let _ = handle.emit(&format!("pty-exit-{}", sid), exit_event);
    });

    (writer, resize_tx, abort_tx)
}

#[tauri::command]
pub async fn connect_ssh(
    id: String,
    session_id: i64,
    cols: u16,
    rows: u16,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
    master_pwd_state: tauri::State<'_, MasterPasswordState>,
    state: tauri::State<'_, SshState>,
    session_store: tauri::State<'_, SessionStore>,
    app: AppHandle,
) -> Result<(), String> {
    let session = session_store.get_session(session_id)?;
    let connection_chain = resolve_connection_chain(
        session_id,
        &master_pwd_state,
        &session_store,
        session.startup_command.clone(),
        key_passphrases,
    )?;
    let exit_requested = Arc::new(AtomicBool::new(false));

    let (writer, resize_tx, abort_tx) = spawn_ssh_task(
        app.clone(),
        id.clone(),
        cols,
        rows,
        connection_chain,
        exit_requested.clone(),
    );

    state.sessions.lock().map_err(|e| e.to_string())?.insert(
        id.clone(),
        SshSession {
            writer,
            resize_tx,
            abort: Some(abort_tx),
            exit_requested,
            session_id,
        },
    );

    Ok(())
}

#[tauri::command]
pub fn disconnect_ssh(id: String, state: tauri::State<SshState>) -> Result<(), String> {
    disconnect_ssh_internal(&id, &state)
}

pub fn disconnect_ssh_internal(id: &str, state: &SshState) -> Result<(), String> {
    crate::sftp::invalidate_sftp_cache(id);
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(session) = sessions.remove(id) {
        session.exit_requested.store(true, Ordering::SeqCst);
        if let Some(abort) = session.abort {
            let _ = abort.send(());
        }
    }
    Ok(())
}

pub fn write_ssh_internal(id: &str, data: &str, state: &SshState) -> Result<(), String> {
    let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(session) = sessions.get(id) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| e.to_string())?;
        session
            .writer
            .send(bytes)
            .map_err(|_| "SSH session writer closed".to_string())?;
    }
    Ok(())
}

pub fn resize_ssh_internal(id: &str, cols: u16, rows: u16, state: &SshState) -> Result<(), String> {
    let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(session) = sessions.get(id) {
        session
            .resize_tx
            .send((cols, rows))
            .map_err(|_| "SSH resize channel closed".to_string())?;
    }
    Ok(())
}
