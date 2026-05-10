use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use iced::widget::{button, canvas::Canvas, column, container, row, scrollable, text, text_input, Column, MouseArea};
use iced::{alignment, time, Font, Length, Color, Element, Subscription, Task};
use serde_json;

use crate::data::{
    AggregatePayload, AlertEvent, AutoKillEvent, ConnectionKey, ConnectionSnapshot, KnownPidLimit,
    MainTab, Message, SortMode, TrackedConnection, HISTORY_LEN,
};

// ─── App state ────────────────────────────────────────────────────────────────

pub struct NetMonitor {
    pub tracked: HashMap<ConnectionKey, TrackedConnection>,
    pub scroll: usize,
    pub last_snapshot_at: Option<Instant>,
    pub sort_mode: SortMode,
    pub in_history: VecDeque<f32>,
    pub out_history: VecDeque<f32>,
    pub active_alert: Option<AlertEvent>,
    pub alert_arrived_at: Option<Instant>,
    pub export_status: Option<(String, Instant)>,

    // Socket polling state
    pub stream: Option<BufReader<UnixStream>>,
    // New fields
    pub filter_text: String,
    pub is_monitoring: bool,
    pub session_start: Option<Instant>,
    pub context_menu: Option<ConnectionSnapshot>,
    /// Set from backend `IFACE` line over the socket (see `backend/src/ipc.rs`).
    pub iface_name: String,
    pub active_tab: MainTab,
    pub latest_aggregate: Option<AggregatePayload>,
    /// Cumulative bytes per (process_label, username), updated from each `AGGREGATE` payload.
    pub session_totals: HashMap<(String, String), (u64, u64)>,
    /// Pending manual stop confirmation `(pid, display name)`.
    pub kill_pending: Option<(u32, String)>,
    /// Per-PID limits mirrored after successful `limit set` / `limit total set` / cleared on remove or `AUTO_KILL`.
    pub known_limits: BTreeMap<u32, KnownPidLimit>,
    pub limit_pid_input: String,
    pub limit_mbps_input: String,
    pub limit_total_mb_input: String,
}

impl NetMonitor {
    pub fn new() -> (Self, Task<Message>) {
        let app = Self {
            tracked: HashMap::new(),
            scroll: 0,
            last_snapshot_at: None,
            sort_mode: SortMode::OutDesc,
            in_history: VecDeque::with_capacity(HISTORY_LEN + 4),
            out_history: VecDeque::with_capacity(HISTORY_LEN + 4),
            active_alert: None,
            alert_arrived_at: None,
            export_status: None,
            stream: None,
            filter_text: String::new(),
            is_monitoring: true,
            session_start: Some(Instant::now()),
            context_menu: None,
            iface_name: "…".to_string(),
            active_tab: MainTab::Connections,
            latest_aggregate: None,
            session_totals: HashMap::new(),
            kill_pending: None,
            known_limits: BTreeMap::new(),
            limit_pid_input: String::new(),
            limit_mbps_input: String::new(),
            limit_total_mb_input: String::new(),
        };
        (app, Task::none())
    }

    pub fn connected(&self) -> bool {
        matches!(self.last_snapshot_at, Some(t) if t.elapsed() < Duration::from_secs(3))
    }

    pub fn sorted_rows(&self) -> Vec<ConnectionSnapshot> {
        let mut rows: Vec<ConnectionSnapshot> = self
            .tracked
            .values()
            .map(|t| t.snapshot.clone())
            .collect();

        // Filter based on filter_text
        if !self.filter_text.is_empty() {
            let filter = self.filter_text.to_lowercase();
            rows.retain(|r| {
                r.process_name.as_deref().unwrap_or("").to_lowercase().contains(&filter)
                    || r.src_ip.to_lowercase().contains(&filter)
                    || r.dst_ip.to_lowercase().contains(&filter)
                    || r.src_port.to_string().contains(&filter)
                    || r.dst_port.to_string().contains(&filter)
                    || r.username.as_deref().unwrap_or("").to_lowercase().contains(&filter)
            });
        }

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

    fn clamp_table_scroll(&mut self) {
        let n = self.sorted_rows().len();
        self.scroll = if n == 0 {
            0
        } else {
            self.scroll.min(n.saturating_sub(1))
        };
    }

    pub fn poll_socket(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();

        // Try to (re)connect
        if self.stream.is_none() {
            match UnixStream::connect(crate::data::SOCKET_PATH) {
                Ok(s) => {
                    // Keep blocking reads — we'll use read_timeout instead of
                    // non-blocking so BufReader::read_line works correctly.
                    // A very short timeout means we won't stall the UI.
                    let _ = s.set_read_timeout(Some(Duration::from_millis(5)));
                    let mut s = s;
                    if s.write_all(b"stream\n").is_ok() {
                        self.stream = Some(BufReader::new(s));
                    }
                }
                Err(_) => return messages,
            }
        }

        let reader = match self.stream.as_mut() {
            Some(r) => r,
            None => return messages,
        };

        // Drain all lines available within our timeout window
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF — backend closed the connection
                    self.stream = None;
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(iface) = trimmed.strip_prefix("IFACE ") {
                        messages.push(Message::IfaceMeta(iface.to_string()));
                        continue;
                    }
                    if let Some(alert_json) = trimmed.strip_prefix("ALERT ") {
                        if let Ok(alert) = serde_json::from_str::<AlertEvent>(alert_json) {
                            messages.push(Message::AlertReceived(alert));
                        }
                        continue;
                    }
                    if let Some(auto_json) = trimmed.strip_prefix("AUTO_KILL ") {
                        if let Ok(ev) = serde_json::from_str::<AutoKillEvent>(auto_json) {
                            messages.push(Message::AutoKillReceived(ev));
                        }
                        continue;
                    }
                    if let Some(agg_json) = trimmed.strip_prefix("AGGREGATE ") {
                        if let Ok(agg) = serde_json::from_str::<AggregatePayload>(agg_json) {
                            messages.push(Message::AggregateData(agg));
                        }
                        continue;
                    }
                    if let Ok(snaps) = serde_json::from_str::<Vec<ConnectionSnapshot>>(trimmed) {
                        messages.push(Message::SocketData(snaps));
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // No more data right now — come back next tick
                    break;
                }
                Err(_) => {
                    // Broken pipe or other error — reconnect next tick
                    self.stream = None;
                    break;
                }
            }
        }

