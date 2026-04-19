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

const SOCKET_PATH: &str = "/tmp/net-monitor.sock";

pub fn start_ipc_server(
    rx: Receiver<String>,
    history: Arc<Mutex<SessionHistory>>,
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
                        thread::spawn(move || {
                            handle_client(stream, clients_for_thread, history_for_thread);
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

fn handle_client(
    mut stream: UnixStream,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    history: Arc<Mutex<SessionHistory>>,
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

    if let Ok(mut guard) = clients.lock() {
        guard.push(stream);
    }
}
