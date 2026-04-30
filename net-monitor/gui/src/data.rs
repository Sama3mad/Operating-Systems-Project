use serde::Deserialize;

pub const SOCKET_PATH: &str = "/tmp/net-monitor.sock";
pub const HISTORY_LEN: usize = 60;

// ─── Data types ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
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
    pub timestamp_unix: u64,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct AlertEvent {
    pub message: String,
    pub total_bytes_per_sec: u64,
    pub threshold: u64,
    pub timestamp_unix: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionKey {
    pub src_ip: String,
    pub src_port: u16,
    pub dst_ip: String,
    pub dst_port: u16,
    pub protocol: String,
}

#[derive(Clone, Debug)]
pub struct TrackedConnection {
    pub snapshot: ConnectionSnapshot,
    pub last_seen: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    OutDesc,
    InDesc,
    PidAsc,
    ProcessAsc,
}

// ─── Messages ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    SocketData(Vec<ConnectionSnapshot>),
    AlertReceived(AlertEvent),
    DismissAlert,
    SetSort(SortMode),
    ExportCsv,
    ExportResult(Result<String, String>),
}