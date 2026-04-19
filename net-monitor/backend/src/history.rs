// To test export over socket:
// Terminal 1: run backend with sudo
// Terminal 2 - request JSON export:
//   echo "export json" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
// Terminal 2 - request CSV export:
//   echo "export csv" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
// Terminal 2 - stream live (existing behavior):
//   echo "stream" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

use crate::output::ConnectionSnapshot;

pub const MAX_HISTORY_RECORDS: usize = 18000;

pub struct SessionHistory {
    pub records: Vec<ConnectionSnapshot>,
}

impl SessionHistory {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    pub fn push(&mut self, snapshots: &[ConnectionSnapshot]) {
        self.records.extend_from_slice(snapshots);
        let excess = self.records.len().saturating_sub(MAX_HISTORY_RECORDS);
        if excess > 0 {
            self.records.drain(0..excess);
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.records).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn to_csv(&self) -> String {
        let mut out = String::from(
            "timestamp_unix,pid,process_name,username,src_ip,src_port,dst_ip,dst_port,protocol,bytes_in_per_sec,bytes_out_per_sec\n",
        );

        for row in &self.records {
            let pid = row.pid.map(|v| v.to_string()).unwrap_or_default();
            let process_name = row.process_name.as_deref().unwrap_or("");
            let username = row.username.as_deref().unwrap_or("");
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                row.timestamp_unix,
                pid,
                csv_escape(process_name),
                csv_escape(username),
                csv_escape(&row.src_ip),
                row.src_port,
                csv_escape(&row.dst_ip),
                row.dst_port,
                csv_escape(&row.protocol),
                row.bytes_in_per_sec,
                row.bytes_out_per_sec
            ));
        }

        out
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
