// To test alerts:
// Run backend with a very low threshold so it triggers on NTP traffic (56 B/s):
//   sudo ... cargo run --bin backend -- --iface eth0 --alert-threshold 50
// In a second terminal, stream from the socket:
//   echo "stream" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
// When NTP traffic appears, you should see a line starting with ALERT.

use crate::output::ConnectionSnapshot;

#[derive(serde::Serialize)]
pub struct AlertEvent {
    pub timestamp_unix: i64,
    pub total_bytes_per_sec: u64,
    pub threshold: u64,
    pub message: String,
}

pub fn check_alert(snapshots: &[ConnectionSnapshot], threshold: u64) -> Option<AlertEvent> {
    let total = snapshots.iter().fold(0_u64, |acc, s| {
        acc.saturating_add(s.bytes_in_per_sec.saturating_add(s.bytes_out_per_sec))
    });

    if total > threshold {
        let timestamp_unix = snapshots.first().map(|s| s.timestamp_unix).unwrap_or_else(|| {
            chrono::Utc::now().timestamp()
        });
        Some(AlertEvent {
            timestamp_unix,
            total_bytes_per_sec: total,
            threshold,
            message: format!(
                "ALERT: traffic {}B/s exceeds threshold {}B/s",
                total, threshold
            ),
        })
    } else {
        None
    }
}
