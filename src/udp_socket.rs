use std::{
    io,
    net::SocketAddr,
    os::unix::io::AsRawFd,
    task::{Context, Poll},
};

use futures_util::Future;
use futures_core::ready;
use nix::{
    cmsg_space,
    libc::{in6_pktinfo, in_pktinfo},
    sys::{
        socket::{
            recvmsg, sendmsg, setsockopt,
            sockopt::{Ipv4PacketInfo, Ipv6RecvPacketInfo},
            ControlMessage, InetAddr, MsgFlags, SockAddr,
        },
        uio::IoVec,
    },
};
use tokio::{io::ReadBuf, net::UdpSocket as TUdpSocket, pin};

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

pub struct UdpSocket(TUdpSocket);

impl UdpSocket {
    pub fn from_std(socket: std::net::UdpSocket) -> io::Result<Self> {
        setsockopt(socket.as_raw_fd(), Ipv4PacketInfo, &true).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("IP_PKTINFO set error: {}", e))
        })?;
        //setsockopt(socket.as_raw_fd(), Ipv6RecvPacketInfo, &true).map_err(|e| {
        //    io::Error::new(
        //        io::ErrorKind::Other,
        //        format!("IPV6_RECVPKTINFO set error: {}", e),
        //    )
        //})?;
        let socket = TUdpSocket::from_std(socket)?;
        Ok(Self(socket))
    }

    pub fn poll_recv_from(
        &mut self,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<(SocketAddr, Option<Iface>)>> {
        let fut = self.0.readable();
        pin!(fut);
        ready!(fut.poll(cx))?;

        let mut ancilliary = cmsg_space!(in6_pktinfo);

        let out = recvmsg(
            self.0.as_raw_fd(),
            &[IoVec::from_mut_slice(buf.initialize_unfilled())],
            Some(&mut ancilliary),
            MsgFlags::empty(),
        );

        use nix::errno::Errno::*;
        match out {
            Err(e) if e == EAGAIN => {
                return Poll::Pending;
            }
            Err(e) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("recvmsg error: {}", e),
                )))
            }
            Ok(msg) => {
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

    pub fn poll_send_to(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        target: (SocketAddr, Option<Iface>),
    ) -> Poll<io::Result<usize>> {
        let fut = self.0.writable();
        pin!(fut);
        ready!(fut.poll(cx))?;

        let fd = self.0.as_raw_fd();
        let flags = MsgFlags::MSG_DONTWAIT;
        let buf = &[IoVec::from_slice(buf)];

        let out = if let Some(iface) = target.1 {
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

        use nix::errno::Errno::*;
        match out {
            Err(e) if e == EAGAIN => Poll::Pending,
            Err(e) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Other,
                format!("sendmsg error: {}", e),
            ))),
            Ok(len) => Poll::Ready(Ok(len)),
        }
    }
}
