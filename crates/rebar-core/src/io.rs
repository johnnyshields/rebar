use std::future::Future;
use std::io;
use std::mem;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use compio_buf::IntoInner;
use compio_driver::op;
use compio_driver::{Key, PushEntry, SharedFd};
use socket2::{Domain, Protocol, SockRef, Socket, Type};

use crate::executor;

/// Result type for buffer-ownership I/O operations.
pub use compio_buf::BufResult;

// ---------------------------------------------------------------------------
// OpFuture — submit a compio op to the proactor and await completion
// ---------------------------------------------------------------------------

/// Future that submits an I/O operation to the thread-local proactor and
/// resolves when the kernel signals completion.
struct OpFuture<T: compio_driver::OpCode + 'static> {
    state: OpState<T>,
}

enum OpState<T: compio_driver::OpCode + 'static> {
    /// Operation not yet submitted.
    Init(T),
    /// Submitted; waiting for completion.
    Pending(Key<T>),
    /// Already completed or taken.
    Done,
}

// SAFETY: OpFuture holds either an unsubmitted op (movable) or a Key
// (movable handle). No self-referential data.
impl<T: compio_driver::OpCode + 'static> Unpin for OpFuture<T> {}

impl<T: compio_driver::OpCode + 'static> Future for OpFuture<T> {
    type Output = BufResult<usize, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match mem::replace(&mut this.state, OpState::Done) {
            OpState::Init(op) => {
                let entry =
                    executor::with_executor(|ex| ex.proactor().borrow_mut().push(op));
                match entry {
                    PushEntry::Ready(buf_result) => Poll::Ready(buf_result),
                    PushEntry::Pending(mut key) => {
                        executor::with_executor(|ex| {
                            ex.proactor().borrow_mut().update_waker(&mut key, cx.waker());
                        });
                        this.state = OpState::Pending(key);
                        Poll::Pending
                    }
                }
            }
            OpState::Pending(key) => {
                let entry =
                    executor::with_executor(|ex| ex.proactor().borrow_mut().pop(key));
                match entry {
                    PushEntry::Ready(buf_result) => Poll::Ready(buf_result),
                    PushEntry::Pending(mut key) => {
                        executor::with_executor(|ex| {
                            ex.proactor().borrow_mut().update_waker(&mut key, cx.waker());
                        });
                        this.state = OpState::Pending(key);
                        Poll::Pending
                    }
                }
            }
            OpState::Done => panic!("OpFuture polled after completion"),
        }
    }
}

/// Submit an operation to the thread-local proactor and return a future that
/// resolves with the `BufResult`.
fn submit<T: compio_driver::OpCode + 'static>(op: T) -> OpFuture<T> {
    OpFuture {
        state: OpState::Init(op),
    }
}

// ---------------------------------------------------------------------------
// TcpListener
// ---------------------------------------------------------------------------

/// A non-blocking TCP listener bound to a local address.
///
/// Uses `socket2` for socket creation and the thread-local compio `Proactor`
/// for async accept operations.
pub struct TcpListener {
    fd: SharedFd<OwnedFd>,
}

impl TcpListener {
    /// Bind a TCP listener to the given address.
    ///
    /// Enables `SO_REUSEADDR` (and `SO_REUSEPORT` on Unix) and sets the
    /// socket to non-blocking mode before binding.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&addr.into())?;
        socket.listen(1024)?;

        let fd = OwnedFd::from(socket);

        // Attach to proactor (required for IOCP on Windows, no-op on Unix).
        executor::with_executor(|ex| ex.proactor().borrow_mut().attach(fd.as_raw_fd()))?;

        Ok(Self {
            fd: SharedFd::new(fd),
        })
    }

    /// Create a TcpListener from a pre-bound `socket2::Socket`.
    ///
    /// The socket must already be bound and listening. This is useful when
    /// the caller needs custom socket options (e.g., different backlog).
    pub fn from_socket(socket: Socket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let fd = OwnedFd::from(socket);
        executor::with_executor(|ex| ex.proactor().borrow_mut().attach(fd.as_raw_fd()))?;
        Ok(Self {
            fd: SharedFd::new(fd),
        })
    }

    /// Accept a new TCP connection.
    ///
    /// Returns the connected `TcpStream` and the peer's `SocketAddr`.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let accept_op = op::Accept::new(self.fd.clone());
        let BufResult(result, completed_op) = submit(accept_op).await;
        let accepted_raw = result? as i32;

        // Prevent the Accept op's destructor from closing the accepted fd
        // on the poll backend (where accepted_fd is Some). On io_uring it is
        // None so this is harmless.
        mem::forget(completed_op);

        // SAFETY: `accepted_raw` is a valid fd just returned by the kernel.
        let fd = unsafe { OwnedFd::from_raw_fd(accepted_raw) };

        // Attach the new fd to the proactor.
        executor::with_executor(|ex| ex.proactor().borrow_mut().attach(fd.as_raw_fd()))?;

        // Retrieve the peer address via getpeername(2).
        let peer_addr = SockRef::from(&fd)
            .peer_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid peer address"))?;

        Ok((TcpStream { fd: SharedFd::new(fd) }, peer_addr))
    }

    /// Returns the local address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SockRef::from(&*self.fd)
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid local address"))
    }
}

