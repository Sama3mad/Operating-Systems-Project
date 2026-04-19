use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;

use clap::Parser;

mod aggregator;
mod alert;
mod capture;
mod filter;
mod history;
mod ipc;
mod output;
mod parser;
mod proc_attr;

#[derive(Parser, Debug)]
#[command(name = "backend")]
struct Cli {
    #[arg(long, default_value = "eth0")]
    iface: String,
    #[arg(long)]
    filter_ip: Option<String>,
    #[arg(long)]
    filter_port: Option<u16>,
    #[arg(long)]
    alert_threshold: Option<u64>,
}

fn main() {
    let cli = Cli::parse();
    eprintln!("Starting capture on interface: {}", cli.iface);

    let capture = match capture::open_capture(&cli.iface) {
        Ok(c) => c,
        Err(err) => {
            if err.to_string().to_ascii_lowercase().contains("permission") {
                eprintln!("Error: packet capture requires root privileges. Run with sudo.");
                process::exit(1);
            }
            eprintln!("Error: failed to open capture interface {}: {}", cli.iface, err);
            process::exit(1);
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let history = Arc::new(Mutex::new(history::SessionHistory::new()));
    let ctrlc_running = Arc::clone(&running);
    if let Err(err) = ctrlc::set_handler(move || {
        ctrlc_running.store(false, Ordering::Relaxed);
    }) {
        eprintln!("Warning: failed to set Ctrl+C handler: {err}");
    }

    let (tx, rx) = mpsc::channel();
    let (ipc_tx, ipc_rx) = mpsc::channel();
    let ipc_thread = ipc::start_ipc_server(ipc_rx, Arc::clone(&history));
    let capture_running = Arc::clone(&running);
    let capture_thread = thread::spawn(move || {
        capture::run_capture_loop(capture, tx, capture_running);
    });

    let agg_result = aggregator::run_aggregator(
        rx,
        ipc_tx,
        &cli.iface,
        cli.filter_ip.clone(),
        cli.filter_port,
        cli.alert_threshold,
        Arc::clone(&history),
        Arc::clone(&running),
    );

    running.store(false, Ordering::Relaxed);
    if let Err(err) = capture_thread.join() {
        eprintln!("Warning: capture thread join error: {:?}", err);
    }
    if let Err(err) = ipc_thread.join() {
        eprintln!("Warning: IPC thread join error: {:?}", err);
    }
    if let Err(err) = agg_result {
        eprintln!("Warning: aggregator error: {err}");
    }

    eprintln!("Shutting down");
}
