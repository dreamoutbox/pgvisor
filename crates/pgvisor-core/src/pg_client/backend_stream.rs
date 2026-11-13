use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;

/// Backend TCP/TLS stream connecting a PgVisor component to a PostgreSQL instance.
pub enum BackendStream {
    Plain(TcpStream),
    Tls(Box<ClientTlsStream<TcpStream>>),
}

impl AsyncRead for BackendStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            BackendStream::Tls(s) => Pin::new(&mut **s).poll_read(cx, buf),
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
            BackendStream::Tls(s) => Pin::new(&mut **s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_flush(cx),
            BackendStream::Tls(s) => Pin::new(&mut **s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BackendStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            BackendStream::Tls(s) => Pin::new(&mut **s).poll_shutdown(cx),
        }
    }
}