        messages
    }

    pub fn apply_snapshots(&mut self, snapshots: Vec<ConnectionSnapshot>) {
        let now = Instant::now();
        let total_in: f32 = snapshots.iter().map(|r| r.bytes_in_per_sec as f32).sum();
        let total_out: f32 = snapshots.iter().map(|r| r.bytes_out_per_sec as f32).sum();

        self.in_history.push_back(total_in);
        self.out_history.push_back(total_out);
        while self.in_history.len() > HISTORY_LEN {
            self.in_history.pop_front();
        }
        while self.out_history.len() > HISTORY_LEN {
            self.out_history.pop_front();
        }

        for snap in snapshots {
            let key = ConnectionKey {
                src_ip: snap.src_ip.clone(),
                src_port: snap.src_port,
                dst_ip: snap.dst_ip.clone(),
                dst_port: snap.dst_port,
                protocol: snap.protocol.clone(),
            };
            self.tracked.insert(key, TrackedConnection { snapshot: snap, last_seen: now });
        }

        self.tracked
            .retain(|_, t| t.last_seen.elapsed() < Duration::from_secs(3));

        self.last_snapshot_at = Some(now);
        self.clamp_table_scroll();
    }
}

// ─── Update ───────────────────────────────────────────────────────────────────

impl NetMonitor {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                // Expire alert
                if let Some(t) = self.alert_arrived_at {
                    if t.elapsed() > Duration::from_secs(5) {
                        self.active_alert = None;
                        self.alert_arrived_at = None;
                    }
                }
                // Expire export status
                if let Some((_, t)) = &self.export_status {
                    if t.elapsed() > Duration::from_secs(4) {
                        self.export_status = None;
                    }
                }

