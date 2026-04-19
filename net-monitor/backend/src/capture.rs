use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pcap::{Capture, Device};

use crate::parser::{RawPacket, parse_packet};

pub fn open_capture(iface: &str) -> Result<Capture<pcap::Active>, pcap::Error> {
    let device = Device::from(iface);
    Capture::from_device(device)?
        .promisc(false)
        .immediate_mode(true)
        .open()
}

pub fn run_capture_loop(
    mut capture: Capture<pcap::Active>,
    tx: Sender<RawPacket>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        match capture.next_packet() {
            Ok(packet) => {
                if let Some(parsed) = parse_packet(packet.data)
                    && tx.send(parsed).is_err()
                {
                    break;
                }
            }
            Err(pcap::Error::TimeoutExpired) => continue,
            Err(_) => break,
        }
    }
}
