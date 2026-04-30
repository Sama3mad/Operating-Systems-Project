use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::data::SOCKET_PATH;

// ─── CSV export (sync) ────────────────────────────────────────────────────────

pub fn do_export_csv() -> std::io::Result<String> {
    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    stream.write_all(b"export csv\n")?;

    let mut reader = BufReader::new(stream);
    let mut csv_content = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => csv_content.push_str(&line),
            Err(e) => return Err(e),
        }
    }

    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let filename = format!("net-monitor-export-{epoch}.csv");
    let mut file = File::create(&filename)?;
    file.write_all(csv_content.as_bytes())?;
    Ok(filename)
}