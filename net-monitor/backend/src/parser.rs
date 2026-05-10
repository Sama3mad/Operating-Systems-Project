use std::net::IpAddr;

use etherparse::icmpv4::{TYPE_ECHO_REPLY, TYPE_ECHO_REQUEST};
use etherparse::icmpv6::{TYPE_ECHO_REPLY as V6_ECHO_REPLY, TYPE_ECHO_REQUEST as V6_ECHO_REQUEST};
use etherparse::{IcmpEchoHeader, IpNumber, NetSlice, SlicedPacket, TransportSlice};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Tcp,
    Udp,
    Sctp,
    Icmpv4,
    Icmpv6,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
            Protocol::Sctp => "SCTP",
            Protocol::Icmpv4 => "ICMPv4",
            Protocol::Icmpv6 => "ICMPv6",
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

fn icmp_v4_ports(icmp: &etherparse::Icmpv4Slice<'_>) -> (u16, u16) {
    let t = icmp.type_u8();
    let c = icmp.code_u8();
    if icmp.slice().len() >= 8
        && ((t == TYPE_ECHO_REQUEST && c == 0) || (t == TYPE_ECHO_REPLY && c == 0))
    {
        let echo = IcmpEchoHeader::from_bytes(icmp.bytes5to8());
        return (echo.id, echo.seq);
    }
    (((t as u16) << 8) | u16::from(c), 0)
}

fn icmp_v6_ports(icmp: &etherparse::Icmpv6Slice<'_>) -> (u16, u16) {
    let t = icmp.type_u8();
    let c = icmp.code_u8();
    if icmp.slice().len() >= 8
        && ((t == V6_ECHO_REQUEST && c == 0) || (t == V6_ECHO_REPLY && c == 0))
    {
        let echo = IcmpEchoHeader::from_bytes(icmp.bytes5to8());
        return (echo.id, echo.seq);
    }
    (((t as u16) << 8) | u16::from(c), 0)
}

/// RFC 4960 common header: source port, dest port, verification tag (12 bytes).
fn sctp_ports_and_len(payload: &[u8]) -> Option<(u16, u16, u64)> {
    if payload.len() < 12 {
        return None;
    }
    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    Some((src_port, dst_port, payload.len() as u64))
}

fn try_parse_sctp_after_ip(net: &NetSlice<'_>) -> Option<RawPacket> {
    let (src_ip, dst_ip, payload) = match net {
        NetSlice::Ipv4(ipv4) => {
            if ipv4.header().is_fragmenting_payload() {
                return None;
            }
            let p = ipv4.payload();
            if p.ip_number != IpNumber::SCTP || p.fragmented {
                return None;
            }
            let src = IpAddr::V4(ipv4.header().source_addr());
            let dst = IpAddr::V4(ipv4.header().destination_addr());
            (src, dst, p.payload)
        }
        NetSlice::Ipv6(ipv6) => {
            let p = ipv6.payload();
            if p.ip_number != IpNumber::SCTP || p.fragmented {
                return None;
            }
            let src = IpAddr::V6(ipv6.header().source_addr());
            let dst = IpAddr::V6(ipv6.header().destination_addr());
            (src, dst, p.payload)
        }
    };
    let (src_port, dst_port, length) = sctp_ports_and_len(payload)?;
    Some(RawPacket {
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        protocol: Protocol::Sctp,
        length,
    })
}

pub fn parse_packet(bytes: &[u8]) -> Option<RawPacket> {
    let sliced = SlicedPacket::from_ethernet(bytes)
        .ok()
        .or_else(|| SlicedPacket::from_ip(bytes).ok())?;

    let (src_ip, dst_ip) = match sliced.net.as_ref()? {
        NetSlice::Ipv4(ipv4) => (
            IpAddr::V4(ipv4.header().source_addr()),
            IpAddr::V4(ipv4.header().destination_addr()),
        ),
        NetSlice::Ipv6(ipv6) => (
            IpAddr::V6(ipv6.header().source_addr()),
            IpAddr::V6(ipv6.header().destination_addr()),
        ),
    };

    if let Some(ts) = sliced.transport.as_ref() {
        let pkt = match ts {
            TransportSlice::Tcp(tcp) => {
                let length = tcp.slice().len() as u64;
                RawPacket {
                    src_ip,
                    dst_ip,
                    src_port: tcp.source_port(),
                    dst_port: tcp.destination_port(),
                    protocol: Protocol::Tcp,
                    length,
                }
            }
            TransportSlice::Udp(udp) => {
                let length = udp.slice().len() as u64;
                RawPacket {
                    src_ip,
                    dst_ip,
                    src_port: udp.source_port(),
                    dst_port: udp.destination_port(),
                    protocol: Protocol::Udp,
                    length,
                }
            }
            TransportSlice::Icmpv4(icmp) => {
                let length = icmp.slice().len() as u64;
                let (src_port, dst_port) = icmp_v4_ports(icmp);
                RawPacket {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    protocol: Protocol::Icmpv4,
                    length,
                }
            }
            TransportSlice::Icmpv6(icmp) => {
                let length = icmp.slice().len() as u64;
                let (src_port, dst_port) = icmp_v6_ports(icmp);
                RawPacket {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    protocol: Protocol::Icmpv6,
                    length,
                }
            }
        };
        return Some(pkt);
    }

    // SCTP: etherparse does not decode SCTP as TransportSlice; parse from IP payload.
    if let Some(net) = sliced.net.as_ref() {
        return try_parse_sctp_after_ip(net);
    }

    None
}
