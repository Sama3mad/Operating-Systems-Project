//! Short-lived Unix socket commands (separate from the long-lived `stream` client).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::data::SOCKET_PATH;

fn send_line(cmd: &str) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(SOCKET_PATH).map_err(|e| format!("connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut line_out = cmd.to_string();
    if !line_out.ends_with('\n') {
        line_out.push('\n');
    }
    stream
        .write_all(line_out.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| format!("read: {e}"))?;
    Ok(line)
}

/// `kill <pid>` — backend must run with sufficient privilege (usually same as capture).
pub fn kill_process(pid: u32) -> Result<(), String> {
    let line = send_line(&format!("kill {pid}"))?;
    parse_ok_err_line(&line)
}

/// `limit set <pid> <bytes_per_sec>`
pub fn limit_set(pid: u32, bytes_per_sec: u64) -> Result<(), String> {
    let line = send_line(&format!("limit set {pid} {bytes_per_sec}"))?;
    parse_ok_err_line(&line)
}

/// `limit clear <pid>`
pub fn limit_clear(pid: u32) -> Result<(), String> {
    let line = send_line(&format!("limit clear {pid}"))?;
    parse_ok_err_line(&line)
}

/// `limit total set <pid> <max_bytes>` — cumulative in+out cap since set (resets counter).
pub fn limit_total_set(pid: u32, max_total_bytes: u64) -> Result<(), String> {
    let line = send_line(&format!("limit total set {pid} {max_total_bytes}"))?;
    parse_ok_err_line(&line)
}

fn parse_ok_err_line(line: &str) -> Result<(), String> {
    let t = line.trim();
    if t == "OK" {
        return Ok(());
    }
    if let Some(rest) = t.strip_prefix("ERR ") {
        return Err(rest.to_string());
    }
    if t.starts_with("ERR") {
        return Err(t.to_string());
    }
    Err(format!("unexpected response: {t:?}"))
}
