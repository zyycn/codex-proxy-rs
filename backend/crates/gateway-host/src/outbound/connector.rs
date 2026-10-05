//! 受管出站连接器，建立直连或代理隧道并完成 TLS 连接

use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use base64::Engine as _;
use futures::future::BoxFuture;
use gateway_core::account::OutboundProxy;
use http::{HeaderValue, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder, MaybeHttpsStream};
use hyper_util::{
    client::legacy::connect::{
        Connected, HttpConnector,
        proxy::{SocksV5, Tunnel},
    },
    rt::TokioIo,
};
use rustls::{ClientConfig, RootCertStore, pki_types::CertificateDer};
use tokio::net::TcpStream;
use tower_service::Service;

use super::HttpError;

type Socket = TokioIo<TcpStream>;
type ProxySocket = MaybeHttpsStream<Socket>;

pub(super) struct Connection {
    socket: ProxySocket,
    forward_proxy: bool,
}

impl hyper_util::client::legacy::connect::Connection for Connection {
    fn connected(&self) -> Connected {
        self.socket.connected().proxy(self.forward_proxy)
    }
}

impl hyper::rt::Read for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_read(cx, buffer)
    }
}

impl hyper::rt::Write for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.socket.is_write_vectored()
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write_vectored(cx, buffers)
    }
}

#[derive(Clone)]
pub(super) struct Connector {
    addresses: Arc<[SocketAddr]>,
    proxy: Option<OutboundProxy>,
    proxy_tls: Arc<ClientConfig>,
}

impl Connector {
    pub(super) fn new(
        addresses: Vec<SocketAddr>,
        proxy: Option<OutboundProxy>,
        tls: Arc<ClientConfig>,
    ) -> Self {
        Self {
            addresses: addresses.into(),
            proxy,
            proxy_tls: tls,
        }
    }

    async fn connect(self, destination: Uri) -> Result<ProxySocket, HttpError> {
        let Some(proxy) = self.proxy else {
            return TcpStream::connect(self.addresses.as_ref())
                .await
                .map(|stream| MaybeHttpsStream::Http(TokioIo::new(stream)))
                .map_err(|_| HttpError::invalid("connect"));
        };
        let proxy_url =
            reqwest::Url::parse(proxy.expose_url()).map_err(|_| HttpError::invalid("proxy"))?;
        let proxy_uri: Uri = proxy
            .endpoint()
            .parse()
            .map_err(|_| HttpError::invalid("proxy"))?;
        let credentials = proxy_credentials(&proxy_url)?;
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(Duration::from_secs(10)));
        if matches!(proxy_url.scheme(), "http" | "https") {
            let mut connector = https(http, self.proxy_tls.as_ref().clone(), false);
            if destination.scheme_str() == Some("http") {
                return connector
                    .call(proxy_uri)
                    .await
                    .map_err(|_| HttpError::invalid("proxy connect"));
            }
            let mut tunnel = Tunnel::new(proxy_uri, connector);
            if let Some((username, password)) = credentials {
                tunnel = tunnel.with_auth(basic_auth(&username, &password)?);
            }
            for address in self.addresses.iter() {
                let pinned = address_uri(destination.scheme_str().unwrap_or("https"), *address)?;
                if let Ok(stream) = tunnel.call(pinned).await {
                    return Ok(stream);
                }
            }
        } else {
            let mut socks = SocksV5::new(proxy_uri, http);
            if let Some((username, password)) = credentials {
                socks = socks.with_auth(username, password);
            }
            for address in self.addresses.iter() {
                let pinned = address_uri(destination.scheme_str().unwrap_or("http"), *address)?;
                if let Ok(stream) = socks.call(pinned).await {
                    return Ok(MaybeHttpsStream::Http(stream));
                }
            }
        }
        Err(HttpError::invalid("proxy tunnel"))
    }
}

impl Service<Uri> for Connector {
    type Response = Connection;
    type Error = HttpError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connector = self.clone();
        let forward_proxy = uri.scheme_str() == Some("http")
            && connector
                .proxy
                .as_ref()
                .is_some_and(|proxy| proxy.expose_url().starts_with("http"));
        Box::pin(async move {
            let socket = tokio::time::timeout(Duration::from_secs(10), connector.connect(uri))
                .await
                .map_err(|_| HttpError::invalid("connect timeout"))??;
            Ok(Connection {
                socket,
                forward_proxy,
            })
        })
    }
}

pub(super) fn https<C>(connector: C, tls: ClientConfig, http2: bool) -> HttpsConnector<C> {
    let builder = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1();
    if http2 {
        builder.enable_http2().wrap_connector(connector)
    } else {
        builder.wrap_connector(connector)
    }
}

pub(super) fn address_uri(scheme: &str, address: SocketAddr) -> Result<Uri, HttpError> {
    format!("{scheme}://{address}")
        .parse()
        .map_err(|_| HttpError::invalid("address URI"))
}

pub(super) fn basic_auth(username: &str, password: &str) -> Result<HeaderValue, HttpError> {
    let value = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    let mut value = HeaderValue::from_str(&format!("Basic {value}"))
        .map_err(|_| HttpError::invalid("proxy credentials"))?;
    value.set_sensitive(true);
    Ok(value)
}

pub(super) fn proxy_credentials(
    proxy: &reqwest::Url,
) -> Result<Option<(String, String)>, HttpError> {
    if proxy.username().is_empty() && proxy.password().is_none() {
        return Ok(None);
    }
    let decode = |value: &str| {
        percent_encoding::percent_decode_str(value)
            .decode_utf8()
            .map(|value| value.into_owned())
            .map_err(|_| HttpError::invalid("proxy credentials"))
    };
    Ok(Some((
        decode(proxy.username())?,
        decode(proxy.password().unwrap_or(""))?,
    )))
}

pub(super) fn tls(additional_roots: Vec<Vec<u8>>) -> Result<Arc<ClientConfig>, HttpError> {
    let mut roots = RootCertStore::empty();
    let certificates = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(certificates.certs);
    for certificate in additional_roots {
        roots
            .add(CertificateDer::from(certificate))
            .map_err(|_| HttpError::invalid("root certificate"))?;
    }
    if roots.is_empty() {
        return Err(HttpError::invalid("trust roots"));
    }
    let tls = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| HttpError::invalid("TLS versions"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(tls))
}
