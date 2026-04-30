use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use iced::widget::{button, canvas::Canvas, column, container, row, scrollable, text, Column};
use iced::{alignment, time, Font, Length, Color, Element, Subscription, Task};
use serde_json;

use crate::data::{AlertEvent, ConnectionKey, ConnectionSnapshot, Message, SortMode, TrackedConnection, HISTORY_LEN};

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
}

impl NetMonitor {
    pub fn new() -> (Self, Task<Message>) {
        let app = Self {
            tracked: HashMap::new(),
            scroll: 0,
            last_snapshot_at: None,
            sort_mode: SortMode::OutDesc,
            in_history: std::collections::VecDeque::with_capacity(HISTORY_LEN + 4),
            out_history: std::collections::VecDeque::with_capacity(HISTORY_LEN + 4),
            active_alert: None,
            alert_arrived_at: None,
            export_status: None,
            stream: None,
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
                    if let Some(alert_json) = trimmed.strip_prefix("ALERT ") {
                        if let Ok(alert) = serde_json::from_str::<AlertEvent>(alert_json) {
                            messages.push(Message::AlertReceived(alert));
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
        self.scroll = 0;
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

            Message::SocketData(snaps) => {
                self.apply_snapshots(snaps);
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
                text("  │  eth0  │  ").size(13).color(MUTED).font(Font::MONOSPACE),
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
        let col_widths: [f32; 8] = [70.0, 130.0, 100.0, 180.0, 180.0, 60.0, 90.0, 90.0];

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
                        _ => MUTED,
                    };

                    let in_rate = r.bytes_in_per_sec;
                    let out_rate = r.bytes_out_per_sec;
                    let in_color = if in_rate > 100_000 { CYAN } else { WHITE };
                    let out_color = if out_rate > 100_000 { AMBER } else { WHITE };

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
                        ])
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

        // ── Footer ──
        let conn_count = self.tracked.len();
        let footer = container(
            row![
                text(format!("{conn_count} connections"))
                    .size(11)
                    .color(MUTED)
                    .font(Font::MONOSPACE),
                iced::widget::Space::with_width(Length::Fill),
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

        // ── Assemble ──
        let mut main_col: Vec<Element<Message>> = vec![header.into()];

        if let Some(banner) = alert_banner {
            main_col.push(banner);
        }
        if let Some(exp) = export_el {
            main_col.push(exp);
        }

        main_col.push(chart_section.into());
        main_col.push(sort_bar.into());
        main_col.push(table_section.height(Length::Fill).into());
        main_col.push(footer.into());

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
        time::every(Duration::from_millis(250)).map(|_| Message::Tick)
    }
}