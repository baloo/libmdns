use super::MDNS_PORT;
use nix::sys::socket::{
    setsockopt,
    sockopt::{Ipv4PacketInfo, Ipv6RecvPacketInfo},
};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::unix::prelude::RawFd;

pub enum Inet {}

pub enum Inet6 {}

pub trait AddressFamily {
    type Addr: Into<IpAddr>;

    const ANY_ADDR: Self::Addr;
    const MDNS_GROUP: Self::Addr;

    const DOMAIN: Domain;

    fn join_multicast(socket: &Socket, multiaddr: &Self::Addr) -> io::Result<()>;

    fn udp_socket() -> io::Result<Socket> {
        Socket::new(Self::DOMAIN, Type::DGRAM, Some(Protocol::UDP))
    }

    fn bind() -> io::Result<UdpSocket> {
        let addr: SockAddr = SocketAddr::new(Self::ANY_ADDR.into(), MDNS_PORT).into();
        let socket = Self::udp_socket()?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;

        #[cfg(not(windows))]
        #[cfg(not(target_os = "illumos"))]
        socket.set_reuse_port(true)?;

        socket.bind(&addr)?;
        Self::join_multicast(&socket, &Self::MDNS_GROUP)?;
        Ok(socket.into())
    }

    fn set_pkt_info(fd: RawFd) -> Result<(), io::Error>;
}

impl AddressFamily for Inet {
    type Addr = Ipv4Addr;

    const ANY_ADDR: Self::Addr = Ipv4Addr::UNSPECIFIED;
    const MDNS_GROUP: Self::Addr = Ipv4Addr::new(224, 0, 0, 251);

    const DOMAIN: Domain = Domain::IPV4;

    fn join_multicast(socket: &Socket, multiaddr: &Self::Addr) -> io::Result<()> {
        socket.join_multicast_v4(multiaddr, &Ipv4Addr::UNSPECIFIED)
    }

    fn set_pkt_info(fd: RawFd) -> Result<(), io::Error> {
        setsockopt(fd, Ipv4PacketInfo, &true).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("IP_PKTINFO set error: {}", e))
        })?;
        Ok(())
    }
}

impl AddressFamily for Inet6 {
    type Addr = Ipv6Addr;

    const ANY_ADDR: Self::Addr = Ipv6Addr::UNSPECIFIED;
    const MDNS_GROUP: Self::Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);

    const DOMAIN: Domain = Domain::IPV6;

    fn join_multicast(socket: &Socket, multiaddr: &Self::Addr) -> io::Result<()> {
        socket.join_multicast_v6(multiaddr, 0)
    }

    fn set_pkt_info(fd: RawFd) -> Result<(), io::Error> {
        setsockopt(fd, Ipv6RecvPacketInfo, &true).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("IPV6_RECV_PKTINFO set error: {}", e),
            )
        })?;
        Ok(())
    }
}
