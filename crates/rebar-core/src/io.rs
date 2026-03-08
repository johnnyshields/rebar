use std::future::Future;
use std::io;
use std::mem::{self, MaybeUninit};
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use compio_buf::{IoBuf, IoBufMut, IntoInner};
use compio_driver::op;
use compio_driver::{Key, PushEntry, SharedFd};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use turbine_core::buffer::leased::LeasedBuffer;

use crate::executor;

/// Result type for buffer-ownership I/O operations.
pub use compio_buf::BufResult;

// ---------------------------------------------------------------------------
// LeaseIoBuf — compio buffer trait adapter for turbine's LeasedBuffer
// ---------------------------------------------------------------------------

/// Wrapper that implements compio's buffer traits for turbine's `LeasedBuffer`.
///
/// This enables zero-copy reads where the kernel writes directly into
/// epoch-arena memory, avoiding per-read allocator calls.
pub struct LeaseIoBuf {
    lease: LeasedBuffer,
    /// Tracks bytes initialized by the kernel (starts at 0).
    init_len: usize,
}

impl LeaseIoBuf {
    /// Wrap a `LeasedBuffer` for use with compio I/O operations.
    pub fn new(lease: LeasedBuffer) -> Self {
        Self { lease, init_len: 0 }
    }

    /// Consume the wrapper and return the inner `LeasedBuffer`.
    pub fn into_inner(self) -> LeasedBuffer {
        self.lease
    }

    /// Number of bytes initialized by the kernel after a read.
    pub fn bytes_read(&self) -> usize {
        self.init_len
    }
}

impl compio_buf::IoBuf for LeaseIoBuf {
    fn as_init(&self) -> &[u8] {
        &self.lease.as_slice()[..self.init_len]
    }
}

impl compio_buf::SetLen for LeaseIoBuf {
    // SAFETY: The caller (compio after a kernel read) guarantees that the first
    // `len` bytes of the underlying arena allocation have been initialized.
    // We only track this count; no memory is freed or reallocated.
    unsafe fn set_len(&mut self, len: usize) {
        self.init_len = len;
    }
}

impl compio_buf::IoBufMut for LeaseIoBuf {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY: LeasedBuffer owns a contiguous arena allocation of `self.lease.len()`
        // bytes. Casting `*mut u8` to `*mut MaybeUninit<u8>` is sound because
        // MaybeUninit<u8> has the same size/alignment as u8. The slice length
        // covers the full allocation so the kernel can write anywhere in it.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.lease.as_mut_slice().as_mut_ptr() as *mut MaybeUninit<u8>,
                self.lease.len(), // full arena allocation, not init_len
            )
        }
    }
}

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
            .ok_or_else(|| io::Error::other("invalid peer address"))?;

        Ok((TcpStream { fd: SharedFd::new(fd) }, peer_addr))
    }

    /// Returns the local address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SockRef::from(&*self.fd)
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::other("invalid local address"))
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
    /// Connect to a remote address, returning a new `TcpStream`.
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_nonblocking(true)?;

        let fd = OwnedFd::from(socket);
        executor::with_executor(|ex| ex.proactor().borrow_mut().attach(fd.as_raw_fd()))?;

        let shared_fd = SharedFd::new(fd);
        let connect_op = op::Connect::new(shared_fd.clone(), addr.into());
        let BufResult(result, _) = submit(connect_op).await;
        result?;

        Ok(Self { fd: shared_fd })
    }

    /// Read exactly `buf.len()` bytes, returning the filled buffer.
    ///
    /// Ownership of `buf` is transferred for the duration of the operation.
    /// On success the buffer is returned fully filled. On error (including
    /// unexpected EOF) the partially-filled buffer is returned.
    pub async fn read_exact(&self, mut buf: Vec<u8>) -> BufResult<(), Vec<u8>> {
        let total = buf.len();
        let mut filled = 0;
        let mut tmp = vec![0u8; total];
        while filled < total {
            tmp.resize(total - filled, 0);
            let BufResult(result, returned) = self.read(tmp).await;
            match result {
                Ok(0) => {
                    return BufResult(
                        Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "unexpected EOF during read_exact",
                        )),
                        buf,
                    );
                }
                Ok(n) => {
                    buf[filled..filled + n].copy_from_slice(&returned[..n]);
                    filled += n;
                    tmp = returned;
                }
                Err(e) => return BufResult(Err(e), buf),
            }
        }
        BufResult(Ok(()), buf)
    }

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
        let total = buf.len();
        // First write uses the original buffer directly (no copy).
        let BufResult(result, mut remaining) = self.write(buf).await;
        match result {
            Ok(0) => {
                return BufResult(
                    Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                    remaining,
                );
            }
            Ok(n) if n >= total => return BufResult(Ok(()), remaining),
            Ok(n) => {
                // Partial write: drain consumed prefix, reuse the allocation.
                remaining.drain(..n);
            }
            Err(e) => return BufResult(Err(e), remaining),
        }
        // Subsequent writes reuse the same shrinking Vec.
        loop {
            let len = remaining.len();
            let BufResult(result, returned) = self.write(remaining).await;
            remaining = returned;
            match result {
                Ok(0) => {
                    return BufResult(
                        Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                        remaining,
                    );
                }
                Ok(n) if n >= len => return BufResult(Ok(()), remaining),
                Ok(n) => {
                    remaining.drain(..n);
                }
                Err(e) => return BufResult(Err(e), remaining),
            }
        }
    }

    /// Read into a `LeasedBuffer` (zero-copy from epoch arena).
    ///
    /// Returns `(bytes_read, LeasedBuffer)`. The caller can inspect
    /// `lease.as_slice()[..n]` for the data read by the kernel.
    pub async fn read_lease(&self, lease: LeasedBuffer) -> BufResult<usize, LeasedBuffer> {
        let wrapper = LeaseIoBuf::new(lease);
        let recv_op = op::Recv::new(self.fd.clone(), wrapper, 0);
        let BufResult(result, completed_op) = submit(recv_op).await;
        let mut wrapper = completed_op.into_inner();
        if let Ok(n) = &result {
            unsafe { compio_buf::SetLen::set_len(&mut wrapper, *n); }
        }
        BufResult(result, wrapper.into_inner())
    }

    /// Convenience: read up to `len` bytes, returning them as a `Vec<u8>`.
    ///
    /// Uses a freshly allocated buffer. For zero-copy reads, use
    /// [`read_lease`](Self::read_lease) with a turbine `LeasedBuffer`.
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
            .ok_or_else(|| io::Error::other("invalid peer address"))
    }

    /// Returns the local address of this stream.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SockRef::from(&*self.fd)
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::Error::other("invalid local address"))
    }
}

