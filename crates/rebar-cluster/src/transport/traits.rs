use std::net::SocketAddr;

use crate::protocol::Frame;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("frame error: {0}")]
    Frame(#[from] crate::protocol::FrameError),
}

pub trait TransportConnection {
    fn send(&mut self, frame: &Frame) -> impl Future<Output = Result<(), TransportError>>;
    fn recv(&mut self) -> impl Future<Output = Result<Frame, TransportError>>;
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>>;
}

pub trait TransportListener {
    type Connection: TransportConnection;
    fn local_addr(&self) -> SocketAddr;
    fn accept(&self) -> impl Future<Output = Result<Self::Connection, TransportError>>;
}
