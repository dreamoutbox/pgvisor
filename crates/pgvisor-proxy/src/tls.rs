use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use pgvisor_core::tls::TlsCertPair;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream as ClientTlsStream, server::TlsStream as ServerTlsStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::info;

#[derive(Debug, Error)]
pub enum ProxyTlsError {
    #[error("I/O error during TLS configuration: {0}")]
    Io(#[from] io::Error),

    #[error("Certificate generation or loading error: {0}")]
    Cert(#[from] pgvisor_core::tls::TlsCertError),

    #[error("Rustls error: {0}")]
    Rustls(#[from] rustls::Error),

    #[error("No private key found in key file: {0}")]
    NoPrivateKey(String),
}

/// Client-side stream connecting the external client to pgvisor-proxy.
pub enum ClientStream {
    Plain(TcpStream),
    Tls(ServerTlsStream<TcpStream>),
}

impl AsyncRead for ClientStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ClientStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            ClientStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ClientStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            ClientStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            ClientStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ClientStream::Plain(s) => Pin::new(s).poll_flush(cx),
            ClientStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ClientStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            ClientStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Backend stream connecting pgvisor-proxy to a PostgreSQL instance.
pub enum BackendStream {
    Plain(TcpStream),
    Tls(ClientTlsStream<TcpStream>),
}

impl AsyncRead for BackendStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            BackendStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for BackendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            BackendStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_flush(cx),
            BackendStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            BackendStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Permissive server certificate verifier for self-signed backend certificates (sslmode=require semantics).
#[derive(Debug)]
struct AcceptAnyServerCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

/// Builds a `TlsConnector` that accepts self-signed backend Postgres certificates.
pub fn build_backend_connector() -> Result<TlsConnector, ProxyTlsError> {
    let client_config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCertVerifier))
    .with_no_client_auth();

    Ok(TlsConnector::from(Arc::new(client_config)))
}

/// Builds a `TlsAcceptor` for the proxy client listener from PEM certificate and key files.
pub fn build_client_acceptor(
    cert_path: impl AsRef<Path>,
    key_path: impl AsRef<Path>,
) -> Result<TlsAcceptor, ProxyTlsError> {
    let cert_p = cert_path.as_ref();
    let key_p = key_path.as_ref();

    let cert_data = std::fs::read(cert_p)?;
    let mut cert_reader = io::BufReader::new(cert_data.as_slice());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()?;

    let key_data = std::fs::read(key_p)?;
    let mut key_reader = io::BufReader::new(key_data.as_slice());
    let key_der: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| ProxyTlsError::NoPrivateKey(key_p.display().to_string()))?;

    let server_config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key_der)?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Loads an existing certificate or generates a self-signed certificate in the specified directory.
pub fn load_or_generate_proxy_certs(
    cert_dir: impl AsRef<Path>,
    custom_cert_file: Option<PathBuf>,
    custom_key_file: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf), ProxyTlsError> {
    let cert_path = custom_cert_file
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| cert_dir.as_ref().join("proxy.crt"));
    let key_path = custom_key_file
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| cert_dir.as_ref().join("proxy.key"));

    TlsCertPair::load_or_generate(
        &cert_path,
        &key_path,
        "pgvisor-proxy",
        &["localhost".into(), "127.0.0.1".into()],
    )?;

    info!(
        cert = ?cert_path,
        key = ?key_path,
        "pgvisor-proxy TLS certificate ready"
    );

    Ok((cert_path, key_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_build_backend_connector() {
        let connector = build_backend_connector();
        assert!(connector.is_ok());
    }

    #[test]
    fn test_build_client_acceptor() {
        let dir = tempdir().unwrap();
        let (cert_p, key_p) = load_or_generate_proxy_certs(dir.path(), None, None).unwrap();
        let acceptor = build_client_acceptor(&cert_p, &key_p);
        assert!(acceptor.is_ok());
    }
}
