use std::net::SocketAddr;

use crate::protocol::Frame;
use crate::transport::traits::{TransportConnection, TransportError, TransportListener};
use sha2::{Digest, Sha256};

/// SHA-256 fingerprint of a DER-encoded certificate.
pub type CertHash = [u8; 32];

/// Generate a self-signed certificate for node-to-node QUIC transport.
///
/// Returns dummy data since rcgen/quinn are removed. Will be replaced with
/// quiche-based QUIC implementation later.
pub fn generate_self_signed_cert() -> (Vec<u8>, Vec<u8>, CertHash) {
    let mut rng_bytes = [0u8; 64];
    // Use random bytes as dummy cert/key data
    getrandom(&mut rng_bytes);
    let cert = rng_bytes[..32].to_vec();
    let key = rng_bytes[32..].to_vec();
    let hash = cert_fingerprint(&cert);
    (cert, key, hash)
}

/// Simple random bytes using rand crate
fn getrandom(buf: &mut [u8]) {
    use rand::Rng;
    let mut rng = rand::rng();
    rng.fill(buf);
}

/// Compute the SHA-256 fingerprint of certificate bytes.
pub fn cert_fingerprint(cert: &[u8]) -> CertHash {
    let mut hasher = Sha256::new();
    hasher.update(cert);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// QUIC Transport (STUB - returns errors for all operations)
// ---------------------------------------------------------------------------

/// QUIC transport stub. Will be replaced with quiche-based implementation.
pub struct QuicTransport {
    _cert: Vec<u8>,
    _key: Vec<u8>,
}

impl QuicTransport {
    pub fn new(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self {
            _cert: cert,
            _key: key,
        }
    }

    pub async fn listen(&self, _addr: SocketAddr) -> Result<QuicListener, TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }

    pub async fn connect(
        &self,
        _addr: SocketAddr,
        _expected_cert_hash: CertHash,
    ) -> Result<QuicConnection, TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }
}

// ---------------------------------------------------------------------------
// QuicTransportConnector (STUB)
// ---------------------------------------------------------------------------

/// Stub [`crate::connection::manager::TransportConnector`] for QUIC.
pub struct QuicTransportConnector {
    _cert: Vec<u8>,
    _key: Vec<u8>,
    _expected_cert_hash: CertHash,
}

impl QuicTransportConnector {
    pub fn new(cert: Vec<u8>, key: Vec<u8>, expected_cert_hash: CertHash) -> Self {
        Self {
            _cert: cert,
            _key: key,
            _expected_cert_hash: expected_cert_hash,
        }
    }
}

impl crate::connection::manager::TransportConnector for QuicTransportConnector {
    type Connection = QuicConnection;

    async fn connect(
        &self,
        _addr: SocketAddr,
    ) -> Result<QuicConnection, TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }
}

// ---------------------------------------------------------------------------
// QuicListener (STUB)
// ---------------------------------------------------------------------------

pub struct QuicListener {
    _addr: SocketAddr,
}

impl TransportListener for QuicListener {
    type Connection = QuicConnection;

    fn local_addr(&self) -> SocketAddr {
        self._addr
    }

    async fn accept(&self) -> Result<Self::Connection, TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }
}

// ---------------------------------------------------------------------------
// QuicConnection (STUB)
// ---------------------------------------------------------------------------

pub struct QuicConnection;

impl TransportConnection for QuicConnection {
    async fn send(&mut self, _frame: &Frame) -> Result<(), TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }

    async fn recv(&mut self) -> Result<Frame, TransportError> {
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "QUIC transport not yet implemented (stub)",
        )))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert_returns_nonempty() {
        let (cert, key, hash) = generate_self_signed_cert();
        assert!(!cert.is_empty());
        assert!(!key.is_empty());
        assert_ne!(hash, [0u8; 32]);
    }

    #[test]
    fn cert_fingerprint_is_deterministic() {
        let (cert, _, hash1) = generate_self_signed_cert();
        let hash2 = cert_fingerprint(&cert);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn different_certs_have_different_fingerprints() {
        let (_, _, hash1) = generate_self_signed_cert();
        let (_, _, hash2) = generate_self_signed_cert();
        assert_ne!(hash1, hash2);
    }

    #[monoio::test(enable_timer = true)]
    async fn quic_transport_listen_returns_stub_error() {
        let (cert, key, _) = generate_self_signed_cert();
        let transport = QuicTransport::new(cert, key);
        let result = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await;
        assert!(result.is_err());
    }

    #[monoio::test(enable_timer = true)]
    async fn quic_transport_connect_returns_stub_error() {
        let (cert, key, hash) = generate_self_signed_cert();
        let transport = QuicTransport::new(cert, key);
        let result = transport
            .connect("127.0.0.1:4000".parse().unwrap(), hash)
            .await;
        assert!(result.is_err());
    }
}
