use chrono::Local;

#[derive(serde::Serialize, Clone, Debug)]
pub struct ConnectionSnapshot {
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub username: Option<String>,
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub protocol: String,
    pub bytes_in_per_sec: u64,
    pub bytes_out_per_sec: u64,
    pub timestamp_unix: i64,
}

pub fn emit_snapshot(snapshot: Vec<ConnectionSnapshot>) -> String {
    let json_line = serde_json::to_string(&snapshot).unwrap_or_else(|_| "[]".to_string());
    println!("{json_line}");

    eprintln!(
        "[{}] {} active connections",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        snapshot.len()
    );

    for conn in &snapshot {
        let proc_label = match (&conn.process_name, conn.pid, &conn.username) {
            (Some(name), Some(pid), Some(user)) => format!("{name} (PID {pid}, user {user})"),
            _ => "unknown (unattributed)".to_string(),
        };

        eprintln!(
            "  {}  {}:{} -> {}:{}  {}  in={} B/s out={} B/s",
            proc_label,
            conn.src_ip,
            conn.src_port,
            conn.dst_ip,
            conn.dst_port,
            conn.protocol,
            conn.bytes_in_per_sec,
            conn.bytes_out_per_sec
        );
    }

    json_line
}
