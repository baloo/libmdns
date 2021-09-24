use std::{
    io,
    marker::PhantomData,
    net::{SocketAddr, ToSocketAddrs},
    os::unix::io::AsRawFd,
    task::{Context, Poll},
};

use futures_core::ready;
use nix::{
    cmsg_space,
    errno::Errno,
    libc::{in6_pktinfo, in_pktinfo},
    sys::{
        socket::{recvmsg, sendmsg, ControlMessage, InetAddr, MsgFlags, SockAddr},
        uio::IoVec,
    },
};
use tokio::io::{unix::AsyncFd, ReadBuf};

use crate::address_family::AddressFamily;

#[derive(Clone)]
pub enum Iface {
    V4(in_pktinfo),
    V6(in6_pktinfo),
}

impl Iface {
    fn to_control_message(&self) -> ControlMessage<'_> {
        use ControlMessage::*;
        match self {
            Iface::V4(ref p) => Ipv4PacketInfo(p),
            Iface::V6(ref p) => Ipv6PacketInfo(p),
        }
    }
}

pub struct UdpSocket<AF: AddressFamily> {
    socket: AsyncFd<std::net::UdpSocket>,
    _af: PhantomData<AF>,
}

impl<AF> UdpSocket<AF>
where
    AF: AddressFamily,
{
    #[allow(unused)]
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let sock = std::net::UdpSocket::bind(addr)?;
        Self::from_std(sock)
    }

    #[allow(unused)]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.get_ref().local_addr()
    }

    pub fn from_std(socket: std::net::UdpSocket) -> io::Result<Self> {
        AF::set_pkt_info(socket.as_raw_fd())?;

        let socket = AsyncFd::new(socket)?;

        Ok(Self {
            socket,
            _af: PhantomData,
        })
    }

    pub fn poll_recv_from(
        &self,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<(SocketAddr, Option<Iface>)>> {
        loop {
            let mut guard = ready!(self.socket.poll_read_ready(cx))?;

            let mut ancilliary = cmsg_space!(in6_pktinfo);

            match guard.try_io(|inner| {
                recvmsg(
                    inner.as_raw_fd(),
                    &[IoVec::from_mut_slice(buf.initialize_unfilled())],
                    Some(&mut ancilliary),
                    MsgFlags::empty(),
                )
                .map_err(|e| {
                    if e == Errno::EAGAIN {
                        io::Error::new(io::ErrorKind::WouldBlock, "recvmsg would block")
                    } else {
                        io::Error::new(io::ErrorKind::Other, format!("recvmsg error: {}", e))
                    }
                })
            }) {
                Err(_would_block) => continue,
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Ok(Ok(msg)) => {
                    buf.advance(msg.bytes);
                    let mut output = None;
                    for cmsg in msg.cmsgs() {
                        use nix::sys::socket::ControlMessageOwned::*;
                        match cmsg {
                            Ipv4PacketInfo(ipv4) => {
                                output.replace(Iface::V4(ipv4));
                            }
                            Ipv6PacketInfo(ipv6) => {
                                output.replace(Iface::V6(ipv6));
                            }
                            _ => {}
                        };
                    }
                    let addr = match msg.address {
                        Some(SockAddr::Inet(sock)) => Some(sock.to_std()),
                        _ => None,
                    };
                    let addr = addr.ok_or(io::Error::new(io::ErrorKind::Other, "no address found"));

                    return Poll::Ready(addr.map(|addr| (addr, output)));
                }
            }
        }
    }

    pub fn poll_send_to(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        target: (SocketAddr, Option<Iface>),
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.socket.poll_write_ready(cx))?;

            match guard.try_io(|inner| {
                let fd = inner.as_raw_fd();
                let flags = MsgFlags::MSG_DONTWAIT;
                let buf = &[IoVec::from_slice(buf)];

                let out = if let Some(ref iface) = target.1 {
                    let cmsgs = &[iface.to_control_message()][..];
                    sendmsg(
                        fd,
                        buf,
                        cmsgs,
                        flags,
                        Some(&SockAddr::Inet(InetAddr::from_std(&target.0))),
                    )
                } else {
                    let cmsgs = &[][..];
                    sendmsg(
                        fd,
                        buf,
                        cmsgs,
                        flags,
                        Some(&SockAddr::Inet(InetAddr::from_std(&target.0))),
                    )
                };
                out.map_err(|e| {
                    if e == Errno::EAGAIN {
                        io::Error::new(io::ErrorKind::WouldBlock, "sendmsg would block")
                    } else {
                        io::Error::new(io::ErrorKind::Other, format!("sendmsg error: {}", e))
                    }
                })
            }) {
                Err(_would_block) => continue,
                Ok(out) => return Poll::Ready(out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_family::Inet;
    use futures_util::future::poll_fn;
    use tokio::io::ReadBuf;

    const MSG: &[u8] = b"hello";

    #[tokio::test]
    async fn send_to_recv_from_poll() -> std::io::Result<()> {
        let sender = UdpSocket::<Inet>::bind("127.0.0.1:0")?;
        let receiver = UdpSocket::<Inet>::bind("127.0.0.1:0")?;

        let receiver_addr = receiver.local_addr()?;
        for _ in 0..5 {
            poll_fn(|cx| sender.poll_send_to(cx, MSG, (receiver_addr, None))).await?;

            let mut recv_buf = [0u8; 32];
            let mut read = ReadBuf::new(&mut recv_buf);
            let addr = poll_fn(|cx| receiver.poll_recv_from(cx, &mut read)).await?;

            assert_eq!(read.filled(), MSG);
            assert_eq!(addr.0, sender.local_addr()?);
        }
        Ok(())
    }
}
