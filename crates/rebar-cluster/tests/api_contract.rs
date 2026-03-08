//! API contract regression tests for rebar-cluster.
//!
//! These tests guard the public API surface that downstream crates (especially
//! the `rebar` facade) depend on.  If any of these tests break, it means the
//! cluster crate's contract has changed in a way that would break consumers.

// ---------------------------------------------------------------------------
// Transport trait contracts
// ---------------------------------------------------------------------------

/// Guards that TransportConnection can be implemented with RPITIT (no async_trait).
#[test]
fn transport_connection_trait_is_implementable() {
    use rebar_cluster::protocol::Frame;
    use rebar_cluster::transport::{TransportConnection, TransportError};

    struct DummyConn;

    impl TransportConnection for DummyConn {
        fn send(&mut self, _frame: &Frame) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
        fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
        fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
    }

    let _ = DummyConn;
}

/// Guards that TransportListener can be implemented with RPITIT (no async_trait).
#[test]
fn transport_listener_trait_is_implementable() {
    use rebar_cluster::transport::{TransportConnection, TransportError, TransportListener};
    use rebar_cluster::protocol::Frame;
    use std::net::SocketAddr;

    struct DummyConn;
    impl TransportConnection for DummyConn {
        fn send(&mut self, _frame: &Frame) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
        fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
        fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
    }

    struct DummyListener;
    impl TransportListener for DummyListener {
        type Connection = DummyConn;
        fn local_addr(&self) -> SocketAddr {
            "127.0.0.1:0".parse().unwrap()
        }
        fn accept(&self) -> impl Future<Output = Result<DummyConn, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
    }

    let _ = DummyListener;
}

// ---------------------------------------------------------------------------
// DistributedRouter — NOT Send+Sync (contains Rc)
// ---------------------------------------------------------------------------

static_assertions::assert_not_impl_any!(rebar_cluster::router::DistributedRouter: Send, Sync);

// ---------------------------------------------------------------------------
// ConnectionManager — generic over TransportConnector
// ---------------------------------------------------------------------------

/// Guards that ConnectionManager<C> is generic: it accepts a concrete connector
/// type (not Box<dyn>).
#[test]
fn connection_manager_is_generic() {
    use rebar_cluster::connection::manager::{ConnectionManager, TransportConnector};
    use rebar_cluster::transport::{TransportConnection, TransportError};
    use rebar_cluster::protocol::Frame;

    struct DummyConn;
    impl TransportConnection for DummyConn {
        fn send(&mut self, _frame: &Frame) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
        fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
        fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
    }

    struct DummyConnector;
    impl TransportConnector for DummyConnector {
        type Connection = DummyConn;
        fn connect(
            &self,
            _addr: std::net::SocketAddr,
        ) -> impl Future<Output = Result<DummyConn, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
    }

    // Concrete type parameter, NOT Box<dyn>.
    let _mgr: ConnectionManager<DummyConnector> = ConnectionManager::new(DummyConnector);
}

// ---------------------------------------------------------------------------
// DistributedRuntime — generic over TransportConnector
// ---------------------------------------------------------------------------

/// Guards that DistributedRuntime<C> takes a type parameter.
#[test]
fn distributed_runtime_is_generic() {
    use rebar::DistributedRuntime;
    use rebar_cluster::connection::manager::{ConnectionManager, TransportConnector};
    use rebar_cluster::transport::{TransportConnection, TransportError};
    use rebar_cluster::protocol::Frame;

    struct DummyConn;
    impl TransportConnection for DummyConn {
        fn send(&mut self, _frame: &Frame) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
        fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
        fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
            async { Ok(()) }
        }
    }

    struct DummyConnector;
    impl TransportConnector for DummyConnector {
        type Connection = DummyConn;
        fn connect(
            &self,
            _addr: std::net::SocketAddr,
        ) -> impl Future<Output = Result<DummyConn, TransportError>> {
            async { Err(TransportError::ConnectionClosed) }
        }
    }

    let mgr = ConnectionManager::new(DummyConnector);
    let _drt: DistributedRuntime<DummyConnector> = DistributedRuntime::new(1, mgr);
}

// ---------------------------------------------------------------------------
// QUIC transport
// ---------------------------------------------------------------------------

/// Guards that QuicTransport and related types exist and can be constructed
/// from a generated certificate.
#[test]
fn quic_transport_types_exist() {
    let (cert, key, _hash) = rebar_cluster::transport::quic::generate_self_signed_cert();
    let _transport = rebar_cluster::transport::quic::QuicTransport::new(cert, key);
}

