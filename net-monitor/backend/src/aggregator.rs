use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::aggregate::compute_aggregate;
use crate::alert::check_alert;
use crate::filter::apply_filters;
use crate::history::SessionHistory;
use crate::output::{ConnectionSnapshot, emit_snapshot};
use crate::parser::{Protocol, RawPacket};
use crate::proc_attr;
use crate::process_control::{term_process, AutoKillEvent, ProcessLimits};

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

#[derive(Default)]
struct IcmpTotals {
    v4_in: u64,
    v4_out: u64,
    v6_in: u64,
    v6_out: u64,
}

impl IcmpTotals {
    fn any(&self) -> bool {
        self.v4_in > 0 || self.v4_out > 0 || self.v6_in > 0 || self.v6_out > 0
    }
}

fn append_icmp_synthetic(snapshots: &mut Vec<ConnectionSnapshot>, t: &IcmpTotals, ts: i64) {
    // Sentinel endpoints: whole-second ICMP totals (not per-flow).
    if t.v4_in > 0 || t.v4_out > 0 {
        snapshots.push(ConnectionSnapshot {
            pid: None,
            process_name: None,
            username: None,
            src_ip: "0.0.0.0".to_string(),
            src_port: 0,
            dst_ip: "0.0.0.0".to_string(),
            dst_port: 0,
            protocol: "ICMPv4".to_string(),
            bytes_in_per_sec: t.v4_in,
            bytes_out_per_sec: t.v4_out,
            timestamp_unix: ts,
        });
    }
    if t.v6_in > 0 || t.v6_out > 0 {
        snapshots.push(ConnectionSnapshot {
            pid: None,
            process_name: None,
            username: None,
            src_ip: "::".to_string(),
            src_port: 0,
            dst_ip: "::".to_string(),
            dst_port: 0,
            protocol: "ICMPv6".to_string(),
            bytes_in_per_sec: t.v6_in,
            bytes_out_per_sec: t.v6_out,
            timestamp_unix: ts,
        });
    }
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
    process_limits: ProcessLimits,
) -> Result<(), String> {
    let local_ips = detect_local_ips(iface);
    let mut table: HashMap<ConnectionKey, ConnectionStats> = HashMap::new();
    let mut icmp_totals = IcmpTotals::default();
    let mut next_flush = Instant::now() + Duration::from_secs(1);

    while running.load(Ordering::Relaxed) {
        let now = Instant::now();
        let timeout = next_flush.saturating_duration_since(now);

        match rx.recv_timeout(timeout) {
            Ok(packet) => match packet.protocol {
                Protocol::Icmpv4 => {
                    if local_ips.contains(&packet.dst_ip) {
                        icmp_totals.v4_in = icmp_totals.v4_in.saturating_add(packet.length);
                    } else {
                        icmp_totals.v4_out = icmp_totals.v4_out.saturating_add(packet.length);
                    }
                }
                Protocol::Icmpv6 => {
                    if local_ips.contains(&packet.dst_ip) {
                        icmp_totals.v6_in = icmp_totals.v6_in.saturating_add(packet.length);
                    } else {
                        icmp_totals.v6_out = icmp_totals.v6_out.saturating_add(packet.length);
                    }
                }
                Protocol::Tcp | Protocol::Udp | Protocol::Sctp => {
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
            },
            Err(RecvTimeoutError::Timeout) => {
                flush_snapshot(
                    &mut table,
                    &mut icmp_totals,
                    &ipc_tx,
                    filter_ip.as_deref(),
                    filter_port,
                    alert_threshold,
                    &history,
                    &process_limits,
                );
                next_flush = Instant::now() + Duration::from_secs(1);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if !table.is_empty() || icmp_totals.any() {
        flush_snapshot(
            &mut table,
            &mut icmp_totals,
            &ipc_tx,
            filter_ip.as_deref(),
            filter_port,
            alert_threshold,
            &history,
            &process_limits,
        );
    }

    Ok(())
}

fn flush_snapshot(
    table: &mut HashMap<ConnectionKey, ConnectionStats>,
    icmp_totals: &mut IcmpTotals,
    ipc_tx: &Sender<String>,
    filter_ip: Option<&str>,
    filter_port: Option<u16>,
    alert_threshold: Option<u64>,
    history: &Arc<Mutex<SessionHistory>>,
    process_limits: &ProcessLimits,
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

        let protocol = match key.protocol.as_str() {
            "TCP" => Protocol::Tcp,
            "UDP" => Protocol::Udp,
            "SCTP" => Protocol::Sctp,
            _ => continue,
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

    append_icmp_synthetic(&mut snapshots, icmp_totals, timestamp);
    *icmp_totals = IcmpTotals::default();

    let filtered = apply_filters(snapshots, filter_ip, filter_port);
    if let Ok(mut history_guard) = history.lock() {
        history_guard.push(&filtered);
        let _ = history_guard.len();
    }

    let json_line = emit_snapshot(filtered.clone());
    let _ = ipc_tx.send(json_line);

    let agg = compute_aggregate(&filtered, timestamp);
    if let Ok(agg_json) = serde_json::to_string(&agg) {
        let _ = ipc_tx.send(format!("AGGREGATE {}", agg_json));
    }

    if let Some(threshold) = alert_threshold
        && let Some(alert_event) = check_alert(&filtered, threshold)
        && let Ok(alert_json) = serde_json::to_string(&alert_event)
    {
        let _ = ipc_tx.send(format!("ALERT {alert_json}"));
    }

    maybe_auto_kill_over_limits(&filtered, process_limits, ipc_tx, timestamp);
}

/// Update per-PID cumulative totals, then if rate or total cap is exceeded, SIGTERM and `AUTO_KILL`.
fn maybe_auto_kill_over_limits(
    filtered: &[ConnectionSnapshot],
    process_limits: &ProcessLimits,
    ipc_tx: &Sender<String>,
    timestamp: i64,
) {
    let mut by_pid: HashMap<u32, u64> = HashMap::new();
    for s in filtered {
        if let Some(pid) = s.pid {
            let add = s.bytes_in_per_sec.saturating_add(s.bytes_out_per_sec);
            let e = by_pid.entry(pid).or_insert(0);
            *e = e.saturating_add(add);
        }
    }

    let victims: Vec<AutoKillEvent> = {
        let mut guard = match process_limits.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_empty() {
            return;
        }

        for (&pid, entry) in guard.iter_mut() {
            if entry.max_total_bytes.is_some() {
                let inc = by_pid.get(&pid).copied().unwrap_or(0);
                entry.accumulated_total_bytes = entry.accumulated_total_bytes.saturating_add(inc);
            }
        }

        let mut out: Vec<AutoKillEvent> = Vec::new();
        for (&pid, entry) in guard.iter() {
            let rate = by_pid.get(&pid).copied().unwrap_or(0);
            let process_name = filtered
                .iter()
                .find(|x| x.pid == Some(pid))
                .and_then(|x| x.process_name.clone());

            if let Some(thr) = entry.max_rate_bps
                && rate > thr
            {
                out.push(AutoKillEvent {
                    pid,
                    process_name,
                    reason: "rate".to_string(),
                    threshold_bps: Some(thr),
                    observed_bps: Some(rate),
                    threshold_total_bytes: None,
                    observed_total_bytes: None,
                    timestamp_unix: timestamp,
                });
            } else if let Some(thr) = entry.max_total_bytes
                && entry.accumulated_total_bytes >= thr
            {
                out.push(AutoKillEvent {
                    pid,
                    process_name,
                    reason: "total".to_string(),
                    threshold_bps: None,
                    observed_bps: None,
                    threshold_total_bytes: Some(thr),
                    observed_total_bytes: Some(entry.accumulated_total_bytes),
                    timestamp_unix: timestamp,
                });
            }
        }
        out
    };

    for ev in victims {
        let pid = ev.pid;
        if let Err(e) = term_process(pid) {
            eprintln!("net-monitor: auto-kill failed for pid={pid}: {e}");
            continue;
        }
        if let Ok(mut g) = process_limits.lock() {
            g.remove(&pid);
        }
        if let Ok(json) = serde_json::to_string(&ev) {
            match ev.reason.as_str() {
                "rate" => eprintln!(
                    "net-monitor: auto-killed pid={pid} (rate {} B/s > limit {:?} B/s)",
                    ev.observed_bps.unwrap_or(0),
                    ev.threshold_bps
                ),
                "total" => eprintln!(
                    "net-monitor: auto-killed pid={pid} (total {} bytes >= cap {:?} bytes)",
                    ev.observed_total_bytes.unwrap_or(0),
                    ev.threshold_total_bytes
                ),
                _ => eprintln!("net-monitor: auto-killed pid={pid} ({})", ev.reason),
            }
            let _ = ipc_tx.send(format!("AUTO_KILL {json}"));
        }
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
