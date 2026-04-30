use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::alert::check_alert;
use crate::filter::apply_filters;
use crate::history::SessionHistory;
use crate::output::{ConnectionSnapshot, emit_snapshot};
use crate::parser::RawPacket;
use crate::proc_attr;

#[derive(Hash, Eq, PartialEq, Clone)]
pub struct ConnectionKey {
    pub src_ip: String,
    pub dst_ip: String,
    pub src_port: u16,
    pub dst_port: u16,
    pub protocol: String,
}

pub struct ConnectionStats {
    pub bytes_in: u64,
    pub bytes_out: u64,
}

pub fn run_aggregator(
    rx: Receiver<RawPacket>,
    ipc_tx: Sender<String>,
    iface: &str,
    filter_ip: Option<String>,
    filter_port: Option<u16>,
    alert_threshold: Option<u64>,
    history: Arc<Mutex<SessionHistory>>,
    running: Arc<AtomicBool>,
) -> Result<(), String> {
    let local_ips = detect_local_ips(iface);
    let mut table: HashMap<ConnectionKey, ConnectionStats> = HashMap::new();
    let mut next_flush = Instant::now() + Duration::from_secs(1);

    while running.load(Ordering::Relaxed) {
        let now = Instant::now();
        let timeout = next_flush.saturating_duration_since(now);

        match rx.recv_timeout(timeout) {
            Ok(packet) => {
                let key = ConnectionKey {
                    src_ip: packet.src_ip.to_string(),
                    dst_ip: packet.dst_ip.to_string(),
                    src_port: packet.src_port,
                    dst_port: packet.dst_port,
                    protocol: packet.protocol.as_str().to_string(),
                };

                let entry = table.entry(key).or_insert(ConnectionStats {
                    bytes_in: 0,
                    bytes_out: 0,
                });

                if local_ips.contains(&packet.dst_ip) {
                    entry.bytes_in = entry.bytes_in.saturating_add(packet.length);
                } else {
                    entry.bytes_out = entry.bytes_out.saturating_add(packet.length);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                flush_snapshot(
                    &mut table,
                    &ipc_tx,
                    filter_ip.as_deref(),
                    filter_port,
                    alert_threshold,
                    &history,
                );
                next_flush = Instant::now() + Duration::from_secs(1);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if !table.is_empty() {
        flush_snapshot(
            &mut table,
            &ipc_tx,
            filter_ip.as_deref(),
            filter_port,
            alert_threshold,
            &history,
        );
    }

    Ok(())
}

fn flush_snapshot(
    table: &mut HashMap<ConnectionKey, ConnectionStats>,
    ipc_tx: &Sender<String>,
    filter_ip: Option<&str>,
    filter_port: Option<u16>,
    alert_threshold: Option<u64>,
    history: &Arc<Mutex<SessionHistory>>,
) {
    let timestamp = chrono::Utc::now().timestamp();
    let drained: Vec<(ConnectionKey, ConnectionStats)> = table.drain().collect();
    let mut snapshots = Vec::with_capacity(drained.len());

    for (key, stats) in drained {
        let src_ip: IpAddr = match key.src_ip.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let dst_ip: IpAddr = match key.dst_ip.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };

        let protocol = if key.protocol == "TCP" {
            crate::parser::Protocol::Tcp
        } else {
            crate::parser::Protocol::Udp
        };

        let proc = proc_attr::attribute(src_ip, key.src_port, dst_ip, key.dst_port, protocol);

        snapshots.push(ConnectionSnapshot {
            pid: proc.as_ref().map(|p| p.pid),
            process_name: proc.as_ref().map(|p| p.name.clone()),
            username: proc.as_ref().map(|p| p.username.clone()),
            src_ip: key.src_ip,
            src_port: key.src_port,
            dst_ip: key.dst_ip,
            dst_port: key.dst_port,
            protocol: key.protocol,
            bytes_in_per_sec: stats.bytes_in,
            bytes_out_per_sec: stats.bytes_out,
            timestamp_unix: timestamp,
        });
    }

    let filtered = apply_filters(snapshots, filter_ip, filter_port);
    if let Ok(mut history_guard) = history.lock() {
        history_guard.push(&filtered);
        let _ = history_guard.len();
    }

    let json_line = emit_snapshot(filtered.clone());
    let _ = ipc_tx.send(json_line);

    if let Some(threshold) = alert_threshold
        && let Some(alert_event) = check_alert(&filtered, threshold)
        && let Ok(alert_json) = serde_json::to_string(&alert_event)
    {
        let _ = ipc_tx.send(format!("ALERT {alert_json}"));
    }
}

fn detect_local_ips(iface: &str) -> HashSet<IpAddr> {
    let mut set = HashSet::new();

    for flag in ["-4", "-6"] {
        let output = Command::new("ip")
            .args([flag, "addr", "show", "dev", iface])
            .output();

        let Ok(output) = output else {
            continue;
        };

        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("inet ") {
                if let Some(ip) = rest.split_whitespace().next()
                    && let Some(addr) = ip.split('/').next()
                    && let Ok(parsed) = addr.parse::<IpAddr>()
                {
                    set.insert(parsed);
                }
            } else if let Some(rest) = trimmed.strip_prefix("inet6 ")
                && let Some(ip) = rest.split_whitespace().next()
                && let Some(addr) = ip.split('/').next()
                && let Ok(parsed) = addr.parse::<IpAddr>()
            {
                set.insert(parsed);
            }
        }
    }

    set
}
