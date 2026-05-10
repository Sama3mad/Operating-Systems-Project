use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use serde::Serialize;

/// Per-PID limits: optional rate (B/s in a 1s window) and/or optional cumulative cap (bytes since `limit total set`).
#[derive(Clone, Debug, Default)]
pub struct PidLimitEntry {
    pub max_rate_bps: Option<u64>,
    pub max_total_bytes: Option<u64>,
    pub accumulated_total_bytes: u64,
}

pub type ProcessLimits = Arc<Mutex<HashMap<u32, PidLimitEntry>>>;

pub fn new_process_limits() -> ProcessLimits {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Send SIGTERM to `pid`. Refuses PID 0/1 and the current process.
pub fn term_process(pid: u32) -> Result<(), String> {
    if pid <= 1 {
        return Err("refusing to signal privileged or invalid PID".into());
    }
    let my = std::process::id();
    if pid == my {
        return Err("refusing to terminate the net-monitor backend".into());
    }
    let p = Pid::from_raw(pid as i32);
    signal::kill(p, Signal::SIGTERM).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct AutoKillEvent {
    pub pid: u32,
    pub process_name: Option<String>,
    /// `"rate"` or `"total"`.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_total_bytes: Option<u64>,
    pub timestamp_unix: i64,
}
