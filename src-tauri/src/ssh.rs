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
use tokio::sync::{mpsc, oneshot};

use crate::crypto;
use crate::crypto::MasterPasswordState;
use crate::port_forward::PortForwardState;
use crate::session_store::{Session, SessionStore};
use crate::sftp::SftpEditState;
use crate::terminal_events::TerminalExitEvent;

pub type SharedSshHandle = Arc<AsyncMutex<client::Handle<SshHandler>>>;
type DynamicAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

const HEADLESS_SSH_ID_PREFIX: &str = "__port_forward__";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPassphraseInput {
    pub session_id: i64,
    pub passphrase: String,
}

pub struct RemoteForwardMap {
    pub by_port: Mutex<HashMap<(String, u32), String>>,
}

impl RemoteForwardMap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            by_port: Mutex::new(HashMap::new()),
        })
    }
}

pub enum FwdCommand {
    StartLocal {
        session_id: i64,
        local_host: String,
        local_port: u16,
        remote_host: String,
        remote_port: u16,
        forward_id: i64,
        stop_rx: Option<tokio::sync::watch::Receiver<bool>>,
        app: tauri::AppHandle,
    },
    StartRemote {
        session_id: i64,
        local_host: String,
        local_port: u16,
        remote_host: String,
        remote_port: u16,
        forward_id: i64,
        stop_rx: Option<tokio::sync::watch::Receiver<bool>>,
        app: tauri::AppHandle,
    },
    StartDynamic {
        session_id: i64,
        local_host: String,
        local_port: u16,
        forward_id: i64,
        stop_rx: Option<tokio::sync::watch::Receiver<bool>>,
        app: tauri::AppHandle,
    },
}

pub struct SshState {
    pub sessions: Mutex<HashMap<String, SshSession>>,
    pub fwd_commands: Mutex<HashMap<i64, mpsc::UnboundedSender<FwdCommand>>>,
    pub ssh_handles: Mutex<HashMap<String, SharedSshHandle>>,
    pub headless_sessions: Mutex<HashSet<i64>>,
    pub remote_forwards: Arc<RemoteForwardMap>,
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
            fwd_commands: Mutex::new(HashMap::new()),
            ssh_handles: Mutex::new(HashMap::new()),
            headless_sessions: Mutex::new(HashSet::new()),
            remote_forwards: RemoteForwardMap::new(),
        }
    }
}

pub fn headless_ssh_id(session_id: i64) -> String {
    format!("{}{}", HEADLESS_SSH_ID_PREFIX, session_id)
}

pub fn has_attached_terminal(state: &SshState, session_id: i64) -> bool {
    if let Ok(sessions) = state.sessions.lock() {
        sessions.iter().any(|(id, session)| {
            session.session_id == session_id && id != &headless_ssh_id(session_id)
        })
    } else {
        false
    }
}

pub(crate) struct SshHandler {
    pub remote_forwards: Arc<RemoteForwardMap>,
    pub agent_forwarding: bool,
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
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let dest = {
            let map = self.remote_forwards.by_port.lock().ok();
            map.as_ref().and_then(|m| {
                let key = (connected_address.to_string(), connected_port);
                m.get(&key).cloned().or_else(|| {
                    // Fall back to port-only lookup (server may report different bind address)
                    m.iter()
                        .find(|((_, p), _)| *p == connected_port)
                        .map(|(_, v)| v.clone())
                })
            })
        };
        let Some(dest) = dest else { return Ok(()) };