// ---------------------------------------------------------------------------
// TcpStream
// ---------------------------------------------------------------------------

/// A non-blocking TCP stream connected to a remote peer.
///
/// All I/O is performed through the thread-local compio `Proactor`.
pub struct TcpStream {
    fd: SharedFd<OwnedFd>,
}

impl TcpStream {
    /// Read into a buffer, returning `(bytes_read, buffer)`.
    ///
    /// Ownership of `buf` is passed to the kernel for the duration of the
    /// operation and returned in the `BufResult`.
    pub async fn read(&self, buf: Vec<u8>) -> BufResult<usize, Vec<u8>> {
        let recv_op = op::Recv::new(self.fd.clone(), buf, 0);
        let BufResult(result, completed_op) = submit(recv_op).await;
        let mut buf = completed_op.into_inner();
        // compio-driver doesn't auto-update the Vec's length after a read —
        // we must set it to the number of bytes actually read.
        if let Ok(n) = &result {
            unsafe { buf.set_len(*n); }
        }
        BufResult(result, buf)
    }

    /// Write from a buffer, returning `(bytes_written, buffer)`.
    ///
    /// Ownership of `buf` is passed to the kernel for the duration of the
    /// operation and returned in the `BufResult`.
    pub async fn write(&self, buf: Vec<u8>) -> BufResult<usize, Vec<u8>> {
        let send_op = op::Send::new(self.fd.clone(), buf, 0);
        let BufResult(result, completed_op) = submit(send_op).await;
        BufResult(result, completed_op.into_inner())
    }

    /// Write all bytes from a buffer, looping until everything is written.
    ///
    /// Returns the buffer after all bytes have been sent.
    pub async fn write_all(&self, buf: Vec<u8>) -> BufResult<(), Vec<u8>> {
        let mut written = 0;
        let total = buf.len();
        let buf = buf;
        while written < total {
            let slice = buf[written..].to_vec();
            let BufResult(result, _) = self.write(slice).await;
            match result {
                Ok(0) => {
                    return BufResult(
                        Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                        buf,
                    );
                }
                Ok(n) => written += n,
                Err(e) => return BufResult(Err(e), buf),
            }
        }
        BufResult(Ok(()), buf)
    }

    /// Convenience: read up to `len` bytes, returning them as a `Vec<u8>`.
    ///
    /// Uses a freshly allocated buffer. For zero-copy reads via turbine
    /// `LeasedBuffer`, a future `read_lease` method will be added.
    pub async fn read_exact_alloc(&self, len: usize) -> io::Result<Vec<u8>> {
        let buf = Vec::with_capacity(len);
        let BufResult(result, buf) = self.read(buf).await;
        result?;
        Ok(buf)
    }

    /// Returns the peer address of this stream.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        SockRef::from(&*self.fd)
            .peer_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid peer address"))
    }

    /// Returns the local address of this stream.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SockRef::from(&*self.fd)
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid local address"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorConfig, RebarExecutor};

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    #[test]
    fn tcp_listener_bind_and_local_addr() {
        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();
            assert_eq!(addr.ip(), std::net::Ipv4Addr::LOCALHOST);
            assert_ne!(addr.port(), 0);
        });
    }

    #[test]
    fn tcp_stream_echo() {
        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            // Spawn a task that accepts one connection and echoes back.
            let echo = crate::executor::spawn(async move {
                let (stream, _peer) = listener.accept().await.unwrap();
                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = stream.read(buf).await;
                let n = result.unwrap();
                let data = buf[..n].to_vec();
                let BufResult(result, _) = stream.write(data).await;
                result.unwrap();
            });

            // Connect from the "client" side using a raw socket.
            let client_sock =
                Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
            client_sock.set_nonblocking(true).unwrap();
            // Non-blocking connect may return WouldBlock/InProgress.
            let _ = client_sock.connect(&addr.into());
            let client_fd = OwnedFd::from(client_sock);
            executor::with_executor(|ex| {
                ex.proactor().borrow_mut().attach(client_fd.as_raw_fd())
            })
            .unwrap();
            let client = TcpStream {
                fd: SharedFd::new(client_fd),
            };

            // Give the accept a chance to complete by yielding.
            crate::executor::spawn(async {}).await;

            // Write a message.
            let msg = b"hello".to_vec();
            let BufResult(result, _) = client.write(msg).await;
            result.unwrap();

            // Read the echo back.
            let buf = Vec::with_capacity(64);
            let BufResult(result, buf) = client.read(buf).await;
            let n = result.unwrap();
            assert_eq!(&buf[..n], b"hello");

            echo.await;
        });
    }
}
