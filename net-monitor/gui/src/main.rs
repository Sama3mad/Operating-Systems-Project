use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Stdout, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::symbols::Marker;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Borders, Cell, Chart, Clear, Dataset, Paragraph, Row, Table};
use serde::Deserialize;

const SOCKET_PATH: &str = "/tmp/net-monitor.sock";

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
struct ConnectionSnapshot {
    pid: Option<u32>,
    process_name: Option<String>,
    username: Option<String>,
    src_ip: String,
    src_port: u16,
    dst_ip: String,
    dst_port: u16,
    protocol: String,
    bytes_in_per_sec: u64,
    bytes_out_per_sec: u64,
    timestamp_unix: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct AlertEvent {
    message: String,
    total_bytes_per_sec: u64,
    threshold: u64,
    timestamp_unix: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConnectionKey {
    src_ip: String,
    src_port: u16,
    dst_ip: String,
    dst_port: u16,
    protocol: String,
}

#[derive(Clone, Debug)]
struct TrackedConnection {
    snapshot: ConnectionSnapshot,
    last_seen: Instant,
}

#[derive(Clone, Copy, Debug)]
enum SortMode {
    OutDesc,
    InDesc,
    PidAsc,
    ProcessAsc,
}

struct AppState {
    tracked: HashMap<ConnectionKey, TrackedConnection>,
    scroll: usize,
    last_snapshot_at: Option<Instant>,
    sort_mode: SortMode,
    bandwidth_history: VecDeque<(f64, f64)>,
    bandwidth_history_out: VecDeque<(f64, f64)>,
    session_start: Instant,
    active_alert: Option<AlertEvent>,
    alert_arrived_at: Option<Instant>,
    alert_cleared_at: Option<Instant>,
    export_status: Option<String>,
    export_status_set_at: Option<Instant>,
}

impl AppState {
    fn new() -> Self {
        Self {
            tracked: HashMap::new(),
            scroll: 0,
            last_snapshot_at: None,
            sort_mode: SortMode::OutDesc,
            bandwidth_history: VecDeque::new(),
            bandwidth_history_out: VecDeque::new(),
            session_start: Instant::now(),
            active_alert: None,
            alert_arrived_at: None,
            alert_cleared_at: None,
            export_status: None,
            export_status_set_at: None,
        }
    }

    fn connected(&self) -> bool {
        matches!(self.last_snapshot_at, Some(t) if t.elapsed() < Duration::from_secs(3))
    }

    fn sorted_rows(&self) -> Vec<ConnectionSnapshot> {
        let mut rows: Vec<ConnectionSnapshot> = self
            .tracked
            .values()
            .map(|tracked| tracked.snapshot.clone())
            .collect();
        match self.sort_mode {
            SortMode::OutDesc => rows.sort_by(|a, b| b.bytes_out_per_sec.cmp(&a.bytes_out_per_sec)),
            SortMode::InDesc => rows.sort_by(|a, b| b.bytes_in_per_sec.cmp(&a.bytes_in_per_sec)),
            SortMode::PidAsc => rows.sort_by(|a, b| a.pid.cmp(&b.pid)),
            SortMode::ProcessAsc => rows.sort_by(|a, b| {
                a.process_name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.process_name.as_deref().unwrap_or(""))
            }),
        }
        rows
    }

    fn drain_updates(&mut self, rx: &Receiver<Vec<ConnectionSnapshot>>) {
        let mut saw_new_snapshot = false;
        while let Ok(snapshot_vec) = rx.try_recv() {
            saw_new_snapshot = true;
            let now = Instant::now();

            let total_in: u64 = snapshot_vec.iter().map(|r| r.bytes_in_per_sec).sum();
            let total_out: u64 = snapshot_vec.iter().map(|r| r.bytes_out_per_sec).sum();
            let elapsed = self.session_start.elapsed().as_secs_f64();
            self.bandwidth_history.push_back((elapsed, total_in as f64));
            self.bandwidth_history_out
                .push_back((elapsed, total_out as f64));
            while self.bandwidth_history.len() > 30 {
                let _ = self.bandwidth_history.pop_front();
            }
            while self.bandwidth_history_out.len() > 30 {
                let _ = self.bandwidth_history_out.pop_front();
            }

            for snapshot in snapshot_vec {
                let key = ConnectionKey {
                    src_ip: snapshot.src_ip.clone(),
                    src_port: snapshot.src_port,
                    dst_ip: snapshot.dst_ip.clone(),
                    dst_port: snapshot.dst_port,
                    protocol: snapshot.protocol.clone(),
                };
                self.tracked.insert(
                    key,
                    TrackedConnection {
                        snapshot,
                        last_seen: now,
                    },
                );
            }
        }

        self.tracked
            .retain(|_, tracked| tracked.last_seen.elapsed() < Duration::from_secs(3));

        if saw_new_snapshot {
            self.last_snapshot_at = Some(Instant::now());
            self.scroll = 0;
        }
    }

    fn active_len(&self) -> usize {
        self.tracked.len()
    }

    fn drain_alerts(&mut self, alert_rx: &Receiver<AlertEvent>) {
        let mut latest: Option<AlertEvent> = None;
        while let Ok(alert) = alert_rx.try_recv() {
            latest = Some(alert);
        }
        if let Some(alert) = latest {
            self.active_alert = Some(alert);
            self.alert_arrived_at = Some(Instant::now());
            self.alert_cleared_at = None;
        }
    }

    fn tick_timers(&mut self) {
        if let Some(arrived_at) = self.alert_arrived_at
            && arrived_at.elapsed() > Duration::from_secs(5)
        {
            self.active_alert = None;
            self.alert_arrived_at = None;
            self.alert_cleared_at = Some(Instant::now());
        }

        if let Some(set_at) = self.export_status_set_at
            && set_at.elapsed() > Duration::from_secs(4)
        {
            self.export_status = None;
            self.export_status_set_at = None;
        }
    }
}

struct TerminalCleanup {
    stdout: Stdout,
}

impl TerminalCleanup {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        Ok(Self { stdout })
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.stdout, LeaveAlternateScreen);
    }
}

