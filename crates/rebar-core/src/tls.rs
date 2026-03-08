//! TLS support for rebar TCP streams via compio-tls (rustls backend).
//!
//! Gated behind the `tls` feature flag. Provides a convenience wrapper around
//! `compio_tls::TlsAcceptor` for server-side TLS handshakes.

use std::io;
use std::sync::Arc;

pub use compio_tls::TlsStream;
pub use rustls;

use crate::io::TcpStream;

/// A TLS acceptor that wraps incoming TCP connections with rustls encryption.
///
/// Constructed from an `Arc<rustls::ServerConfig>`. The acceptor is `Clone`,
/// `Send`, and `Sync`, so it can be shared across worker threads.
#[derive(Clone)]
pub struct TlsAcceptor {
    inner: compio_tls::TlsAcceptor,
}

impl TlsAcceptor {
    /// Create a new `TlsAcceptor` from a rustls `ServerConfig`.
    pub fn new(config: Arc<rustls::ServerConfig>) -> Self {
        Self {
            inner: compio_tls::TlsAcceptor::from(config),
        }
    }

    /// Perform a TLS handshake on the given TCP stream.
    ///
    /// Returns a `TlsStream<TcpStream>` that implements `AsyncRead` and
    /// `AsyncWrite` (via compio-io), transparently encrypting/decrypting data.
    pub async fn accept(&self, stream: TcpStream) -> io::Result<TlsStream<TcpStream>> {
        self.inner.accept(stream).await
    }
}

impl From<Arc<rustls::ServerConfig>> for TlsAcceptor {
    fn from(config: Arc<rustls::ServerConfig>) -> Self {
        Self::new(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorConfig, RebarExecutor};
    use crate::io::{BufResult, TcpListener};
    use compio_io::{AsyncRead, AsyncWrite};

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    /// Generate matching client config that trusts the self-signed cert.
    fn self_signed_client_config(
        server_cert: &rcgen::CertifiedKey,
    ) -> Arc<rustls::ClientConfig> {
        let mut root_store = rustls::RootCertStore::empty();
        root_store
            .add(rustls::pki_types::CertificateDer::from(
                server_cert.cert.der().to_vec(),
            ))
            .unwrap();

        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        )
    }

    /// Helper: generate cert + server/client configs for tests.
    fn test_tls_configs() -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der =
            rustls::pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der()).unwrap();
        let server_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key_der)
                .unwrap(),
        );
        let client_config = self_signed_client_config(&cert);
        (server_config, client_config)
    }

    #[test]
    fn tls_client_write_server_read() {
        let (server_config, client_config) = test_tls_configs();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();

        let server_handle = std::thread::spawn(move || {
            let ex = test_executor();
            ex.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let acceptor = TlsAcceptor::new(server_config);
                let (stream, _peer) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(stream).await.unwrap();

                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = AsyncRead::read(&mut tls, buf).await;
                let n = result.unwrap();
                result_tx.send(buf[..n].to_vec()).unwrap();
            });
        });

        let addr = addr_rx.recv().unwrap();
        let ex = test_executor();
        ex.block_on(async {
            let stream = TcpStream::connect(addr).await.unwrap();
            let connector = compio_tls::TlsConnector::from(client_config);
            let mut tls = connector.connect("localhost", stream).await.unwrap();

            let msg = b"tls one-way".to_vec();
            let BufResult(result, _) = AsyncWrite::write(&mut tls, msg).await;
            result.unwrap();
            AsyncWrite::flush(&mut tls).await.unwrap();
            AsyncWrite::shutdown(&mut tls).await.unwrap();
        });

        let received = result_rx.recv().unwrap();
        assert_eq!(received, b"tls one-way");
        server_handle.join().unwrap();
    }

    #[test]
    fn tls_handshake_and_echo() {
        let (server_config, client_config) = test_tls_configs();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();

        let server_handle = std::thread::spawn(move || {
            let ex = test_executor();
            ex.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let acceptor = TlsAcceptor::new(server_config);
                let (stream, _peer) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(stream).await.unwrap();

                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = AsyncRead::read(&mut tls, buf).await;
                let n = result.unwrap();
                let data = buf[..n].to_vec();
                let BufResult(result, _) = AsyncWrite::write(&mut tls, data).await;
                result.unwrap();
                AsyncWrite::flush(&mut tls).await.unwrap();
                AsyncWrite::shutdown(&mut tls).await.unwrap();
            });
        });

        let addr = addr_rx.recv().unwrap();
        let ex = test_executor();
        ex.block_on(async {
            let stream = TcpStream::connect(addr).await.unwrap();
            let connector = compio_tls::TlsConnector::from(client_config);
            let mut tls = connector.connect("localhost", stream).await.unwrap();

            let msg = b"tls echo test".to_vec();
            let BufResult(result, _) = AsyncWrite::write(&mut tls, msg).await;
            result.unwrap();
            AsyncWrite::flush(&mut tls).await.unwrap();

            let mut all_data = Vec::new();
            loop {
                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = AsyncRead::read(&mut tls, buf).await;
                match result {
                    Ok(0) => break,
                    Ok(n) => all_data.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }
            assert_eq!(all_data, b"tls echo test");
        });

        server_handle.join().unwrap();
    }

    #[test]
    fn tls_data_integrity() {
        let (server_config, client_config) = test_tls_configs();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();

        let server_handle = std::thread::spawn(move || {
            let ex = test_executor();
            ex.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let acceptor = TlsAcceptor::new(server_config);
                let (stream, _peer) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(stream).await.unwrap();

                // Send 4KB of patterned data.
                let data: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
                let BufResult(result, _) = AsyncWrite::write(&mut tls, data).await;
                result.unwrap();
                AsyncWrite::shutdown(&mut tls).await.unwrap();
            });
        });

        let addr = addr_rx.recv().unwrap();
        let ex = test_executor();
        ex.block_on(async {
            let stream = TcpStream::connect(addr).await.unwrap();
            let connector = compio_tls::TlsConnector::from(client_config);
            let mut tls = connector.connect("localhost", stream).await.unwrap();

            let mut all_data = Vec::new();
            loop {
                let buf = Vec::with_capacity(1024);
                let BufResult(result, buf) = AsyncRead::read(&mut tls, buf).await;
                match result {
                    Ok(0) => break,
                    Ok(n) => all_data.extend_from_slice(&buf[..n]),
                    Err(e) => panic!("read error: {e}"),
                }
            }

            let expected: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
            assert_eq!(all_data, expected);
        });

        server_handle.join().unwrap();
    }
}