                let msgs = self.poll_socket();
                for msg in msgs {
                    let _ = self.update(msg);
                }
                Task::none()
            }

            Message::IfaceMeta(name) => {
                self.iface_name = name;
                Task::none()
            }

            Message::SocketData(snaps) => {
                self.apply_snapshots(snaps);
                Task::none()
            }

            Message::AggregateData(agg) => {
                for p in &agg.by_process {
                    let e = self
                        .session_totals
                        .entry((p.process_label.clone(), p.username.clone()))
                        .or_insert((0, 0));
                    e.0 = e.0.saturating_add(p.bytes_in_per_sec);
                    e.1 = e.1.saturating_add(p.bytes_out_per_sec);
                }
                self.latest_aggregate = Some(agg);
                Task::none()
            }

            Message::ResetSessionTotals => {
                self.session_totals.clear();
                Task::none()
            }

            Message::SetTab(tab) => {
                self.active_tab = tab;
                self.context_menu = None;
                Task::none()
            }

            Message::AlertReceived(alert) => {
                self.active_alert = Some(alert);
                self.alert_arrived_at = Some(Instant::now());
                Task::none()
            }

            Message::DismissAlert => {
                self.active_alert = None;
                self.alert_arrived_at = None;
                Task::none()
            }

            Message::SetSort(mode) => {
                self.sort_mode = mode;
                self.scroll = 0;
                Task::none()
            }

            Message::ExportCsv => Task::perform(
                async {
                    match crate::export::do_export_csv() {
                        Ok(name) => Ok(name),
                        Err(e) => Err(e.to_string()),
                    }
                },
                Message::ExportResult,
            ),

            Message::ExportResult(result) => {
                let msg = match result {
                    Ok(name) => format!("✓ Exported to {name}"),
                    Err(e) => format!("✗ Export failed: {e}"),
                };
                self.export_status = Some((msg, Instant::now()));
                Task::none()
            }

            Message::SetFilter(text) => {
                self.filter_text = text;
                self.context_menu = None;
                self.scroll = 0;
                Task::none()
            }

            Message::ToggleMonitor => {
                self.is_monitoring = !self.is_monitoring;
                Task::none()
            }

            Message::ShowContextMenu(conn) => {
                self.context_menu = Some(conn);
                Task::none()
            }

            Message::OpenKillConfirm { pid, label } => {
                if pid > 1 {
                    self.kill_pending = Some((pid, label));
                }
                self.context_menu = None;
                Task::none()
            }

            Message::CancelKillConfirm => {
                self.kill_pending = None;
                Task::none()
            }

            Message::ConfirmKillProcess => {
                let Some((pid, _)) = self.kill_pending else {
                    return Task::none();
                };
                self.kill_pending = None;
                Task::perform(
                    async move {
                        match tokio::task::spawn_blocking(move || crate::ipc_cmd::kill_process(pid)).await {
                            Ok(inner) => inner,
                            Err(e) => Err(format!("{e}")),
                        }
                    },
                    Message::KillRequestDone,
                )
            }

            Message::KillRequestDone(result) => {
                let msg = match &result {
                    Ok(()) => "✓ Process sent SIGTERM (stop requested)".to_string(),
                    Err(e) => format!("✗ Stop failed: {e}"),
                };
                self.export_status = Some((msg, Instant::now()));
                Task::none()
            }

            Message::LimitPidInput(s) => {
                self.limit_pid_input = s;
                Task::none()
            }

            Message::LimitMbpsInput(s) => {
                self.limit_mbps_input = s;
                Task::none()
            }

            Message::LimitTotalMbInput(s) => {
                self.limit_total_mb_input = s;
                Task::none()
            }

            Message::SubmitLimit => {
                let pid: u32 = match self.limit_pid_input.trim().parse() {
                    Ok(p) if p > 1 => p,
                    _ => {
                        self.export_status =
                            Some(("✗ Invalid PID (need integer > 1)".into(), Instant::now()));
                        return Task::none();
                    }
                };
                let mbps: f64 = match self.limit_mbps_input.trim().parse::<f64>() {
                    Ok(v) if v.is_finite() && v > 0.0 => v,
                    _ => {
                        self.export_status =
                            Some(("✗ Invalid MB/s (need positive number)".into(), Instant::now()));
                        return Task::none();
                    }
                };
                let bps = (mbps * 1_000_000.0).max(1.0) as u64;
                Task::perform(
                    async move {
                        match tokio::task::spawn_blocking(move || {
                            crate::ipc_cmd::limit_set(pid, bps).map(|_| (pid, bps))
                        })
                        .await
                        {
                            Ok(inner) => inner,
                            Err(e) => Err(format!("{e}")),
                        }
                    },
                    Message::LimitSetDone,
                )
            }

            Message::LimitSetDone(result) => {
                match result {
                    Ok((pid, bps)) => {
                        self.known_limits.entry(pid).or_default().rate_bps = Some(bps);
                        self.export_status = Some((
                            format!("✓ Rate limit set: PID {pid} at {} MB/s", bps as f64 / 1_000_000.0),
                            Instant::now(),
                        ));
                        self.limit_pid_input.clear();
                        self.limit_mbps_input.clear();
                    }
                    Err(e) => {
                        self.export_status = Some((format!("✗ Limit set failed: {e}"), Instant::now()));
                    }
                }
                Task::none()
            }

            Message::SubmitTotalLimit => {
                let pid: u32 = match self.limit_pid_input.trim().parse() {
                    Ok(p) if p > 1 => p,
                    _ => {
                        self.export_status =
                            Some(("✗ Invalid PID (need integer > 1)".into(), Instant::now()));
                        return Task::none();
                    }
                };
                let mb: f64 = match self.limit_total_mb_input.trim().parse::<f64>() {
                    Ok(v) if v.is_finite() && v > 0.0 => v,
                    _ => {
                        self.export_status =
                            Some(("✗ Invalid total MB (need positive number)".into(), Instant::now()));
                        return Task::none();
                    }
                };
                let max_bytes = (mb * 1_000_000.0).max(1.0) as u64;
                Task::perform(
                    async move {
                        match tokio::task::spawn_blocking(move || {
                            crate::ipc_cmd::limit_total_set(pid, max_bytes).map(|_| (pid, max_bytes))
                        })
                        .await
                        {
                            Ok(inner) => inner,
                            Err(e) => Err(format!("{e}")),
                        }
                    },
                    Message::LimitTotalSetDone,
                )
            }

            Message::LimitTotalSetDone(result) => {
                match result {
                    Ok((pid, max_bytes)) => {
                        self.known_limits.entry(pid).or_default().total_max_bytes = Some(max_bytes);
                        self.export_status = Some((
                            format!(
                                "✓ Total cap set: PID {pid} at {:.3} MB cumulative",
                                max_bytes as f64 / 1_000_000.0
                            ),
                            Instant::now(),
                        ));
                        self.limit_pid_input.clear();
                        self.limit_total_mb_input.clear();
                    }
                    Err(e) => {
                        self.export_status =
                            Some((format!("✗ Total cap set failed: {e}"), Instant::now()));
                    }
                }
                Task::none()
            }

            Message::RemoveLimitPid(pid) => Task::perform(
                async move {
                    match tokio::task::spawn_blocking(move || crate::ipc_cmd::limit_clear(pid).map(|_| pid)).await
                    {
                        Ok(inner) => inner,
                        Err(e) => Err(format!("{e}")),
                    }
                },
                Message::LimitClearDone,
            ),

            Message::LimitClearDone(result) => {
                match result {
                    Ok(pid) => {
                        self.known_limits.remove(&pid);
                        self.export_status =
                            Some((format!("✓ Cleared limit for PID {pid}"), Instant::now()));
                    }
                    Err(e) => {
                        self.export_status = Some((format!("✗ Clear limit failed: {e}"), Instant::now()));
                    }
                }
                Task::none()
            }

            Message::AutoKillReceived(ev) => {
                self.known_limits.remove(&ev.pid);
                let name = ev
                    .process_name
                    .as_deref()
                    .unwrap_or("(unknown)");
                let msg = if ev.reason == "total" {
                    format!(
                        "⚠ [{}] Auto-stopped PID {} ({}) — cumulative {:.3} MB reached cap {:.3} MB",
                        ev.timestamp_unix,
                        ev.pid,
                        name,
                        ev.observed_total_bytes.unwrap_or(0) as f64 / 1_000_000.0,
                        ev.threshold_total_bytes.unwrap_or(0) as f64 / 1_000_000.0
                    )
                } else {
                    let obs = ev.observed_bps.unwrap_or(0);
                    let thr = ev.threshold_bps.unwrap_or(0);
                    format!(
                        "⚠ [{}] Auto-stopped PID {} ({}) — {:.3} MB/s exceeded rate cap {:.3} MB/s",
                        ev.timestamp_unix,
                        ev.pid,
                        name,
                        obs as f64 / 1_000_000.0,
                        thr as f64 / 1_000_000.0
                    )
                };
                self.export_status = Some((msg, Instant::now()));
                Task::none()
            }
        }
    }
}

// ─── View ─────────────────────────────────────────────────────────────────────

impl NetMonitor {
    pub fn view(&self) -> Element<'_, Message> {
        use crate::view::*;
        let rows = self.sorted_rows();
        let total_in: u64 = rows.iter().map(|r| r.bytes_in_per_sec).sum();
        let total_out: u64 = rows.iter().map(|r| r.bytes_out_per_sec).sum();

        // ── Header bar ──
        let status_color = if self.connected() { GREEN } else { RED };
        let status_label = if self.connected() { "● LIVE" } else { "○ OFFLINE" };

