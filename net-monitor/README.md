# net-monitor

A real-time network traffic monitor for Linux, built in Rust.  
It captures packets, attributes network connections to running processes, and displays live bandwidth usage through either a graphical interface (GUI) or terminal interface (TUI).

---

## Features

- **Packet Capture**
  - Capture TCP, UDP, SCTP, ICMPv4, and ICMPv6 traffic using `libpcap`

- **Process Attribution**
  - Map active network connections to Linux processes using `/proc`

- **Real-Time Monitoring**
  - Live connection table with continuously updated bandwidth statistics

- **Bandwidth Alerts**
  - Trigger alerts when traffic exceeds configured thresholds

- **Auto-Kill Support**
  - Automatically terminate processes that exceed configured bandwidth or data usage limits

- **Aggregate Views**
  - View traffic grouped by process or protocol

- **Session History**
  - Store and export historical traffic data as JSON or CSV

- **Dual Interfaces**
  - Choose between:
    - GUI built with **Iced**
    - TUI built with **Ratatui**

---

## Architecture

```text
┌────────────────────┐      Unix Socket IPC      ┌──────────────────┐
│      Backend       │◄─────────────────────────►│       GUI        │
│  (runs as root)    │    /tmp/net-monitor.sock  │      (Iced)      │
│                    │                            │                  │
│  - libpcap         │                            └──────────────────┘
│  - etherparse      │
│  - traffic engine  │                            ┌──────────────────┐
│  - proc analyzer   │◄─────────────────────────►│       TUI        │
│  - alert manager   │    /tmp/net-monitor.sock  │    (Ratatui)     │
└────────────────────┘                            └──────────────────┘
```

### Components

#### Backend
Responsible for:
- Packet capture
- Traffic aggregation
- Process attribution
- Alert handling
- IPC communication via Unix socket

> Requires root privileges for packet capture.

#### GUI
Modern graphical interface built with **Iced** featuring:
- Live charts
- Multi-tab views
- Context menus
- Process management controls

#### TUI
Terminal-based interface built with **Ratatui** featuring:
- Real-time traffic tables
- Bandwidth graphs
- Lightweight terminal monitoring

---

## Requirements

### Operating System
- Linux only

### Dependencies
- Rust toolchain
- `libpcap` development package

### Privileges
- Root access is required for packet capture

### Rust Versions
| Component | Rust Edition |
|-----------|--------------|
| Backend | 2024 (nightly) |
| TUI | 2024 (nightly) |
| GUI | 2021 (stable) |

---

## Installing Dependencies

### Debian / Ubuntu

Install `libpcap` development files:

```bash
sudo apt update
sudo apt install libpcap-dev
```

---

## Building

Clone the repository and build all binaries:

```bash
git clone <repository-url>
cd net-monitor

cargo build
```

This builds:
- `backend`
- `gui`
- `tui`

Binaries will be located in:

```text
target/debug/
```

---

## Usage

## 1. Start the Backend

The backend must run with root privileges.

```bash
sudo ./target/debug/backend --iface eth0
```

### Example

```bash
sudo ./target/debug/backend \
    --iface wlan0 \
    --alert-threshold 1000000
```

---

## 2. Launch the GUI or TUI

### GUI

```bash
cargo run --bin gui
```

### TUI

```bash
cargo run --bin tui
```

Both interfaces automatically connect to:

```text
/tmp/net-monitor.sock
```

---

## Backend CLI Arguments

| Argument | Default | Description |
|----------|----------|-------------|
| `--iface <interface>` | `eth0` | Network interface to monitor |
| `--filter-ip <ip>` | None | Filter traffic by IP address |
| `--filter-port <port>` | None | Filter traffic by port |
| `--alert-threshold <bytes/sec>` | None | Trigger alert above threshold |

---

## IPC Commands

You can communicate directly with the backend socket using tools like `socat`.

### Stream Live Data

```bash
echo "stream" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Export Session History (JSON)

```bash
echo "export json" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Export Session History (CSV)

```bash
echo "export csv" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Kill a Process

```bash
echo "kill 1234" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Set Bandwidth Rate Limit

```bash
echo "limit set 1234 1000000" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Set Total Data Limit

```bash
echo "limit total set 1234 500000000" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

### Clear Limits

```bash
echo "limit clear 1234" | sudo socat - UNIX-CONNECT:/tmp/net-monitor.sock
```

---

## Notes

- The backend must be running before launching the GUI or TUI.
- Some interfaces may use different naming conventions (`eth0`, `wlan0`, `enp0s3`, etc.).
- Running packet capture on high-traffic systems may require optimization or filtering.

---
