# net-monitor

A real-time network traffic monitor for Linux, built in Rust. Captures packets, attributes connections to processes, and displays bandwidth usage via a GUI or TUI.

## Features

- **Packet capture** - Capture TCP, UDP, SCTP, ICMPv4, and ICMPv6 traffic via libpcap
- **Process attribution** - Link connections to running processes using `/proc` filesystem analysis
- **Real-time monitoring** - Live connection table with per-second traffic statistics
- **Bandwidth alerts** - Threshold-based alerts when bandwidth exceeds configured limits
- **Auto-kill** - Automatically terminate processes that exceed rate or total data limits
- **Aggregate views** - Roll up traffic by process or by protocol
- **Session history** - Store and export historical data as JSON or CSV
- **Dual interfaces** - Choose between a graphical UI (GUI) or terminal UI (TUI)

## Architecture

```
┌──────────────┐         Unix Socket           ┌───────────┐
│   Backend    │◄──────────────────────────────│    GUI    │
│  (packet     │         /tmp/                 │  (Iced)   │
│  capture)    │         net-monitor.sock      │           │
│              │                               └───────────┘
│  - libpcap   │                              ┌───────────┐
│  - etherparse│                              │    TUI    │
│  - aggregator│                              │ (Ratatui) │
│  - proc_attr │                              └───────────┘
└──────────────┘
```

- **Backend** - Runs as root, captures packets, aggregates traffic, exposes IPC via Unix socket
- **GUI** - Modern graphical interface with live charts, multi-tab views, and context menus (Iced)
- **TUI** - Terminal interface with real-time table and bandwidth chart (Ratatui)

## Requirements

- Linux (uses `/proc/net/tcp`, `/proc/<pid>/fd`, Unix domain sockets)
- Root privileges (for packet capture)
- Rust toolchain (nightly for backend/tui with 2024 edition, stable for GUI with 2021 edition)
- libpcap development files (`libpcap-dev` on Debian/Ubuntu)

## Building

```bash
cd net-monitor
cargo build
```

This builds all three components: `backend`, `gui`, and `tui` binaries.

## Usage

### 1. Run the Backend (requires sudo)

```bash
sudo cargo run --bin backend -- --iface eth0
```

### 2. Run the GUI or TUI

```bash
# GUI
cargo run --bin gui

# TUI
cargo run --bin tui
```

Both the GUI and TUI connect to the backend automatically via the Unix socket at `/tmp/net-monitor.sock`.

## Configuration

### Backend CLI Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--iface <interface>` | `eth0` | Network interface to capture on |
| `--filter-ip <IP>` | None | Filter by source or destination IP address |
| `--filter-port <port>` | None | Filter by source or destination port |
| `--alert-threshold <bytes/sec>` | None | Bandwidth threshold to trigger alerts |

### IPC Commands

Connect to the socket and send commands:

```bash
# Stream live snapshots (default when GUI/TUI connects)
echo "stream" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Export session history as JSON
echo "export json" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Export session history as CSV
echo "export csv" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Kill a process by PID
echo "kill 1234" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Set a rate limit for a PID (bytes/sec)
echo "limit set 1234 1000000" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Set a total data cap for a PID (bytes)
echo "limit total set 1234 500000000" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock

# Clear limits for a PID
echo "limit clear 1234" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

## License

MIT