        let header = container(
            row![
                text("NET MONITOR")
                    .size(15)
                    .font(Font::MONOSPACE)
                    .color(CYAN),
                text(format!("  │  {}  │  ", self.iface_name))
                    .size(13)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
                text("↓ ").size(13).color(CYAN).font(Font::MONOSPACE),
                text(fmt_bytes(total_in)).size(13).color(WHITE).font(Font::MONOSPACE),
                text("   ↑ ").size(13).color(AMBER).font(Font::MONOSPACE),
                text(fmt_bytes(total_out)).size(13).color(WHITE).font(Font::MONOSPACE),
                text("   ").size(13).color(MUTED).font(Font::MONOSPACE),
                text(status_label).size(13).color(status_color).font(Font::MONOSPACE),
            ]
            .align_y(alignment::Vertical::Center),
        )
        .padding([10, 16])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 0.0, radius: 0.0.into() },
            ..Default::default()
        });

        // ── Alert banner ──
        let alert_banner: Option<Element<Message>> = self.active_alert.as_ref().map(|alert| {
            container(
                row![
                    text("⚠  ").size(14).color(WHITE).font(Font::MONOSPACE),
                    text(alert.message.clone()).size(13).color(WHITE).font(Font::MONOSPACE),
                    iced::widget::Space::with_width(Length::Fill),
                    button(text("✕ dismiss").size(12).color(WHITE).font(Font::MONOSPACE))
                        .on_press(Message::DismissAlert)
                        .style(|_, _| button::Style {
                            background: Some(iced::Background::Color(
                                Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                            )),
                            border: iced::Border {
                                color: Color::from_rgba(1.0, 1.0, 1.0, 0.3),
                                width: 1.0,
                                radius: 4.0.into(),
                            },
                            ..Default::default()
                        })
                        .padding([4, 10]),
                ]
                .align_y(alignment::Vertical::Center),
            )
            .padding([10, 16])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(Color::from_rgb(0.65, 0.10, 0.10))),
                border: iced::Border {
                    color: Color::from_rgb(1.0, 0.30, 0.30),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
        });

        // ── Export status ──
        let export_el: Option<Element<Message>> =
            self.export_status.as_ref().map(|(msg, _)| {
                let color = if msg.starts_with('✓') { GREEN } else { RED };
                container(text(msg.clone()).size(12).color(color).font(Font::MONOSPACE))
                    .padding([4, 16])
                    .width(Length::Fill)
                    .into()
            });

        // ── Bandwidth chart ──
        let chart_widget = Canvas::new(crate::chart::BandwidthProgram {
            in_history: &self.in_history,
            out_history: &self.out_history,
        })
        .width(Length::Fill)
        .height(Length::Fixed(120.0));

        let chart_legend = row![
            text("━ ").size(13).color(CYAN).font(Font::MONOSPACE),
            text("Inbound  ").size(12).color(MUTED).font(Font::MONOSPACE),
            text("━ ").size(13).color(AMBER).font(Font::MONOSPACE),
            text("Outbound").size(12).color(MUTED).font(Font::MONOSPACE),
        ]
        .spacing(4);

        let chart_section = container(
            column![
                row![
                    text("BANDWIDTH  (last 60s)").size(11).color(MUTED).font(Font::MONOSPACE),
                    iced::widget::Space::with_width(Length::Fill),
                    chart_legend,
                ]
                .align_y(alignment::Vertical::Center),
                chart_widget,
            ]
            .spacing(0),
        )
        .padding(12)
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        let tab_bar = container(
            row![
                tab_btn("Connections", MainTab::Connections, self.active_tab),
                tab_btn("Aggregate", MainTab::Aggregate, self.active_tab),
                tab_btn("Session totals", MainTab::SessionTotals, self.active_tab),
                tab_btn("Limits", MainTab::Limits, self.active_tab),
                iced::widget::Space::with_width(Length::Fill),
            ]
            .spacing(8)
            .align_y(alignment::Vertical::Center),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        // ── Toolbar ──
        let toolbar = container(
            row![
                text("FILTER:").size(11).color(MUTED).font(Font::MONOSPACE),
                text_input("process name, IP, port, user", &self.filter_text)
                    .on_input(Message::SetFilter)
                    .padding([4, 8])
                    .size(12)
                    .font(Font::MONOSPACE),
                iced::widget::Space::with_width(Length::Fill),
                button(
                    text(if self.is_monitoring { "⏸ Stop Monitor" } else { "▶ Start Monitor" })
                        .size(12)
                        .color(WHITE)
                        .font(Font::MONOSPACE),
                )
                .on_press(Message::ToggleMonitor)
                .style(|_, _| button::Style {
                    background: Some(iced::Background::Color(if self.is_monitoring { RED } else { GREEN })),
                    border: iced::Border {
                        color: BORDER,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
                .padding([4, 10]),
            ]
            .spacing(8)
            .align_y(alignment::Vertical::Center),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        // ── Sort controls ──
        let sort_bar = container(
            row![
                text("SORT:").size(11).color(MUTED).font(Font::MONOSPACE),
                sort_btn("↓ Out", SortMode::OutDesc, self.sort_mode),
                sort_btn("↓ In", SortMode::InDesc, self.sort_mode),
                sort_btn("PID", SortMode::PidAsc, self.sort_mode),
                sort_btn("Process", SortMode::ProcessAsc, self.sort_mode),
                iced::widget::Space::with_width(Length::Fill),
                button(
                    text("⬇ Export CSV").size(12).color(AMBER).font(Font::MONOSPACE),
                )
                .on_press(Message::ExportCsv)
                .style(|_, _| button::Style {
                    background: Some(iced::Background::Color(
                        Color::from_rgba(1.0, 0.75, 0.2, 0.12),
                    )),
                    border: iced::Border {
                        color: AMBER,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
                .padding([4, 10]),
            ]
            .spacing(6)
            .align_y(alignment::Vertical::Center),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        // ── Table ──
        let col_widths: [f32; 9] = [64.0, 118.0, 88.0, 158.0, 158.0, 50.0, 78.0, 78.0, 72.0];

        let header_row = container(
            row(vec![
                container(header_cell("PID")).width(Length::Fixed(col_widths[0])).into(),
                container(header_cell("PROCESS")).width(Length::Fixed(col_widths[1])).into(),
                container(header_cell("USER")).width(Length::Fixed(col_widths[2])).into(),
                container(header_cell("SRC IP:PORT")).width(Length::Fixed(col_widths[3])).into(),
                container(header_cell("DST IP:PORT")).width(Length::Fixed(col_widths[4])).into(),
                container(header_cell("PROTO")).width(Length::Fixed(col_widths[5])).into(),
                container(header_cell("IN")).width(Length::Fixed(col_widths[6])).into(),
                container(header_cell("OUT")).width(Length::Fixed(col_widths[7])).into(),
                container(header_cell("STOP")).width(Length::Fixed(col_widths[8])).into(),
            ])
        )
        .padding([2, 4])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE2)),
            ..Default::default()
        });

        let table_body: Element<Message> = if self.last_snapshot_at.is_none() {
            container(
                text("Waiting for backend connection…")
                    .size(14)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .padding(40)
            .into()
        } else {
            let data_rows: Vec<Element<Message>> = rows
                .iter()
                .skip(self.scroll)
                .enumerate()
                .map(|(idx, r)| {
                    let bg = if idx % 2 == 0 { BG } else { SURFACE };
                    let proto_color = match r.protocol.as_str() {
                        "TCP" => GREEN,
                        "UDP" => AMBER,
                        "SCTP" => SCTP,
                        "ICMPv4" => ICMP_V4,
                        "ICMPv6" => ICMP_V6,
                        _ => MUTED,
                    };

                    let in_rate = r.bytes_in_per_sec;
                    let out_rate = r.bytes_out_per_sec;
                    let in_color = if in_rate > 100_000 { CYAN } else { WHITE };
                    let out_color = if out_rate > 100_000 { AMBER } else { WHITE };

                    MouseArea::new(
                        container(
                            row(vec![
                                container(cell(
                                    r.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()),
                                    MUTED,
                                ))
                                .width(Length::Fixed(col_widths[0]))
                                .into(),
                                container(cell(
                                    r.process_name.as_deref().unwrap_or("—").to_string(),
                                    WHITE,
                                ))
                                .width(Length::Fixed(col_widths[1]))
                                .into(),
                                container(cell(
                                    r.username.as_deref().unwrap_or("—").to_string(),
                                    MUTED,
                                ))
                                .width(Length::Fixed(col_widths[2]))
                                .into(),
                                container(cell(
                                    format!("{}:{}", r.src_ip, r.src_port),
                                    WHITE,
                                ))
                                .width(Length::Fixed(col_widths[3]))
                                .into(),
                                container(cell(
                                    format!("{}:{}", r.dst_ip, r.dst_port),
                                    MUTED,
                                ))
                                .width(Length::Fixed(col_widths[4]))
                                .into(),
                                container(cell(r.protocol.clone(), proto_color))
                                    .width(Length::Fixed(col_widths[5]))
                                    .into(),
                                container(cell(fmt_bytes(in_rate), in_color))
                                    .width(Length::Fixed(col_widths[6]))
                                    .into(),
                                container(cell(fmt_bytes(out_rate), out_color))
                                    .width(Length::Fixed(col_widths[7]))
                                    .into(),
                                {
                                    let stop_cell: Element<Message> = if let Some(pid) = r.pid {
                                        if pid > 1 {
                                            button(text("Stop").size(11).color(RED).font(Font::MONOSPACE))
                                                .on_press(Message::OpenKillConfirm {
                                                    pid,
                                                    label: r
                                                        .process_name
                                                        .clone()
                                                        .unwrap_or_else(|| "(unknown)".into()),
                                                })
                                                .style(|_, _| button::Style {
                                                    background: Some(iced::Background::Color(
                                                        SURFACE2,
                                                    )),
                                                    border: iced::Border {
                                                        color: RED,
                                                        width: 1.0,
                                                        radius: 4.0.into(),
                                                    },
                                                    ..Default::default()
                                                })
                                                .padding([2, 6])
                                                .into()
                                        } else {
                                            container(text("—").size(11).color(MUTED).font(Font::MONOSPACE))
                                                .padding([4, 8])
                                                .width(Length::Fill)
                                                .into()
                                        }
                                    } else {
                                        container(text("—").size(11).color(MUTED).font(Font::MONOSPACE))
                                            .padding([4, 8])
                                            .width(Length::Fill)
                                            .into()
                                    };
                                    container(stop_cell)
                                        .width(Length::Fixed(col_widths[8]))
                                        .into()
                                },
                            ])
                        )
                        .padding([0, 4])
                        .width(Length::Fill)
                        .style(move |_| container::Style {
                            background: Some(iced::Background::Color(bg)),
                            ..Default::default()
                        })
                    )
                    .on_right_press(Message::ShowContextMenu(r.clone()))
                    .into()
                })
                .collect();

            scrollable(
                Column::with_children(data_rows).spacing(0).width(Length::Fill),
            )
            .width(Length::Fill)
            .into()
        };

        let table_section = container(
            column![header_row, table_body,].spacing(0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(BG)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        let connections_panel: Element<Message> = column![
            toolbar,
            sort_bar,
            table_section.height(Length::Fill),
        ]
        .spacing(0)
        .width(Length::Fill)
        .into();

        let pw = [200.0f32, 110.0, 72.0, 100.0, 100.0];
        let aggregate_panel: Element<Message> = if self.last_snapshot_at.is_none() {
            container(
                text("Waiting for backend connection…")
                    .size(14)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .padding(40)
            .into()
        } else if self.latest_aggregate.is_none() {
            container(
                text("Connected — waiting for aggregate rollups…")
                    .size(14)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .padding(40)
            .into()
        } else {
            let agg = self.latest_aggregate.as_ref().unwrap();
            let proc_header = container(
                row(vec![
                    container(header_cell("PROCESS"))
                        .width(Length::Fixed(pw[0]))
                        .into(),
                    container(header_cell("USER"))
                        .width(Length::Fixed(pw[1]))
                        .into(),
                    container(header_cell("FLOWS"))
                        .width(Length::Fixed(pw[2]))
                        .into(),
                    container(header_cell("IN"))
                        .width(Length::Fixed(pw[3]))
                        .into(),
                    container(header_cell("OUT"))
                        .width(Length::Fixed(pw[4]))
                        .into(),
                ]),
            )
            .padding([2, 4])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(SURFACE2)),
                ..Default::default()
            });

            let proc_rows: Vec<Element<Message>> = agg
                .by_process
                .iter()
                .enumerate()
                .map(|(idx, p)| {
                    let bg = if idx % 2 == 0 { BG } else { SURFACE };
                    let ti = p.bytes_in_per_sec;
                    let to = p.bytes_out_per_sec;
                    container(
                        row(vec![
                            container(cell(p.process_label.clone(), WHITE))
                                .width(Length::Fixed(pw[0]))
                                .into(),
                            container(cell(p.username.clone(), MUTED))
                                .width(Length::Fixed(pw[1]))
                                .into(),
                            container(cell(p.flow_count.to_string(), MUTED))
                                .width(Length::Fixed(pw[2]))
                                .into(),
                            container(cell(fmt_bytes(ti), CYAN))
                                .width(Length::Fixed(pw[3]))
                                .into(),
                            container(cell(fmt_bytes(to), AMBER))
                                .width(Length::Fixed(pw[4]))
                                .into(),
                        ]),
                    )
                    .padding([0, 4])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(iced::Background::Color(bg)),
                        ..Default::default()
                    })
                    .into()
                })
                .collect();

            let pr = [72.0f32, 80.0, 100.0, 100.0];
            let prot_header = container(
                row(vec![
                    container(header_cell("PROTO"))
                        .width(Length::Fixed(pr[0]))
                        .into(),
                    container(header_cell("FLOWS"))
                        .width(Length::Fixed(pr[1]))
                        .into(),
                    container(header_cell("IN"))
                        .width(Length::Fixed(pr[2]))
                        .into(),
                    container(header_cell("OUT"))
                        .width(Length::Fixed(pr[3]))
                        .into(),
                ]),
            )
            .padding([2, 4])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(SURFACE2)),
                ..Default::default()
            });

            let prot_rows: Vec<Element<Message>> = agg
                .by_protocol
                .iter()
                .enumerate()
                .map(|(idx, p)| {
                    let bg = if idx % 2 == 0 { BG } else { SURFACE };
                    let proto_color = match p.protocol.as_str() {
                        "TCP" => GREEN,
                        "UDP" => AMBER,
                        "SCTP" => SCTP,
                        "ICMPv4" => ICMP_V4,
                        "ICMPv6" => ICMP_V6,
                        _ => MUTED,
                    };
                    let ti = p.bytes_in_per_sec;
                    let to = p.bytes_out_per_sec;
                    container(
                        row(vec![
                            container(cell(p.protocol.clone(), proto_color))
                                .width(Length::Fixed(pr[0]))
                                .into(),
                            container(cell(p.flow_count.to_string(), MUTED))
                                .width(Length::Fixed(pr[1]))
                                .into(),
                            container(cell(fmt_bytes(ti), CYAN))
                                .width(Length::Fixed(pr[2]))
                                .into(),
                            container(cell(fmt_bytes(to), AMBER))
                                .width(Length::Fixed(pr[3]))
                                .into(),
                        ]),
                    )
                    .padding([0, 4])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(iced::Background::Color(bg)),
                        ..Default::default()
                    })
                    .into()
                })
                .collect();

            let ts = agg.timestamp_unix;
            container(
                column![
                    text(format!("Rollups for unix time {ts} (same 1s window as connection snapshots)"))
                        .size(11)
                        .color(MUTED)
                        .font(Font::MONOSPACE),
                    text("BY PROCESS").size(12).color(CYAN).font(Font::MONOSPACE),
                    proc_header,
                    scrollable(Column::with_children(proc_rows).spacing(0).width(Length::Fill))
                        .height(Length::Fill)
                        .width(Length::Fill),
                    text("BY PROTOCOL").size(12).color(CYAN).font(Font::MONOSPACE),
                    prot_header,
                    scrollable(Column::with_children(prot_rows).spacing(0).width(Length::Fill))
                        .height(Length::Fill)
                        .width(Length::Fill),
                ]
                .spacing(6)
                .width(Length::Fill),
            )
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(BG)),
                border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
                ..Default::default()
            })
            .into()
        };

        let session_intro = container(
            row![
                text("Per-process cumulative traffic since this GUI started (or since Reset). Each second adds that second's Aggregate BY PROCESS bytes.")
                    .size(11)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
                iced::widget::Space::with_width(Length::Fill),
                button(text("Reset totals").size(12).color(WHITE).font(Font::MONOSPACE))
                    .on_press(Message::ResetSessionTotals)
                    .style(|_, _| button::Style {
                        background: Some(iced::Background::Color(SURFACE2)),
                        border: iced::Border {
                            color: BORDER,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    })
                    .padding([4, 10]),
            ]
            .align_y(alignment::Vertical::Center),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        let tw = [200.0f32, 110.0, 110.0, 110.0, 110.0];
        let session_table_body: Element<Message> = if self.session_totals.is_empty() {
            container(
                text("Waiting for data…")
                    .size(14)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .padding(24)
            .into()
        } else {
            let mut rows_vec: Vec<((String, String), u64, u64)> = self
                .session_totals
                .iter()
                .map(|(k, &(bi, bo))| (k.clone(), bi, bo))
                .collect();
            rows_vec.sort_by(|a, b| {
                let ta = a.1.saturating_add(a.2);
                let tb = b.1.saturating_add(b.2);
                tb.cmp(&ta)
            });

            let st_header = container(
                row(vec![
                    container(header_cell("PROCESS"))
                        .width(Length::Fixed(tw[0]))
                        .into(),
                    container(header_cell("USER"))
                        .width(Length::Fixed(tw[1]))
                        .into(),
                    container(header_cell("TOTAL IN"))
                        .width(Length::Fixed(tw[2]))
                        .into(),
                    container(header_cell("TOTAL OUT"))
                        .width(Length::Fixed(tw[3]))
                        .into(),
                    container(header_cell("TOTAL"))
                        .width(Length::Fixed(tw[4]))
                        .into(),
                ]),
            )
            .padding([2, 4])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(SURFACE2)),
                ..Default::default()
            });

            let st_rows: Vec<Element<Message>> = rows_vec
                .into_iter()
                .enumerate()
                .map(|(idx, ((proc, user), bi, bo))| {
                    let bg = if idx % 2 == 0 { BG } else { SURFACE };
                    let tot = bi.saturating_add(bo);
                    container(
                        row(vec![
                            container(cell(proc, WHITE))
                                .width(Length::Fixed(tw[0]))
                                .into(),
                            container(cell(user, MUTED))
                                .width(Length::Fixed(tw[1]))
                                .into(),
                            container(cell(fmt_bytes_total(bi), CYAN))
                                .width(Length::Fixed(tw[2]))
                                .into(),
                            container(cell(fmt_bytes_total(bo), AMBER))
                                .width(Length::Fixed(tw[3]))
                                .into(),
                            container(cell(fmt_bytes_total(tot), WHITE))
                                .width(Length::Fixed(tw[4]))
                                .into(),
                        ]),
                    )
                    .padding([0, 4])
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(iced::Background::Color(bg)),
                        ..Default::default()
                    })
                    .into()
                })
                .collect();

            column![
                st_header,
                scrollable(Column::with_children(st_rows).spacing(0).width(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill),
            ]
            .spacing(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        };

        let session_totals_panel: Element<Message> = container(
            column![session_intro, session_table_body,]
                .spacing(8)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(BG)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into();

        let limits_intro = text(
            "Rate limit: max combined in+out per 1s snapshot (decimal MB/s × 1_000_000 → B/s). \
             Total cap: max cumulative in+out since you set it (decimal MB × 1_000_000 → bytes; counter resets on each `limit total set`). \
             A PID may have both; whichever trips first triggers SIGTERM and clears all limits for that PID.",
        )
        .size(11)
        .color(MUTED)
        .font(Font::MONOSPACE);

        let limits_pid_row = row![
            text("PID").size(11).color(MUTED).font(Font::MONOSPACE),
            text_input("e.g. 4242", &self.limit_pid_input)
                .on_input(Message::LimitPidInput)
                .padding([4, 8])
                .size(12)
                .width(Length::Fixed(120.0))
                .font(Font::MONOSPACE),
            iced::widget::Space::with_width(Length::Fill),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center);

        let limits_form_rate = row![
            text("Rate").size(11).color(CYAN).font(Font::MONOSPACE),
            text("MB/s").size(11).color(MUTED).font(Font::MONOSPACE),
            text_input("e.g. 10", &self.limit_mbps_input)
                .on_input(Message::LimitMbpsInput)
                .padding([4, 8])
                .size(12)
                .width(Length::Fixed(100.0))
                .font(Font::MONOSPACE),
            button(text("Set rate").size(12).color(WHITE).font(Font::MONOSPACE))
                .on_press(Message::SubmitLimit)
                .style(|_, _| button::Style {
                    background: Some(iced::Background::Color(CYAN)),
                    border: iced::Border {
                        color: BORDER,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
                .padding([4, 12]),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center);

        let limits_form_total = row![
            text("Total").size(11).color(AMBER).font(Font::MONOSPACE),
            text("MB cap").size(11).color(MUTED).font(Font::MONOSPACE),
            text_input("e.g. 500", &self.limit_total_mb_input)
                .on_input(Message::LimitTotalMbInput)
                .padding([4, 8])
                .size(12)
                .width(Length::Fixed(100.0))
                .font(Font::MONOSPACE),
            button(text("Set total").size(12).color(WHITE).font(Font::MONOSPACE))
                .on_press(Message::SubmitTotalLimit)
                .style(|_, _| button::Style {
                    background: Some(iced::Background::Color(AMBER)),
                    border: iced::Border {
                        color: BORDER,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
                .padding([4, 12]),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center);

        let limits_form = column![limits_pid_row, limits_form_rate, limits_form_total]
            .spacing(10)
            .width(Length::Fill);

        let limit_rows: Vec<Element<Message>> = {
            let rows: Vec<_> = self
                .known_limits
                .iter()
                .filter(|(_, kl)| kl.rate_bps.is_some() || kl.total_max_bytes.is_some())
                .map(|(&pid, kl)| {
                    let mut desc = String::new();
                    if let Some(bps) = kl.rate_bps {
                        let mb = bps as f64 / 1_000_000.0;
                        desc.push_str(&format!("rate ≤ {mb:.3} MB/s"));
                    }
                    if let Some(tb) = kl.total_max_bytes {
                        if !desc.is_empty() {
                            desc.push_str("  |  ");
                        }
                        let mb = tb as f64 / 1_000_000.0;
                        desc.push_str(&format!("total ≤ {mb:.3} MB"));
                    }
                    row![
                        text(format!("PID {pid}"))
                            .size(12)
                            .color(WHITE)
                            .font(Font::MONOSPACE),
                        text(desc)
                            .size(12)
                            .color(CYAN)
                            .font(Font::MONOSPACE),
                        iced::widget::Space::with_width(Length::Fill),
                        button(text("Remove").size(11).color(AMBER).font(Font::MONOSPACE))
                            .on_press(Message::RemoveLimitPid(pid))
                            .style(|_, _| button::Style {
                                background: Some(iced::Background::Color(SURFACE2)),
                                border: iced::Border {
                                    color: BORDER,
                                    width: 1.0,
                                    radius: 4.0.into(),
                                },
                                ..Default::default()
                            })
                            .padding([4, 10]),
                    ]
                    .spacing(12)
                    .align_y(alignment::Vertical::Center)
                    .into()
                })
                .collect();
            if rows.is_empty() {
                vec![text("No active limits (set one above, or they were cleared after auto-kill).")
                    .size(12)
                    .color(MUTED)
                    .font(Font::MONOSPACE)
                    .into()]
            } else {
                rows
            }
        };

        let limits_panel: Element<Message> = container(
            column![
                text("Traffic limits (auto-kill)")
                    .size(14)
                    .color(CYAN)
                    .font(Font::MONOSPACE),
                limits_intro,
                limits_form,
                text("Active limits")
                    .size(12)
                    .color(CYAN)
                    .font(Font::MONOSPACE),
                scrollable(Column::with_children(limit_rows).spacing(6).width(Length::Fill))
                    .height(Length::Fill)
                    .width(Length::Fill),
            ]
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(BG)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into();

        let main_body: Element<Message> = match self.active_tab {
            MainTab::Connections => connections_panel,
            MainTab::Aggregate => aggregate_panel,
            MainTab::SessionTotals => session_totals_panel,
            MainTab::Limits => limits_panel,
        };

        // ── Footer ──
        let conn_count = self.tracked.len();
        let session_time = if let Some(start) = self.session_start {
        let elapsed = start.elapsed();
        let total_secs = elapsed.as_secs();

        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;

        format!("Session: {:02}:{:02}:{:02}", hours, minutes, seconds)
        } else {
            "Session: N/A".to_string()
        };
        let footer = container(
            row![
                // LEFT GROUP
                row![
                    text(format!("{conn_count} connections"))
                        .size(11)
                        .color(MUTED)
                        .font(Font::MONOSPACE),

                    iced::widget::Space::with_width(Length::Fixed(16.0)), // small gap

                    text(session_time)
                        .size(11)
                        .color(MUTED)
                        .font(Font::MONOSPACE),
                ]
                .align_y(alignment::Vertical::Center),

                iced::widget::Space::with_width(Length::Fill),

                // RIGHT SIDE
                text("net-monitor v0.1  |  iced gui")
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.2))
                    .font(Font::MONOSPACE),
            ]
            .align_y(alignment::Vertical::Center),
        )
        .padding([6, 16])
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(SURFACE)),
            border: iced::Border { color: BORDER, width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

        let kill_confirm_banner: Option<Element<Message>> = self.kill_pending.as_ref().map(|(pid, label)| {
            container(
                row![
                    text(format!("Stop process \"{label}\" (PID {pid})?"))
                        .size(13)
                        .color(WHITE)
                        .font(Font::MONOSPACE),
                    iced::widget::Space::with_width(Length::Fill),
                    button(text("Cancel").size(12).color(WHITE).font(Font::MONOSPACE))
                        .on_press(Message::CancelKillConfirm)
                        .style(|_, _| button::Style {
                            background: Some(iced::Background::Color(SURFACE2)),
                            border: iced::Border {
                                color: BORDER,
                                width: 1.0,
                                radius: 4.0.into(),
                            },
                            ..Default::default()
                        })
                        .padding([4, 12]),
                    button(text("Confirm stop").size(12).color(WHITE).font(Font::MONOSPACE))
                        .on_press(Message::ConfirmKillProcess)
                        .style(|_, _| button::Style {
                            background: Some(iced::Background::Color(RED)),
                            border: iced::Border {
                                color: Color::from_rgb(1.0, 0.35, 0.35),
                                width: 1.0,
                                radius: 4.0.into(),
                            },
                            ..Default::default()
                        })
                        .padding([4, 12]),
                ]
                .align_y(alignment::Vertical::Center),
            )
            .padding([10, 16])
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(Color::from_rgb(0.35, 0.22, 0.08))),
                border: iced::Border {
                    color: AMBER,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
        });

        // ── Assemble ──
        let mut main_col: Vec<Element<Message>> = vec![header.into()];

        if let Some(banner) = kill_confirm_banner {
            main_col.push(banner);
        }
        if let Some(banner) = alert_banner {
            main_col.push(banner);
        }
        if let Some(exp) = export_el {
            main_col.push(exp);
        }

        main_col.push(chart_section.into());
        main_col.push(tab_bar.into());
        main_col.push(
            container(main_body)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        );
        main_col.push(footer.into());

        // Context menu if active
        if let Some(conn) = &self.context_menu {
            let stop_row: Element<Message> = if conn.pid.is_some_and(|pid| pid > 1) {
                let pid = conn.pid.unwrap_or(0);
                let label = conn
                    .process_name
                    .clone()
                    .unwrap_or_else(|| "(unknown)".into());
                button(text("Stop process…").size(12).color(RED).font(Font::MONOSPACE))
                    .on_press(Message::OpenKillConfirm { pid, label })
                    .style(|_, _| button::Style {
                        background: Some(iced::Background::Color(SURFACE)),
                        border: iced::Border::default(),
                        ..Default::default()
                    })
                    .padding([4, 8])
                    .into()
            } else {
                container(
                    text("Stop unavailable (no PID)")
                        .size(12)
                        .color(MUTED)
                        .font(Font::MONOSPACE),
                )
                .padding([4, 8])
                .into()
            };

            let menu = container(
                column![
                    button(
                        text(format!("Filter by user: {}", conn.username.as_deref().unwrap_or("—")))
                            .size(12)
                            .color(WHITE)
                            .font(Font::MONOSPACE)
                    )
                    .on_press(Message::SetFilter(conn.username.clone().unwrap_or_default()))
                    .style(|_, _| button::Style {
                        background: Some(iced::Background::Color(SURFACE)),
                        border: iced::Border::default(),
                        ..Default::default()
                    })
                    .padding([4, 8]),
                    button(
                        text(format!("Filter by process: {}", conn.process_name.as_deref().unwrap_or("—")))
                            .size(12)
                            .color(WHITE)
                            .font(Font::MONOSPACE)
                    )
                    .on_press(Message::SetFilter(conn.process_name.clone().unwrap_or_default()))
                    .style(|_, _| button::Style {
                        background: Some(iced::Background::Color(SURFACE)),
                        border: iced::Border::default(),
                        ..Default::default()
                    })
                    .padding([4, 8]),
                    stop_row,
                ]
                .spacing(2)
            )
            .padding(8)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(SURFACE2)),
                border: iced::Border { color: BORDER, width: 1.0, radius: 4.0.into() },
                ..Default::default()
            });
            main_col.push(menu.into());
        }

        container(Column::with_children(main_col).spacing(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Background::Color(BG)),
                ..Default::default()
            })
            .into()
    }
}

// ─── Subscription ─────────────────────────────────────────────────────────────

impl NetMonitor {
    pub fn subscription(&self) -> Subscription<Message> {
        if self.is_monitoring {
            time::every(Duration::from_millis(250)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }
}