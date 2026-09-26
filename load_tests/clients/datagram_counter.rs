// Counts every UDP datagram a synthetic client's transports send: RTP, RTCP,
// STUN and DTLS alike. Clients send only to the server's media workers, so the
// sum over clients is what the workers' sockets received, the denominator of
// the kernel's drops there. webrtc-rs's transport statistics never count sent
// packets, so the count is taken at the socket the runtime wraps.

use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta, Runtime,
    Transmit,
};

/// Datagrams in a transmit that sent `accepted` bytes: one, or one per UDP GSO
/// segment when the transmit asked for segmentation.
fn datagrams_in(accepted: usize, segment_size: Option<usize>) -> u64 {
    match segment_size {
        Some(segment) if segment > 0 => accepted.div_ceil(segment).max(1) as u64,
        _ => 1,
    }
}

/// A runtime that behaves exactly as `inner` and counts what its UDP sockets send.
pub struct CountingRuntime {
    inner: Arc<dyn Runtime>,
    datagrams: Arc<AtomicU64>,
}

impl CountingRuntime {
    pub fn new(inner: Arc<dyn Runtime>, datagrams: Arc<AtomicU64>) -> Self {
        Self { inner, datagrams }
    }
}

impl fmt::Debug for CountingRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CountingRuntime")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Runtime for CountingRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        reactor_pool_size: usize,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(CountingSocket {
            inner: self.inner.wrap_udp_socket(socket)?,
            datagrams: Arc::clone(&self.datagrams),
        }))
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        remote_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.inner.connect_tcp(remote_addr)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.inner.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future);
    }

    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.yield_now()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

/// A socket that counts each datagram it sends. `send_to` keeps the trait's
/// default, which sends through `poll_send`, so every send is counted once.
#[derive(Debug)]
struct CountingSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    datagrams: Arc<AtomicU64>,
}

impl AsyncUdpSocket for CountingSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        let sent = self.inner.poll_send(cx, transmit);
        if let Poll::Ready(Ok(accepted)) = &sent {
            self.datagrams.fetch_add(
                datagrams_in(*accepted, transmit.segment_size),
                Ordering::Relaxed,
            );
        }
        sent
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn max_gso_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_transmit_is_one_datagram_or_one_per_gso_segment() {
        assert_eq!(datagrams_in(1_200, None), 1);
        assert_eq!(datagrams_in(250, Some(100)), 3);
        assert_eq!(datagrams_in(200, Some(100)), 2);
        // A zero segment size cannot split anything.
        assert_eq!(datagrams_in(50, Some(0)), 1);
    }

    #[tokio::test]
    async fn sends_through_a_wrapped_socket_are_counted() {
        let inner = webrtc::runtime::default_runtime().expect("a runtime feature is enabled");
        let counted = Arc::new(AtomicU64::new(0));
        let runtime = CountingRuntime::new(Arc::clone(&inner), Arc::clone(&counted));
        assert_eq!(runtime.name(), inner.name());
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = receiver.local_addr().unwrap();
        let sender = runtime
            .wrap_udp_socket(std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .unwrap();
        for _ in 0..3 {
            assert_eq!(sender.send_to(b"datagram", target).await.unwrap(), 8);
        }
        assert_eq!(counted.load(Ordering::Relaxed), 3);
        let mut buffer = [0_u8; 16];
        assert_eq!(receiver.recv(&mut buffer).unwrap(), 8);
    }
}
