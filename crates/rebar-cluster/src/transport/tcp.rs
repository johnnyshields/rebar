use std::net::SocketAddr;

use monoio::io::{AsyncReadRentExt, AsyncWriteRentExt};
use monoio::net::{TcpListener, TcpStream};

use crate::protocol::Frame;
use crate::transport::traits::{TransportConnection, TransportError, TransportListener};

/// TCP transport using length-prefixed framing.
///
/// Wire format:
/// ```text
/// +----------+--------------+
/// | len: u32 | payload: [u8]|
/// +----------+--------------+
/// ```
pub struct TcpTransport;

impl TcpTransport {
    pub fn new() -> Self {
        TcpTransport
    }

    pub async fn listen(&self, addr: SocketAddr) -> Result<TcpTransportListener, TransportError> {
        let listener = TcpListener::bind(addr)?;
        Ok(TcpTransportListener { inner: listener })
    }

    pub async fn connect(&self, addr: SocketAddr) -> Result<TcpConnection, TransportError> {
        let stream = TcpStream::connect(addr).await?;
        Ok(TcpConnection { stream: Some(stream) })
    }
}

pub struct TcpTransportListener {
    inner: TcpListener,
}

impl TransportListener for TcpTransportListener {
    type Connection = TcpConnection;

    fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr().expect("listener has local addr")
    }

    async fn accept(&self) -> Result<Self::Connection, TransportError> {
        let (stream, _addr) = self.inner.accept().await?;
        Ok(TcpConnection { stream: Some(stream) })
    }
}

pub struct TcpConnection {
    stream: Option<TcpStream>,
}

impl TransportConnection for TcpConnection {
    async fn send(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let stream = self.stream.as_mut().ok_or(TransportError::ConnectionClosed)?;
        let encoded = frame.encode();
        let len = encoded.len() as u32;
        let len_bytes = len.to_be_bytes().to_vec();
        let (result, _) = stream.write_all(len_bytes).await;
        result?;
        let (result, _) = stream.write_all(encoded).await;
        result?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Frame, TransportError> {
        let stream = self.stream.as_mut().ok_or(TransportError::ConnectionClosed)?;
        let len_buf = vec![0u8; 4];
        let (result, len_buf) = stream.read_exact(len_buf).await;
        match result {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(TransportError::ConnectionClosed);
            }
            Err(e) => return Err(TransportError::Io(e)),
        }
        let len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
        let buf = vec![0u8; len];
        let (result, buf) = stream.read_exact(buf).await;
        result?;
        let frame = Frame::decode(&buf)?;
        Ok(frame)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        // Drop the stream to close the connection
        self.stream.take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, MsgType};

    #[monoio::test(enable_timer = true)]
    async fn connect_and_send_frame() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();

        // We can't use monoio::spawn (it returns JoinHandle differently),
        // so we use a simple sequential approach: client sends, then we accept.
        let mut client = transport.connect(addr).await.unwrap();
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Heartbeat,
            request_id: 0,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::Nil,
        };
        client.send(&frame).await.unwrap();
        client.close().await.unwrap();

        let mut conn = listener.accept().await.unwrap();
        let received = conn.recv().await.unwrap();
        assert_eq!(received.msg_type, MsgType::Heartbeat);
    }

    #[monoio::test(enable_timer = true)]
    async fn bidirectional_echo() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();

        let mut client = transport.connect(addr).await.unwrap();
        let frame = Frame {
            version: 1,
            msg_type: MsgType::Send,
            request_id: 42,
            header: rmpv::Value::Nil,
            payload: rmpv::Value::String("ping".into()),
        };
        client.send(&frame).await.unwrap();

        let mut server_conn = listener.accept().await.unwrap();
        let received = server_conn.recv().await.unwrap();
        server_conn.send(&received).await.unwrap();

        let response = client.recv().await.unwrap();
        assert_eq!(response.request_id, 42);
        assert_eq!(response.payload, rmpv::Value::String("ping".into()));
    }

    #[monoio::test(enable_timer = true)]
    async fn multiple_frames_sequential() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();

        let mut client = transport.connect(addr).await.unwrap();
        for i in 0..5u64 {
            client
                .send(&Frame {
                    version: 1,
                    msg_type: MsgType::Send,
                    request_id: i,
                    header: rmpv::Value::Nil,
                    payload: rmpv::Value::Integer(i.into()),
                })
                .await
                .unwrap();
        }

        let mut conn = listener.accept().await.unwrap();
        let mut frames = Vec::new();
        for _ in 0..5 {
            frames.push(conn.recv().await.unwrap());
        }
        assert_eq!(frames.len(), 5);
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.request_id, i as u64);
        }
    }

    #[monoio::test(enable_timer = true)]
    async fn large_frame() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();

        let mut client = transport.connect(addr).await.unwrap();
        let big = "x".repeat(1_000_000);
        client
            .send(&Frame {
                version: 1,
                msg_type: MsgType::Send,
                request_id: 0,
                header: rmpv::Value::Nil,
                payload: rmpv::Value::String(big.into()),
            })
            .await
            .unwrap();

        let mut conn = listener.accept().await.unwrap();
        let received = conn.recv().await.unwrap();
        assert_eq!(received.payload.as_str().unwrap().len(), 1_000_000);
    }

    #[monoio::test(enable_timer = true)]
    async fn recv_after_close_returns_error() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();

        let mut client = transport.connect(addr).await.unwrap();
        client.close().await.unwrap();

        let mut conn = listener.accept().await.unwrap();
        let result = conn.recv().await;
        assert!(result.is_err());
    }

    #[monoio::test(enable_timer = true)]
    async fn connect_to_invalid_address_returns_error() {
        let transport = TcpTransport::new();
        let result = transport.connect("127.0.0.1:1".parse().unwrap()).await;
        assert!(result.is_err());
    }

    #[monoio::test(enable_timer = true)]
    async fn listener_local_addr() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();
        assert_eq!(
            addr.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        assert_ne!(addr.port(), 0);
    }

    #[monoio::test(enable_timer = true)]
    async fn high_throughput() {
        let transport = TcpTransport::new();
        let listener = transport
            .listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr();
        let count = 1000u64;

        let mut client = transport.connect(addr).await.unwrap();
        for i in 0..count {
            client
                .send(&Frame {
                    version: 1,
                    msg_type: MsgType::Heartbeat,
                    request_id: i,
                    header: rmpv::Value::Nil,
                    payload: rmpv::Value::Nil,
                })
                .await
                .unwrap();
        }

        let mut conn = listener.accept().await.unwrap();
        let mut received = 0u64;
        for _ in 0..count {
            conn.recv().await.unwrap();
            received += 1;
        }
        assert_eq!(received, count);
    }
}
