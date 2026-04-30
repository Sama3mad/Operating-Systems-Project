use std::net::IpAddr;

use etherparse::{InternetSlice, SlicedPacket, TransportSlice};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RawPacket {
    pub src_ip: IpAddr,
    pub dst_ip: IpAddr,
    pub src_port: u16,
    pub dst_port: u16,
    pub protocol: Protocol,
    pub length: u64,
}

pub fn parse_packet(bytes: &[u8]) -> Option<RawPacket> {
    let sliced = SlicedPacket::from_ethernet(bytes)
        .ok()
        .or_else(|| SlicedPacket::from_ip(bytes).ok())?;

    let (src_ip, dst_ip) = match sliced.net? {
        InternetSlice::Ipv4(ipv4) => (
            IpAddr::V4(ipv4.header().source_addr()),
            IpAddr::V4(ipv4.header().destination_addr()),
        ),
        InternetSlice::Ipv6(ipv6) => (
            IpAddr::V6(ipv6.header().source_addr()),
            IpAddr::V6(ipv6.header().destination_addr()),
        ),
    };

    let length = match &sliced.transport {
        Some(TransportSlice::Tcp(tcp)) => tcp.slice().len() as u64,
        Some(TransportSlice::Udp(udp)) => udp.slice().len() as u64,
        _ => 0,
    };

    let (protocol, src_port, dst_port) = match sliced.transport? {
        TransportSlice::Tcp(tcp) => (Protocol::Tcp, tcp.source_port(), tcp.destination_port()),
        TransportSlice::Udp(udp) => (Protocol::Udp, udp.source_port(), udp.destination_port()),
        _ => return None,
    };

    Some(RawPacket {
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        protocol,
        length,
    })
}