// ---------------------------------------------------------------------------
// compio-io trait implementations for ecosystem interop
// ---------------------------------------------------------------------------

// Implement on `&TcpStream` first — SharedFd is stateless so `&self` is safe.
// This also satisfies `for<'a> &'a TcpStream: AsyncRead + AsyncWrite` which
// compio-tls requires for `TlsAcceptor::accept`.

impl compio_io::AsyncRead for &TcpStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        let recv_op = op::Recv::new(self.fd.clone(), buf, 0);
        let BufResult(result, completed_op) = submit(recv_op).await;
        let mut buf = completed_op.into_inner();
        // The kernel wrote bytes but the driver doesn't update the buffer length.
        if let Ok(n) = &result {
            unsafe { compio_buf::SetLen::set_len(&mut buf, *n); }
        }
        BufResult(result, buf)
    }
}

impl compio_io::AsyncWrite for &TcpStream {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        let send_op = op::Send::new(self.fd.clone(), buf, 0);
        let BufResult(result, completed_op) = submit(send_op).await;
        BufResult(result, completed_op.into_inner())
    }

    async fn flush(&mut self) -> io::Result<()> {
        // TCP doesn't buffer — nothing to flush.
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        // Shutdown the write half. This is a non-blocking syscall, not an I/O
        // operation, so a synchronous call via SockRef is appropriate.
        SockRef::from(&*self.fd).shutdown(std::net::Shutdown::Write)?;
        Ok(())
    }
}

// Delegate owned impls to the `&TcpStream` impls.

impl compio_io::AsyncRead for TcpStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        compio_io::AsyncRead::read(&mut &*self, buf).await
    }
}

impl compio_io::AsyncWrite for TcpStream {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        compio_io::AsyncWrite::write(&mut &*self, buf).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        compio_io::AsyncWrite::flush(&mut &*self).await
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        compio_io::AsyncWrite::shutdown(&mut &*self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorConfig, RebarExecutor};
    use turbine_core::config::PoolConfig;