fn spawn_socket_thread(tx: Sender<Vec<ConnectionSnapshot>>, alert_tx: Sender<AlertEvent>) {
    thread::spawn(move || {
        loop {
            match UnixStream::connect(SOCKET_PATH) {
                Ok(mut stream) => {
                    let _ = stream.write_all(b"stream\n");
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();

                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break,
                            Ok(_) => {
                                let trimmed = line.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }
                                if let Some(alert_json) = trimmed.strip_prefix("ALERT ") {
                                    if let Ok(alert) = serde_json::from_str::<AlertEvent>(alert_json) {
                                        let _ = alert_tx.send(alert);
                                    }
                                    continue;
                                }
                                if let Ok(snapshot) =
                                    serde_json::from_str::<Vec<ConnectionSnapshot>>(trimmed)
                                {
                                    let _ = tx.send(snapshot);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(_) => thread::sleep(Duration::from_secs(2)),
            }
        }
    });
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let _ = app.alert_cleared_at;
    let size = frame.area();
    let constraints = if app.active_alert.is_some() {
        vec![
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(size);

    let sorted_rows = app.sorted_rows();
    let total_in: u64 = sorted_rows.iter().map(|r| r.bytes_in_per_sec).sum();
    let total_out: u64 = sorted_rows.iter().map(|r| r.bytes_out_per_sec).sum();
    let status_text = if app.connected() {
        Span::styled("Connected", Style::default().fg(Color::Green))
    } else {
        Span::styled("Not connected", Style::default().fg(Color::Red))
    };

    let top = Paragraph::new(Line::from(vec![
        Span::raw("Linux Network Monitor | Interface: eth0 | "),
        Span::raw(format!("In: {} B/s | Out: {} B/s | Status: ", total_in, total_out)),
        status_text,
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(top, chunks[0]);

    let mut graph_chunk_idx = 1;
    let mut table_chunk_idx = 2;
    let mut footer_chunk_idx = 3;

    if let Some(alert) = &app.active_alert {
        let _ = (alert.total_bytes_per_sec, alert.threshold, alert.timestamp_unix);
        graph_chunk_idx = 2;
        table_chunk_idx = 3;
        footer_chunk_idx = 4;
        let alert_text = format!(
            "{}\n(auto-clears in 5s, press 'a' to dismiss)",
            alert.message
        );
        let alert_banner = Paragraph::new(alert_text)
            .alignment(Alignment::Center)
            .style(Style::default().bg(Color::Red).fg(Color::White))
            .block(
                Block::default()
                    .title(
                        Span::styled(
                            " ⚠ BANDWIDTH ALERT ",
                            Style::default()
                                .fg(Color::Red)
                                .add_modifier(Modifier::BOLD),
                        ),
                    )
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            );
        frame.render_widget(alert_banner, chunks[1]);
    }

    if let Some(status) = &app.export_status {
        let color = if status.starts_with("Exported") {
            Color::Green
        } else {
            Color::Red
        };
        let status_line = Paragraph::new(status.clone())
            .alignment(Alignment::Left)
            .style(Style::default().fg(color));
        let top_inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[0]);
        frame.render_widget(status_line, top_inner[1]);
    }

    let in_points: Vec<(f64, f64)> = app.bandwidth_history.iter().copied().collect();
    let out_points: Vec<(f64, f64)> = app.bandwidth_history_out.iter().copied().collect();
    let most_recent_elapsed = in_points
        .last()
        .map(|(x, _)| *x)
        .or_else(|| out_points.last().map(|(x, _)| *x))
        .unwrap_or(30.0);
    let x_min = if most_recent_elapsed > 30.0 {
        most_recent_elapsed - 30.0
    } else {
        0.0
    };
    let max_in = in_points.iter().map(|(_, y)| *y).fold(0.0_f64, f64::max);
    let max_out = out_points.iter().map(|(_, y)| *y).fold(0.0_f64, f64::max);
    let max_value = max_in.max(max_out).max(100.0);

    let dataset_in = Dataset::default()
        .name("In")
        .marker(Marker::Braille)
        .style(Style::default().fg(Color::Cyan))
        .data(&in_points);
    let dataset_out = Dataset::default()
        .name("Out")
        .marker(Marker::Braille)
        .style(Style::default().fg(Color::Yellow))
        .data(&out_points);
    let chart = Chart::new(vec![dataset_in, dataset_out])
        .block(
            Block::default()
                .title("Bandwidth (last 30s)")
                .borders(Borders::ALL),
        )
        .x_axis(
            Axis::default()
                .title(Span::styled(
                    "Time (s)",
                    Style::default().fg(Color::Gray),
                ))
                .bounds([x_min, x_min + 30.0])
                .labels([
                    Span::styled(format!("{x_min:.0}"), Style::default().fg(Color::Gray)),
                    Span::styled(format!("{:.0}", x_min + 15.0), Style::default().fg(Color::Gray)),
                    Span::styled(
                        format!("{:.0}", x_min + 30.0),
                        Style::default().fg(Color::Gray),
                    ),
                ]),
        )
        .y_axis(
            Axis::default()
                .title(Span::styled("B/s", Style::default().fg(Color::Gray)))
                .bounds([0.0, max_value * 1.2])
                .labels([
                    Span::styled("0", Style::default().fg(Color::Gray)),
                    Span::styled(format!("{:.0}", (max_value * 1.2) / 2.0), Style::default().fg(Color::Gray)),
                    Span::styled(format!("{:.0}", max_value * 1.2), Style::default().fg(Color::Gray)),
                ]),
        );
    frame.render_widget(chart, chunks[graph_chunk_idx]);

    if app.last_snapshot_at.is_none() {
        let message_area = centered_rect(chunks[table_chunk_idx], 40, 20);
        frame.render_widget(Clear, message_area);
        let waiting = Paragraph::new("Waiting for backend...")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White));
        frame.render_widget(waiting, message_area);
    } else {
        let header = Row::new(vec![
            Cell::from("PID"),
            Cell::from("Process"),
            Cell::from("User"),
            Cell::from("Src IP:Port"),
            Cell::from("Dst IP:Port"),
            Cell::from("Proto"),
            Cell::from("In B/s"),
            Cell::from("Out B/s"),
        ])
        .style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        let body_rows = sorted_rows.iter().enumerate().map(|(idx, r)| {
            let bg = if idx % 2 == 0 {
                Color::Reset
            } else {
                Color::DarkGray
            };
            let proto_color = match r.protocol.as_str() {
                "TCP" => Color::Green,
                "UDP" => Color::Yellow,
                _ => Color::White,
            };

            Row::new(vec![
                Cell::from(r.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".to_string()))
                    .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(
                    r.process_name
                        .as_deref()
                        .unwrap_or("—")
                        .to_string(),
                )
                .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(r.username.as_deref().unwrap_or("—").to_string())
                    .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(format!("{}:{}", r.src_ip, r.src_port))
                    .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(format!("{}:{}", r.dst_ip, r.dst_port))
                    .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(r.protocol.clone()).style(Style::default().fg(proto_color).bg(bg)),
                Cell::from(r.bytes_in_per_sec.to_string())
                    .style(Style::default().fg(Color::White).bg(bg)),
                Cell::from(r.bytes_out_per_sec.to_string())
                    .style(Style::default().fg(Color::White).bg(bg)),
            ])
        });

        let table = Table::new(
            body_rows.skip(app.scroll),
            [
                Constraint::Length(8),
                Constraint::Length(16),
                Constraint::Length(12),
                Constraint::Length(22),
                Constraint::Length(22),
                Constraint::Length(7),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(table, chunks[table_chunk_idx]);
    }

    let footer = Paragraph::new(
        "q: quit | e: export CSV | s: sort by In | S: sort by Out | p: sort by PID | n: sort by Process | a: dismiss alert",
    )
        .style(Style::default().fg(Color::White));
    frame.render_widget(footer, chunks[footer_chunk_idx]);
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: Receiver<Vec<ConnectionSnapshot>>,
    alert_rx: Receiver<AlertEvent>,
) -> io::Result<()> {
    let mut app = AppState::new();

    loop {
        app.drain_updates(&rx);
        app.drain_alerts(&alert_rx);
        app.tick_timers();
        terminal.draw(|f| draw_ui(f, &app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break,
                    KeyCode::Char('a') => {
                        app.active_alert = None;
                        app.alert_arrived_at = None;
                        app.alert_cleared_at = Some(Instant::now());
                    }
                    KeyCode::Char('e') => {
                        app.export_status = Some(match export_csv() {
                            Ok(filename) => format!("Exported to {filename}"),
                            Err(err) => format!("Export failed: {err}"),
                        });
                        app.export_status_set_at = Some(Instant::now());
                    }
                    KeyCode::Char('s') => {
                        app.sort_mode = SortMode::InDesc;
                    }
                    KeyCode::Char('S') => {
                        app.sort_mode = SortMode::OutDesc;
                    }
                    KeyCode::Char('p') => {
                        app.sort_mode = SortMode::PidAsc;
                    }
                    KeyCode::Char('n') => {
                        app.sort_mode = SortMode::ProcessAsc;
                    }
                    KeyCode::Up => {
                        if app.scroll > 0 {
                            app.scroll -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if app.scroll + 1 < app.active_len() {
                            app.scroll += 1;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    Ok(())
}

fn main() -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<Vec<ConnectionSnapshot>>();
    let (alert_tx, alert_rx) = mpsc::channel::<AlertEvent>();
    spawn_socket_thread(tx, alert_tx);

    let cleanup = TerminalCleanup::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, rx, alert_rx);
    drop(terminal);
    drop(cleanup);
    result
}

fn export_csv() -> io::Result<String> {
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
            Err(err) => return Err(err),
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
