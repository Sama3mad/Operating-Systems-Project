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
    pub timestamp_unix: i64,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct AlertEvent {
    pub message: String,
    pub total_bytes_per_sec: u64,
    pub threshold: u64,
    pub timestamp_unix: i64,
}

#[derive(Clone, Debug, Default)]
pub struct KnownPidLimit {
    pub rate_bps: Option<u64>,
    pub total_max_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AutoKillEvent {
    pub pid: u32,
    pub process_name: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub threshold_bps: Option<u64>,
    #[serde(default)]
    pub observed_bps: Option<u64>,
    #[serde(default)]
    pub threshold_total_bytes: Option<u64>,
    #[serde(default)]
    pub observed_total_bytes: Option<u64>,
    pub timestamp_unix: i64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainTab {
    Connections,
    Aggregate,
    SessionTotals,
    Limits,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProcessRollup {
    pub process_label: String,
    pub username: String,
    pub bytes_in_per_sec: u64,
    pub bytes_out_per_sec: u64,
    pub flow_count: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProtocolRollup {
    pub protocol: String,
    pub bytes_in_per_sec: u64,
    pub bytes_out_per_sec: u64,
    pub flow_count: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AggregatePayload {
    pub timestamp_unix: i64,
    pub by_process: Vec<ProcessRollup>,
    pub by_protocol: Vec<ProtocolRollup>,
}

// ─── Messages ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    IfaceMeta(String),
    SocketData(Vec<ConnectionSnapshot>),
    AggregateData(AggregatePayload),
    SetTab(MainTab),
    ResetSessionTotals,
    AlertReceived(AlertEvent),
    DismissAlert,
    SetSort(SortMode),
    ExportCsv,
    ExportResult(Result<String, String>),
    SetFilter(String),
    ToggleMonitor,
    ShowContextMenu(ConnectionSnapshot),
    OpenKillConfirm { pid: u32, label: String },
    CancelKillConfirm,
    ConfirmKillProcess,
    KillRequestDone(Result<(), String>),
    LimitPidInput(String),
    LimitMbpsInput(String),
    LimitTotalMbInput(String),
    SubmitLimit,
    SubmitTotalLimit,
    LimitSetDone(Result<(u32, u64), String>),
    LimitTotalSetDone(Result<(u32, u64), String>),
    LimitClearDone(Result<u32, String>),
    RemoveLimitPid(u32),
    AutoKillReceived(AutoKillEvent),
}