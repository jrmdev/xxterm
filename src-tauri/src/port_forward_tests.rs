use super::*;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::{server, Channel};
use std::sync::Arc;
use tokio::sync::oneshot;

struct EchoServer;

impl server::Handler for EchoServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        origin_host: &str,
        origin_port: u32,
        _: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        if host != "echo.test" || port != 80 || origin_host != "127.0.0.1" || origin_port == 0 {
            return Ok(false);
        }
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let mut message = [0; 4];
            stream.read_exact(&mut message).await.unwrap();
            stream.write_all(&message).await.unwrap();
        });
        Ok(true)
    }
}

async fn run_test_ssh(
    handler: impl server::Handler + Send + 'static,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let config = Arc::new(server::Config {
            keys: vec![Ed25519Keypair::from_seed(&[7; 32]).into()],
            ..Default::default()
        });
        server::run_stream(config, socket, handler)
            .await
            .unwrap_or_else(|_| {
                panic!("Failed to start loopback SSH test server");
            })
            .await
            .ok();
    });
    (addr, task)
}

#[tokio::test]
async fn socks_connect_relays_traffic_with_the_clients_originator_address() {
    timeout(Duration::from_secs(5), async {
        let (ssh_addr, _) = run_test_ssh(EchoServer).await;
        let mut handle = client::connect(
            Arc::default(),
            ssh_addr,
            ssh::SshHandler {
                remote_forward: None,
                agent_forwarding: false,
                _connected: None,
            },
        )
        .await
        .unwrap();
        assert!(handle.authenticate_none("test").await.unwrap().success());

        let proxy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut client = TcpStream::connect(proxy.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, peer) = proxy.accept().await.unwrap();
        let task = tokio::spawn(handle_socks5(stream, peer, Arc::new(handle), 1));

        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0; 2];
        client.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);

        client
            .write_all(b"\x05\x01\x00\x03\x09echo.test\x00\x50ping")
            .await
            .unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0, "SOCKS CONNECT was denied");
        let mut echoed = [0; 4];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        drop(client);
        task.await.unwrap();
    })
    .await
    .unwrap();
}

fn config(kind: &str, port: u16) -> PortForward {
    PortForward {
        id: 1,
        session_id: 1,
        forward_type: kind.to_string(),
        local_port: port,
        local_host: String::new(),
        remote_host: None,
        remote_port: None,
        always_start: false,
    }
}

#[tokio::test]
async fn local_bind_failure_leaves_the_transport_usable_for_retry() {
    timeout(Duration::from_secs(5), async {
        let (ssh_addr, _) = run_test_ssh(EchoServer).await;
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = occupied.local_addr().unwrap();
        let forward = config("local", addr.port());
        let transport = ForwardTransport::default();
        let (stop, stop_rx) = watch::channel(false);
        let result = run_forward(
            &forward,
            &transport,
            ssh::connect_test_forward_transport(ssh_addr),
            Instant::now() + START_TIMEOUT,
            stop_rx,
            || panic!("Bound an occupied port"),
        )
        .await;
        assert!(result.unwrap_err().starts_with("Cannot listen on"));
        assert!(transport.is_usable());
        drop(occupied);

        let (ready, started) = oneshot::channel();
        let retry = run_forward(
            &forward,
            &transport,
            std::future::pending(),
            Instant::now() + START_TIMEOUT,
            stop.subscribe(),
            || ready.send(()).unwrap(),
        );
        let check = async {
            started.await.unwrap();
            assert!(TcpListener::bind(addr).await.is_err());
            stop.send_replace(true);
        };
        let (result, ()) = tokio::join!(retry, check);
        result.unwrap();
        assert!(TcpListener::bind(addr).await.is_ok());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stalled_ssh_handshake_times_out_and_closes_the_socket() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_keep_running, stop_rx) = watch::channel(false);

    let forward = config("dynamic", 0);
    let transport = ForwardTransport::default();
    let startup = run_forward(
        &forward,
        &transport,
        ssh::connect_test_forward_transport(addr),
        Instant::now() + Duration::from_millis(250),
        stop_rx,
        || panic!("The forward started before the SSH handshake"),
    );
    let peer = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        socket.read_to_end(&mut received).await.unwrap();
        assert!(received.starts_with(b"SSH-2.0-"));
    };
    let (result, _) = timeout(Duration::from_secs(5), async {
        tokio::join!(startup, peer)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), START_TIMEOUT_ERROR);
}

struct DelayedRemoteListener {
    bound: Option<oneshot::Sender<SocketAddr>>,
    reply: Option<oneshot::Receiver<()>>,
    listener: Option<TcpListener>,
}

impl server::Handler for DelayedRemoteListener {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        let listener = TcpListener::bind((address, *port as u16)).await.unwrap();
        let bound = listener.local_addr().unwrap();
        *port = bound.port().into();
        self.listener = Some(listener);
        self.bound.take().unwrap().send(bound).unwrap();
        self.reply.take().unwrap().await.unwrap();
        Ok(true)
    }
}

#[tokio::test]
async fn remote_start_timeout_releases_a_listener_bound_before_the_ssh_reply() {
    timeout(Duration::from_secs(5), async {
        let (bound_tx, bound_rx) = oneshot::channel();
        let (reply_tx, reply_rx) = oneshot::channel();
        let (ssh_addr, server) = run_test_ssh(DelayedRemoteListener {
            bound: Some(bound_tx),
            reply: Some(reply_rx),
            listener: None,
        })
        .await;
        let transport = ForwardTransport {
            remote: true,
            ..Default::default()
        };
        let connection = ssh::connect_test_forward_transport(ssh_addr).await.unwrap();
        assert!(transport.connection.set(Ok(connection)).is_ok());
        let (_keep_running, stop) = watch::channel(false);
        let forward = config("remote", 0);
        let startup = run_forward(
            &forward,
            &transport,
            std::future::pending(),
            Instant::now() + Duration::from_millis(350),
            stop,
            || panic!("The forward started before the SSH reply"),
        );
        let (result, bound) = tokio::join!(startup, bound_rx);
        assert_eq!(result.unwrap_err(), START_TIMEOUT_ERROR);
        assert!(!transport.is_usable());
        reply_tx.send(()).unwrap();
        server.await.unwrap();
        let listener = TcpListener::bind(bound.unwrap()).await.unwrap();
        drop(listener);
    })
    .await
    .unwrap();
}