/// Guards that certificate generation produces non-empty certs with
/// deterministic fingerprints.
#[test]
fn quic_cert_generation_works() {
    let (cert, _key, hash) = rebar_cluster::transport::quic::generate_self_signed_cert();

    assert!(
        !AsRef::<[u8]>::as_ref(&cert).is_empty(),
        "generated certificate must not be empty"
    );

    assert_ne!(hash, [0u8; 32], "certificate hash must not be all zeros");

    let recomputed = rebar_cluster::transport::quic::cert_fingerprint(&cert);
    assert_eq!(
        hash, recomputed,
        "cert_fingerprint must be deterministic for the same certificate"
    );
}

/// Guards the QUIC send/recv roundtrip: listen on 127.0.0.1:0, send a Frame
/// from a client, receive it on the server, and verify all fields.
#[tokio::test]
#[ignore = "QUIC transport is a stub in v5"]
async fn quic_send_recv_roundtrip() {
    use rebar_cluster::protocol::{Frame, MsgType};
    use rebar_cluster::transport::traits::{TransportConnection, TransportListener};
    use rebar_cluster::transport::quic::{QuicTransport, generate_self_signed_cert};

    let (cert, key, hash) = generate_self_signed_cert();
    let transport = QuicTransport::new(cert, key);
    let listener = transport
        .listen("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr();

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        conn.recv().await.unwrap()
    });

    let mut client = transport.connect(addr, hash).await.unwrap();
    let frame = Frame {
        version: 1,
        msg_type: MsgType::Send,
        request_id: 42,
        header: rmpv::Value::Nil,
        payload: rmpv::Value::String("quic-roundtrip".into()),
    };
    client.send(&frame).await.unwrap();

    let received = server.await.unwrap();
    assert_eq!(received.version, 1);
    assert_eq!(received.msg_type, MsgType::Send);
    assert_eq!(received.request_id, 42);
    assert_eq!(
        received.payload,
        rmpv::Value::String("quic-roundtrip".into())
    );
}

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

/// Guards that all 13 MsgType variants exist with correct u8 discriminants.
#[test]
fn msg_type_variants_complete() {
    use rebar_cluster::protocol::MsgType;

    let variants: [(MsgType, u8); 13] = [
        (MsgType::Send, 0x01),
        (MsgType::Monitor, 0x02),
        (MsgType::Demonitor, 0x03),
        (MsgType::Link, 0x04),
        (MsgType::Unlink, 0x05),
        (MsgType::Exit, 0x06),
        (MsgType::ProcessDown, 0x07),
        (MsgType::NameLookup, 0x08),
        (MsgType::NameRegister, 0x09),
        (MsgType::NameUnregister, 0x0A),
        (MsgType::Heartbeat, 0x0B),
        (MsgType::HeartbeatAck, 0x0C),
        (MsgType::NodeInfo, 0x0D),
    ];

    for (variant, expected_value) in &variants {
        assert_eq!(
            *variant as u8, *expected_value,
            "MsgType::{:?} should have discriminant 0x{:02X}",
            variant, expected_value
        );
    }

    assert!(
        MsgType::from_u8(0x00).is_err(),
        "0x00 must not be a valid MsgType"
    );
    assert!(
        MsgType::from_u8(0x0E).is_err(),
        "0x0E must not be a valid MsgType"
    );
}

/// Guards that Frame encode/decode is deterministic: encoding the same frame
/// twice produces identical bytes, and decoding recovers all fields.
#[test]
fn frame_encode_decode_deterministic() {
    use rebar_cluster::protocol::{Frame, MsgType};

    let frame = Frame {
        version: 1,
        msg_type: MsgType::Send,
        request_id: 12345,
        header: rmpv::Value::Map(vec![(
            rmpv::Value::String("key".into()),
            rmpv::Value::Integer(99.into()),
        )]),
        payload: rmpv::Value::String("deterministic".into()),
    };

    let bytes1 = frame.encode();
    let bytes2 = frame.encode();
    assert_eq!(bytes1, bytes2, "encoding the same frame twice must produce identical bytes");

    let decoded = Frame::decode(&bytes1).unwrap();
    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.msg_type, MsgType::Send);
    assert_eq!(decoded.request_id, 12345);
    assert_eq!(
        decoded.payload,
        rmpv::Value::String("deterministic".into())
    );
}
