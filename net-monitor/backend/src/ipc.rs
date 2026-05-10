// To test: run the backend with sudo, then in a second terminal:
//   socat - UNIX-CONNECT:/tmp/net-monitor.sock
// You should see one JSON line printed per second.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::history::SessionHistory;
use crate::process_control::{term_process, PidLimitEntry, ProcessLimits};

const SOCKET_PATH: &str = "/tmp/net-monitor.sock";

pub fn start_ipc_server(
    rx: Receiver<String>,
    history: Arc<Mutex<SessionHistory>>,
    iface: Arc<String>,
    process_limits: ProcessLimits,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let _ = fs::remove_file(SOCKET_PATH);

        let listener = match UnixListener::bind(SOCKET_PATH) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("Warning: failed to start IPC server on {}: {}", SOCKET_PATH, err);
                return;
            }
        };

        if let Err(err) = listener.set_nonblocking(true) {
            eprintln!("Warning: failed to configure IPC listener as non-blocking: {}", err);
            let _ = fs::remove_file(SOCKET_PATH);
            return;
        }

        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        loop {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let clients_for_thread = Arc::clone(&clients);
                        let history_for_thread = Arc::clone(&history);
                        let iface_for_thread = Arc::clone(&iface);
                        let limits_for_thread = Arc::clone(&process_limits);
                        thread::spawn(move || {
                            handle_client(
                                stream,
                                clients_for_thread,
                                history_for_thread,
                                iface_for_thread,
                                limits_for_thread,
                            );
                        });
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }

            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let payload = format!("{line}\n");
                    if let Ok(mut guard) = clients.lock() {
                        guard.retain_mut(|stream| stream.write_all(payload.as_bytes()).is_ok());
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let _ = fs::remove_file(SOCKET_PATH);
    })
}

fn write_line(stream: &mut UnixStream, line: &str) {
    let _ = stream.write_all(line.as_bytes());
    let _ = stream.write_all(b"\n");
}

fn handle_client(
    mut stream: UnixStream,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    history: Arc<Mutex<SessionHistory>>,
    iface: Arc<String>,
    process_limits: ProcessLimits,
) {
    let mut command = String::new();
    if let Ok(reader_stream) = stream.try_clone() {
        let mut reader = BufReader::new(reader_stream);
        let _ = reader.read_line(&mut command);
    }

    let command = command.trim();

    if command.eq_ignore_ascii_case("export json") {
        let response = if let Ok(history_guard) = history.lock() {
            history_guard.to_json()
        } else {
            "[]".to_string()
        };
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    if command.eq_ignore_ascii_case("export csv") {
        let response = if let Ok(history_guard) = history.lock() {
            history_guard.to_csv()
        } else {
            String::new()
        };
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    let parts: Vec<&str> = command.split_whitespace().collect();
    match parts.as_slice() {
        [kill, pid_str] if kill.eq_ignore_ascii_case("kill") => {
            let Ok(pid) = pid_str.parse::<u32>() else {
                write_line(&mut stream, "ERR invalid pid");
                return;
            };
            match term_process(pid) {
                Ok(()) => write_line(&mut stream, "OK"),
                Err(e) => write_line(&mut stream, &format!("ERR {e}")),
            }
            return;
        }
        [limit, set, pid_str, bps_str]
            if limit.eq_ignore_ascii_case("limit") && set.eq_ignore_ascii_case("set") =>
        {
            let Ok(pid) = pid_str.parse::<u32>() else {
                write_line(&mut stream, "ERR invalid pid");
                return;
            };
            let Ok(bps) = bps_str.parse::<u64>() else {
                write_line(&mut stream, "ERR invalid bytes_per_sec");
                return;
            };
            if bps == 0 {
                write_line(&mut stream, "ERR limit must be > 0");
                return;
            }
            match process_limits.lock() {
                Ok(mut g) => {
                    let e = g.entry(pid).or_insert(PidLimitEntry::default());
                    e.max_rate_bps = Some(bps);
                    write_line(&mut stream, "OK");
                }
                Err(_) => write_line(&mut stream, "ERR limits lock poisoned"),
            }
            return;
        }
        [limit, total, set, pid_str, bytes_str]
            if limit.eq_ignore_ascii_case("limit")
                && total.eq_ignore_ascii_case("total")
                && set.eq_ignore_ascii_case("set") =>
        {
            let Ok(pid) = pid_str.parse::<u32>() else {
                write_line(&mut stream, "ERR invalid pid");
                return;
            };
            let Ok(max_bytes) = bytes_str.parse::<u64>() else {
                write_line(&mut stream, "ERR invalid max_bytes");
                return;
            };
            if max_bytes == 0 {
                write_line(&mut stream, "ERR max_bytes must be > 0");
                return;
            }
            match process_limits.lock() {
                Ok(mut g) => {
                    let e = g.entry(pid).or_insert(PidLimitEntry::default());
                    e.max_total_bytes = Some(max_bytes);
                    e.accumulated_total_bytes = 0;
                    write_line(&mut stream, "OK");
                }
                Err(_) => write_line(&mut stream, "ERR limits lock poisoned"),
            }
            return;
        }
        [limit, clear, pid_str]
            if limit.eq_ignore_ascii_case("limit") && clear.eq_ignore_ascii_case("clear") =>
        {
            let Ok(pid) = pid_str.parse::<u32>() else {
                write_line(&mut stream, "ERR invalid pid");
                return;
            };
            match process_limits.lock() {
                Ok(mut g) => {
                    g.remove(&pid);
                    write_line(&mut stream, "OK");
                }
                Err(_) => write_line(&mut stream, "ERR limits lock poisoned"),
            }
            return;
        }
        _ => {}
    }

    if let Ok(mut guard) = clients.lock() {
        // One line for stream clients so UIs can show the real capture interface.
        let meta = format!("IFACE {}\n", iface.as_str());
        let _ = stream.write_all(meta.as_bytes());
        guard.push(stream);
    }
}
