use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use futures::future::BoxFuture;
use gateway_host::outbound::DnsResolver;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;

pub const ROOT: &[u8] = include_bytes!("certificates/root.der");

pub struct FixedDns(pub IpAddr);
impl DnsResolver for FixedDns {
    fn resolve(&self, _: String, port: u16) -> BoxFuture<'static, io::Result<Vec<SocketAddr>>> {
        let address = SocketAddr::new(self.0, port);
        Box::pin(async move { Ok(vec![address]) })
    }
}

#[derive(Debug)]
pub struct Observation {
    pub head: String,
    pub sni: Option<String>,
    pub target: Option<SocketAddr>,
    pub authentication: Option<(String, String)>,
}

#[derive(Clone, Copy)]
pub enum Mode {
    Origin,
    Http2Origin,
    HttpProxy,
    Socks,
}

pub struct Server {
    pub address: SocketAddr,
    received: mpsc::UnboundedReceiver<Observation>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn start(mode: Mode, tls: bool, bind: &str) -> Self {
        let listener = TcpListener::bind(bind).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, received) = mpsc::unbounded_channel();
        let acceptor = tls.then(|| tls_acceptor(matches!(mode, Mode::Http2Origin)));
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (stream, _) = result.unwrap();
                        let acceptor = acceptor.clone();
                        let sender = sender.clone();
                        connections.spawn(async move {
                            let (stream, sni): (Box<dyn Socket>, _) = if let Some(acceptor) = acceptor {
                                let Ok(stream) = acceptor.accept(stream).await else { return; };
                                let sni = stream.get_ref().1.server_name().map(str::to_owned);
                                (Box::new(stream), sni)
                            } else { (Box::new(stream), None) };
                            let result = tokio::time::timeout(Duration::from_secs(5), serve(stream, sni, mode, sender)).await;
                            if let Ok(Err(error)) = result { assert!(matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe), "fixture transport failed: {error}"); }
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        });
        Self {
            address,
            received,
            task,
        }
    }

    pub async fn next(&mut self) -> Observation {
        tokio::time::timeout(Duration::from_secs(3), self.received.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}

async fn serve(
    mut stream: Box<dyn Socket>,
    sni: Option<String>,
    mode: Mode,
    observations: mpsc::UnboundedSender<Observation>,
) -> io::Result<()> {
    if matches!(mode, Mode::Http2Origin) {
        return serve_http2(stream, sni, observations).await;
    }
    if matches!(mode, Mode::Socks) {
        return socks(stream, observations).await;
    }
    let head = read_head(&mut stream).await?;
    if matches!(mode, Mode::Origin) {
        observations
            .send(Observation {
                head,
                sni,
                target: None,
                authentication: None,
            })
            .unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nsecured")
            .await?;
        return stream.shutdown().await;
    }
    let mut first = head.lines().next().unwrap().split_whitespace();
    let method = first.next().unwrap();
    let uri: http::Uri = first.next().unwrap().parse().unwrap();
    let target: SocketAddr = uri
        .authority()
        .unwrap()
        .as_str()
        .parse()
        .expect("proxy must receive an IP, not a hostname");
    observations
        .send(Observation {
            head: head.clone(),
            sni,
            target: Some(target),
            authentication: None,
        })
        .unwrap();
    let mut upstream = TcpStream::connect(target).await?;
    if method == "CONNECT" {
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await?;
    } else {
        let path = uri.path_and_query().map_or("/", |path| path.as_str());
        let mut forwarded = format!("{method} {path} HTTP/1.1\r\n");
        for header in head.lines().skip(1).filter(|line| {
            !line.is_empty()
                && !line
                    .to_ascii_lowercase()
                    .starts_with("proxy-authorization:")
        }) {
            forwarded.push_str(header);
            forwarded.push_str("\r\n");
        }
        forwarded.push_str("\r\n");
        upstream.write_all(forwarded.as_bytes()).await?;
    }
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

async fn serve_http2(
    stream: Box<dyn Socket>,
    sni: Option<String>,
    observations: mpsc::UnboundedSender<Observation>,
) -> io::Result<()> {
    let service =
        hyper::service::service_fn(move |request: http::Request<hyper::body::Incoming>| {
            // 模拟 nginx 1.24：URI 已携带 :authority，额外的 Host 仍按重复头拒绝。
            let status = if request.headers().contains_key(http::header::HOST) {
                400
            } else {
                200
            };
            let mut head = format!(
                "{} {} {:?}\r\n",
                request.method(),
                request.uri(),
                request.version()
            );
            for (name, value) in request.headers() {
                head.push_str(&format!("{name}: {}\r\n", value.to_str().unwrap()));
            }
            observations
                .send(Observation {
                    head,
                    sni: sni.clone(),
                    target: None,
                    authentication: None,
                })
                .unwrap();
            let response = http::Response::builder()
                .status(status)
                .body(http_body_util::Full::new(bytes::Bytes::from_static(
                    b"secured",
                )))
                .unwrap();
            async { Ok::<_, std::convert::Infallible>(response) }
        });
    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .await
        .map_err(io::Error::other)
}

async fn read_head(stream: &mut Box<dyn Socket>) -> io::Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(stream.read_u8().await?);
        assert!(bytes.len() <= 8192);
    }
    Ok(String::from_utf8(bytes).unwrap())
}

async fn socks(
    mut stream: Box<dyn Socket>,
    observations: mpsc::UnboundedSender<Observation>,
) -> io::Result<()> {
    assert_eq!(stream.read_u8().await?, 5);
    let mut methods = vec![0; stream.read_u8().await? as usize];
    stream.read_exact(&mut methods).await?;
    assert!(methods.contains(&2));
    stream.write_all(&[5, 2]).await?;
    assert_eq!(stream.read_u8().await?, 1);
    let mut username = vec![0; stream.read_u8().await? as usize];
    stream.read_exact(&mut username).await?;
    let mut password = vec![0; stream.read_u8().await? as usize];
    stream.read_exact(&mut password).await?;
    stream.write_all(&[1, 0]).await?;
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).await?;
    assert_eq!(&prefix[..3], &[5, 1, 0]);
    let ip = match prefix[3] {
        1 => {
            let mut bytes = [0; 4];
            stream.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        4 => {
            let mut bytes = [0; 16];
            stream.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        _ => panic!("SOCKS proxy received an unpinned hostname"),
    };
    let target = SocketAddr::new(ip, stream.read_u16().await?);
    observations
        .send(Observation {
            head: String::new(),
            sni: None,
            target: Some(target),
            authentication: Some((
                String::from_utf8(username).unwrap(),
                String::from_utf8(password).unwrap(),
            )),
        })
        .unwrap();
    let mut upstream = TcpStream::connect(target).await?;
    stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

fn tls_acceptor(http2: bool) -> TlsAcceptor {
    use rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };
    let mut config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(
            include_bytes!("certificates/server.der").to_vec(),
        )],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            include_bytes!("certificates/server-key.der").to_vec(),
        )),
    )
    .unwrap();
    if http2 {
        config.alpn_protocols = vec![b"h2".to_vec()];
    }
    TlsAcceptor::from(Arc::new(config))
}