    fn test_executor() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig::default()).unwrap()
    }

    fn test_executor_with_pool() -> RebarExecutor {
        RebarExecutor::new(ExecutorConfig {
            pool_config: Some(PoolConfig::default()),
            ..Default::default()
        })
        .unwrap()
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

            let client = TcpStream::connect(addr).await.unwrap();

            // Write a message.
            let msg = b"hello".to_vec();
            let BufResult(result, _) = client.write(msg).await;
            result.unwrap();

            // Read the echo back.
            let buf = Vec::with_capacity(64);
            let BufResult(result, buf) = client.read(buf).await;
            let n = result.unwrap();
            assert_eq!(&buf[..n], b"hello");

            echo.await.unwrap();
        });
    }

    #[test]
    fn tcp_stream_echo_with_leased_buffer() {
        let ex = test_executor_with_pool();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            // Server: accept, read via leased buffer, echo back.
            let echo = crate::executor::spawn(async move {
                let (stream, _peer) = listener.accept().await.unwrap();
                let lease = crate::executor::with_buffer_pool(|pool| pool.lease(8192))
                    .expect("pool should be configured")
                    .expect("lease should succeed");
                let BufResult(result, lease) = stream.read_lease(lease).await;
                let n = result.unwrap();
                let data = lease.as_slice()[..n].to_vec();
                drop(lease); // Release lease back to arena
                let BufResult(result, _) = stream.write(data).await;
                result.unwrap();
            });

            // Client: connect, write, read echo.
            let client = TcpStream::connect(addr).await.unwrap();

            let msg = b"hello leased".to_vec();
            let BufResult(result, _) = client.write(msg).await;
            result.unwrap();

            let buf = Vec::with_capacity(64);
            let BufResult(result, buf) = client.read(buf).await;
            let n = result.unwrap();
            assert_eq!(&buf[..n], b"hello leased");

            echo.await.unwrap();
        });
    }

    #[test]
    fn tcp_stream_connect() {
        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            let echo = crate::executor::spawn(async move {
                let (stream, _peer) = listener.accept().await.unwrap();
                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = stream.read(buf).await;
                let n = result.unwrap();
                let data = buf[..n].to_vec();
                let BufResult(result, _) = stream.write(data).await;
                result.unwrap();
            });

            let client = TcpStream::connect(addr).await.unwrap();

            let msg = b"via connect".to_vec();
            let BufResult(result, _) = client.write(msg).await;
            result.unwrap();

            let buf = Vec::with_capacity(64);
            let BufResult(result, buf) = client.read(buf).await;
            let n = result.unwrap();
            assert_eq!(&buf[..n], b"via connect");

            echo.await.unwrap();
        });
    }

    #[test]
    fn tcp_stream_read_exact() {
        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            let server = crate::executor::spawn(async move {
                let (stream, _peer) = listener.accept().await.unwrap();
                let BufResult(result, _) = stream.write_all(b"hello world!!".to_vec()).await;
                result.unwrap();
            });

            let client = TcpStream::connect(addr).await.unwrap();

            // Read exactly 5 bytes
            let buf = vec![0u8; 5];
            let BufResult(result, buf) = client.read_exact(buf).await;
            result.unwrap();
            assert_eq!(&buf, b"hello");

            // Read remaining 8 bytes
            let buf = vec![0u8; 8];
            let BufResult(result, buf) = client.read_exact(buf).await;
            result.unwrap();
            assert_eq!(&buf, b" world!!");

            server.await.unwrap();
        });
    }

    #[test]
    fn async_read_write_trait_echo() {
        use compio_io::{AsyncRead, AsyncWrite};

        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            let echo = crate::executor::spawn(async move {
                let (mut stream, _peer) = listener.accept().await.unwrap();
                let buf = Vec::with_capacity(64);
                let BufResult(result, buf) = AsyncRead::read(&mut stream, buf).await;
                let n = result.unwrap();
                let data = buf[..n].to_vec();
                let BufResult(result, _) = AsyncWrite::write(&mut stream, data).await;
                result.unwrap();
            });

            let mut client = TcpStream::connect(addr).await.unwrap();

            let msg = b"trait echo".to_vec();
            let BufResult(result, _) = AsyncWrite::write(&mut client, msg).await;
            result.unwrap();

            let buf = Vec::with_capacity(64);
            let BufResult(result, buf) = AsyncRead::read(&mut client, buf).await;
            let n = result.unwrap();
            assert_eq!(&buf[..n], b"trait echo");

            echo.await.unwrap();
        });
    }

    #[test]
    fn async_write_flush_returns_ok() {
        use compio_io::AsyncWrite;

        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            let _server = crate::executor::spawn(async move {
                let _ = listener.accept().await;
            });

            let mut client = TcpStream::connect(addr).await.unwrap();
            assert!(AsyncWrite::flush(&mut client).await.is_ok());
        });
    }

    #[test]
    fn async_write_shutdown_causes_peer_eof() {
        use compio_io::{AsyncRead, AsyncWrite};

        let ex = test_executor();
        ex.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = listener.local_addr().unwrap();

            let server = crate::executor::spawn(async move {
                let (mut stream, _peer) = listener.accept().await.unwrap();
                // Read should return 0 (EOF) after client shuts down write half.
                let buf = Vec::with_capacity(64);
                let BufResult(result, _) = AsyncRead::read(&mut stream, buf).await;
                let n = result.unwrap();
                assert_eq!(n, 0, "expected EOF after shutdown");
            });

            let mut client = TcpStream::connect(addr).await.unwrap();
            AsyncWrite::shutdown(&mut client).await.unwrap();

            server.await.unwrap();
        });
    }

    #[test]
    fn lease_io_buf_traits() {
        let ex = test_executor_with_pool();
        ex.block_on(async {
            let lease = crate::executor::with_buffer_pool(|pool| pool.lease(4096))
                .expect("pool configured")
                .expect("lease ok");
            let len = lease.len();
            let mut wrapper = LeaseIoBuf::new(lease);

            // Initially no bytes read
            assert_eq!(wrapper.bytes_read(), 0);
            assert_eq!(compio_buf::IoBuf::as_init(&wrapper).len(), 0);

            // Full capacity available for kernel writes
            assert_eq!(compio_buf::IoBufMut::as_uninit(&mut wrapper).len(), len);

            // Simulate kernel writing 5 bytes
            unsafe { compio_buf::SetLen::set_len(&mut wrapper, 5); }
            assert_eq!(wrapper.bytes_read(), 5);
            assert_eq!(compio_buf::IoBuf::as_init(&wrapper).len(), 5);
        });
    }
}