        tokio::spawn(async move {
            match tokio::net::TcpStream::connect(&dest).await {
                Ok(tcp) => crate::port_forward::bridge_tcp_channel(tcp, channel).await,
                Err(e) => log::error!("remote forward: connect to {} failed: {}", dest, e),
            }
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

/// Dispatch a single port-forward command onto a freshly spawned task.
fn spawn_forward_command(cmd: FwdCommand, shared_handle: SharedSshHandle) {
    match cmd {
        FwdCommand::StartLocal {
            session_id,
            local_host,
            local_port,
            remote_host,
            remote_port,
            forward_id,
            stop_rx,
            app,
        } => {
            let h = shared_handle.clone();
            let stop_rx = stop_rx.unwrap_or_else(|| {
                let (_, rx) = tokio::sync::watch::channel(false);
                rx
            });
            tokio::spawn(async move {
                crate::port_forward::run_local_forward(
                    h,
                    session_id,
                    local_host,
                    local_port,
                    remote_host,
                    remote_port,
                    forward_id,
                    stop_rx,
                    app,
                )
                .await;
            });
        }
        FwdCommand::StartRemote {
            session_id,
            local_host,
            local_port,
            remote_host,
            remote_port,
            forward_id,
            stop_rx,
            app,
        } => {
            let h = shared_handle.clone();
            let stop_rx = stop_rx.unwrap_or_else(|| {
                let (_, rx) = tokio::sync::watch::channel(false);
                rx
            });
            tokio::spawn(async move {
                crate::port_forward::run_remote_forward(
                    h,
                    session_id,
                    local_host,
                    local_port,
                    remote_host,
                    remote_port,
                    forward_id,
                    stop_rx,
                    app,
                )
                .await;
            });
        }
        FwdCommand::StartDynamic {
            session_id,
            local_host,
            local_port,
            forward_id,
            stop_rx,
            app,
        } => {
            let h = shared_handle.clone();
            let stop_rx = stop_rx.unwrap_or_else(|| {
                let (_, rx) = tokio::sync::watch::channel(false);
                rx
            });
            tokio::spawn(async move {
                crate::port_forward::run_dynamic_forward(
                    h, session_id, local_host, local_port, forward_id, stop_rx, app,
                )
                .await;
            });
        }
    }
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
    handle: &AppHandle,
    sid: &str,
    session: &ResolvedSession,
    upstream: Option<SharedSshHandle>,
) -> Result<SharedSshHandle, ()> {
    let config = Arc::new(client::Config {
        nodelay: true,
        ..client::Config::default()
    });
    let handler = SshHandler {
        remote_forwards: handle.state::<SshState>().remote_forwards.clone(),
        agent_forwarding: session.auth_method == "agent" && session.agent_forwarding,
    };

    let mut ssh_session = match upstream {
        Some(upstream_handle) => {
            let channel = {
                let upstream = upstream_handle.lock().await;
                match upstream
                    .channel_open_direct_tcpip(
                        session.host.clone(),
                        session.port as u32,
                        "127.0.0.1".to_string(),
                        0,
                    )
                    .await
                {
                    Ok(channel) => channel,
                    Err(e) => {
                        emit_ssh_error(
                            handle,
                            sid,
                            format!(
                                "Failed to open jump host channel to {} ({}:{}): {}",
                                session.name, session.host, session.port, e
                            ),
                        );
                        return Err(());
                    }
                }
            };

            match client::connect_stream(config, channel.into_stream(), handler).await {
                Ok(session_handle) => session_handle,
                Err(e) => {
                    emit_ssh_error(
                        handle,
                        sid,
                        format!(
                            "SSH connection through jump host failed for {} ({}:{}): {}",
                            session.name, session.host, session.port, e
                        ),
                    );
                    return Err(());
                }
            }
        }
        None => match client::connect(config, (session.host.as_str(), session.port), handler).await
        {
            Ok(session_handle) => session_handle,
            Err(e) => {
                emit_ssh_error(
                    handle,
                    sid,
                    format!(
                        "SSH connection failed for {} ({}:{}): {}",
                        session.name, session.host, session.port, e
                    ),
                );
                return Err(());
            }
        },
    };

    let auth_result = match session.auth_method.as_str() {
        "password" => match session.password.as_ref() {
            Some(password) => {
                ssh_session
                    .authenticate_password(&session.username, password)
                    .await
            }
            None => {
                emit_ssh_error(
                    handle,
                    sid,
                    format!("No password stored for session {}", session.name),
                );
                return Err(());
            }
        },
        "key" => match session.private_key_path.as_ref() {
            Some(path) => {
                match russh::keys::load_secret_key(path, session.key_passphrase.as_deref()) {
                    Ok(key) => {
                        use russh::keys::PrivateKeyWithHashAlg;
                        let algorithm = key.algorithm();
                        let hash_alg = best_rsa_hash_alg(&ssh_session, &algorithm, "ssh key").await;
                        ssh_session
                            .authenticate_publickey(
                                &session.username,
                                PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
                            )
                            .await
                    }
                    Err(e) => {
                        emit_ssh_error(
                            handle,
                            sid,
                            format!(
                                "Failed to load private key for session {}: {}",
                                session.name, e
                            ),
                        );
                        return Err(());
                    }
                }
            }
            None => {
                emit_ssh_error(
                    handle,
                    sid,
                    format!(
                        "No private key path configured for session {}",
                        session.name
                    ),
                );
                return Err(());
            }
        },
        "agent" => match authenticate_with_agent(&mut ssh_session, &session.username).await {
            Ok(result) => Ok(result),
            Err(err) => {
                emit_ssh_error(
                    handle,
                    sid,
                    format!(
                        "SSH agent authentication failed for session {}: {}",
                        session.name, err
                    ),
                );
                return Err(());
            }
        },
        other => {
            emit_ssh_error(
                handle,
                sid,
                format!(
                    "Unknown auth method for session {}: {}",
                    session.name, other
                ),
            );
            return Err(());
        }
    };

    if !matches!(auth_result, Ok(AuthResult::Success)) {
        let err_msg = match auth_result {
            Err(e) => format!("Authentication error for session {}: {}", session.name, e),
            _ => format!("Authentication failed for session {}", session.name),
        };
        emit_ssh_error(handle, sid, err_msg);
        return Err(());
    }

    Ok(Arc::new(AsyncMutex::new(ssh_session)))
}

fn spawn_ssh_task(
    app: AppHandle,
    id: String,
    session_id: i64,
    cols: u16,
    rows: u16,
    connection_chain: Vec<ResolvedSession>,
    register_port_forward_transport: bool,
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
        let (transport_registered, exit_event) = async {
            let mut upstream: Option<SharedSshHandle> = None;
            let mut shared_handle = None;

            for session in &connection_chain {
                let next_handle =
                    match connect_authenticated_session(&handle, &sid, session, upstream.clone())
                        .await
                    {
                        Ok(next_handle) => next_handle,
                        Err(()) => {
                            return (
                                false,
                                if exit_requested.load(Ordering::SeqCst) {
                                    TerminalExitEvent::user_initiated()
                                } else {
                                    TerminalExitEvent::unexpected(default_exit_message.clone())
                                },
                            );
                        }
                    };
                upstream = Some(next_handle.clone());
                shared_handle = Some(next_handle);
            }

            let Some(shared_handle) = shared_handle else {
                emit_ssh_error(&handle, &sid, "No SSH session configured");
                return (false, TerminalExitEvent::unexpected(default_exit_message.clone()));
            };

            if let Ok(mut h) = handle.state::<SshState>().ssh_handles.lock() {
                h.insert(sid.clone(), shared_handle.clone());
            }

            let (fwd_tx, mut fwd_rx) = mpsc::unbounded_channel::<FwdCommand>();
            let mut transport_registered = false;
            if register_port_forward_transport {
                if let Ok(mut cmds) = handle.state::<SshState>().fwd_commands.lock() {
                    cmds.insert(session_id, fwd_tx);
                    transport_registered = true;
                }

                if transport_registered {
                    crate::port_forward::process_pending_forwards(&handle, session_id);
                }
            }

            if register_port_forward_transport {
                loop {
                    tokio::select! {
                        Some(_) = rx.recv() => {}
                        Some(_) = resize_rx.recv() => {}
                        Some(cmd) = fwd_rx.recv() => {
                            spawn_forward_command(cmd, shared_handle.clone());
                        }
                        _ = &mut abort_rx => {
                            log::info!("ssh {}: abort signal received", sid);
                            break;
                        }
                        else => break,
                    }
                }

                return (
                    transport_registered,
                    if exit_requested.load(Ordering::SeqCst) {
                        TerminalExitEvent::user_initiated()
                    } else {
                        TerminalExitEvent::unexpected(default_exit_message.clone())
                    },
                );
            }

            let mut channel = {
                let guard = shared_handle.lock().await;
                let ch = match guard.channel_open_session().await {
                    Ok(ch) => ch,
                    Err(e) => {
                        emit_ssh_error(&handle, &sid, format!("Failed to open channel: {}", e));
                        return (
                            transport_registered,
                            TerminalExitEvent::unexpected(default_exit_message.clone()),
                        );
                    }
                };

                if let Err(e) = ch
                    .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
                    .await
                {
                    emit_ssh_error(&handle, &sid, format!("Failed to request PTY: {}", e));
                    return (
                        transport_registered,
                        TerminalExitEvent::unexpected(default_exit_message.clone()),
                    );
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
                        return (
                            transport_registered,
                            TerminalExitEvent::unexpected(default_exit_message.clone()),
                        );
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
                    Some(msg) = channel.wait() => {
                        match msg {
                            ChannelMsg::Data { ref data } => {
                                if !data.is_empty() {
                                    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                                    let _ = handle.emit(&format!("pty-output-{}", sid), &encoded);
                                }
                            }
                            ChannelMsg::Eof | ChannelMsg::Close => {
                                log::info!("ssh {}: channel closed", sid);
                                break;
                            }
                            ChannelMsg::ExitStatus { exit_status } => {
                                exit_event = TerminalExitEvent::unexpected(format!(
                                    "[Process exited with code {}]",
                                    exit_status
                                ));
                                break;
                            }
                            _ => {}
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
                    Some(cmd) = fwd_rx.recv() => {
                        spawn_forward_command(cmd, shared_handle.clone());
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

            (transport_registered, exit_event)
        }.await;

        if transport_registered {
            handle
                .state::<PortForwardState>()
                .unregister_session(session_id, &handle.state::<SessionStore>());
            if let Ok(mut cmds) = handle.state::<SshState>().fwd_commands.lock() {
                cmds.remove(&session_id);
            }
        }
        if register_port_forward_transport {
            if let Ok(mut headless) = handle.state::<SshState>().headless_sessions.lock() {
                headless.remove(&session_id);
            }
        }
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
        session_id,
        cols,
        rows,
        connection_chain,
        false,
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

pub async fn connect_ssh_headless(
    session_id: i64,
    key_passphrases: Option<Vec<KeyPassphraseInput>>,
    master_pwd_state: &MasterPasswordState,
    state: &SshState,
    session_store: &SessionStore,
    app: &AppHandle,
) -> Result<(), String> {
    if state
        .fwd_commands
        .lock()
        .map_err(|e| e.to_string())?
        .contains_key(&session_id)
    {
        return Ok(());
    }

    {
        let mut headless = state.headless_sessions.lock().map_err(|e| e.to_string())?;
        if !headless.insert(session_id) {
            return Ok(());
        }
    }

    let connection_chain = match resolve_connection_chain(
        session_id,
        master_pwd_state,
        session_store,
        None,
        key_passphrases,
    ) {
        Ok(chain) => chain,
        Err(err) => {
            if let Ok(mut headless) = state.headless_sessions.lock() {
                headless.remove(&session_id);
            }
            return Err(err);
        }
    };

    let headless_id = headless_ssh_id(session_id);
    let exit_requested = Arc::new(AtomicBool::new(false));
    let (writer, resize_tx, abort_tx) = spawn_ssh_task(
        app.clone(),
        headless_id.clone(),
        session_id,
        80,
        24,
        connection_chain,
        true,
        exit_requested.clone(),
    );

    state.sessions.lock().map_err(|e| e.to_string())?.insert(
        headless_id,
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

pub fn disconnect_headless_ssh(session_id: i64, state: &SshState) -> Result<(), String> {
    disconnect_ssh_internal(&headless_ssh_id(session_id), state)
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
